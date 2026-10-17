//! This crate implements a zero-copy thread-safe double buffer with automatic swapping.
//!
//! [`DoubleBuf`] is a data structure with two buffers (buffer 1 and buffer 2) and two [users](Accessor),
//! each having exclusive by-reference access to one of the buffers. The users can request [read](Accessor::read) or [write](Accessor::write) access
//! to the underlying buffer (we say that a user is _active_ while it keeps this access), such that the following holds:
//!
//! _Whenever both users are inactive with at least one having requested [write](Accessor::write) access since the last swap,
//!  the two buffers are atomically swapped._
//!
//! Note that while we use the term "buffer", any type `T` is allowed.
//!
//! # Example
//! ```
//! use doublebuf::*;
//! let mut db: DoubleBuf<u8> = DoubleBuf::new();
//! let (back, front) = db.init();
//! let mut writer = back.write();
//! let reader = front.read();
//! // Both are initialized with the default value
//! assert_eq!(*reader, 0);
//! assert_eq!(*writer, 0);
//! *writer = 5;
//! drop(writer);
//! drop(reader);
//! // the buffers are swapped now!
//! let reader = front.read();
//! assert_eq!(*reader, 5);
//! ```
//!
//! # Notes
//! - `no_std` usage is explicitly allowed.
//! - The implementation is lock-free on targets with atomic compare-and-swap on single bytes.
//!   On targets without a native compare-and-swap (e.g. `thumbv6m`, AVR), the application must configure one of
//!   [`portable-atomic`'s fallbacks](https://docs.rs/portable-atomic#optional-features), e.g. its `critical-section`
//!   feature together with a `critical-section` implementation.
//! - While double buffers conventionally have a "front" and a "back" buffer (the back buffer being used by the producer and
//!   the front buffer by the consumer), our implementation is completely symmetric.
//! - The buffers are not swapped if there has been no [write](Accessor::write) access since the last swap.
//! - The semantics allow for a live-lock: If the two accessors are continuously overlapping their accesses (that is, if there is no point in time when neither is inactive),
//!   the buffers will never get swapped.

#![no_std]

use core::{
    cell::UnsafeCell,
    fmt,
    ops::{Deref, DerefMut},
};

#[cfg(loom)]
use loom::sync::atomic::{AtomicU8, Ordering};
#[cfg(not(loom))]
use portable_atomic::{AtomicU8, Ordering};

/// Identifies one of the two accessors by the buffer it is associated with by default (i.e. upon initialization, while not swapped).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Buf1 = 1,
    Buf2 = 2,
}

impl Side {
    /// The `ACTIVE` bit of this side in the [`State`].
    const fn active_bit(self) -> u8 {
        match self {
            Side::Buf1 => StateInner::ACTIVE_1,
            Side::Buf2 => StateInner::ACTIVE_2,
        }
    }

    /// 1 or 2, for diagnostics.
    const fn number(self) -> u8 {
        self as u8
    }
}

/// A plain copy of the [`State`] bits, for inspecting it and for computing transitions.
#[derive(Clone, Copy, PartialEq, Eq)]
struct StateInner(u8);

/// We pack the state in a single byte as follows:
/// ****SDAB (in big endian)
/// - S is the swapped bit
/// - D is the dirty bit
/// - A and B are the active bits for the two accessors
impl StateInner {
    /// Set while the accessor associated with buffer 1 by default holds a guard.
    const ACTIVE_1: u8 = 1 << 0;
    /// Set while the accessor associated with buffer 2 by default holds a guard.
    const ACTIVE_2: u8 = 1 << 1;
    const ACTIVE_ANY: u8 = Self::ACTIVE_1 | Self::ACTIVE_2;
    /// Set when there has been a write access since the last swap.
    const DIRTY: u8 = 1 << 2;
    /// Set when the buffers are currently swapped relative to their default association.
    const SWAPPED: u8 = 1 << 3;

    /// Whether the accessor of the given side currently holds a guard.
    const fn is_active(self, side: Side) -> bool {
        self.0 & side.active_bit() != 0
    }

    /// Whether any accessor currently holds a guard.
    const fn is_any_active(self) -> bool {
        self.0 & Self::ACTIVE_ANY != 0
    }

    /// Whether there has been a write access since the last swap.
    const fn is_dirty(self) -> bool {
        self.0 & Self::DIRTY != 0
    }

    /// Whether the buffers are currently swapped relative to their default association.
    const fn is_swapped(self) -> bool {
        self.0 & Self::SWAPPED != 0
    }

    // The transitions below are pure; `State` applies them atomically.

    const fn with_active(self, side: Side) -> Self {
        Self(self.0 | side.active_bit())
    }

    const fn with_inactive(self, side: Side) -> Self {
        Self(self.0 & !side.active_bit())
    }

    const fn with_dirty(self) -> Self {
        Self(self.0 | Self::DIRTY)
    }

    /// Swaps the buffers, which also clears the dirty flag.
    const fn with_swapped(self) -> Self {
        Self((self.0 & !Self::DIRTY) ^ Self::SWAPPED)
    }
}

/// The complete state of the double buffer, packed into one byte so that every transition is a single
/// atomic read-modify-write (see the safety argument below).
struct State(AtomicU8);

impl State {
    #[cfg(not(loom))]
    const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    // loom's atomics cannot be constructed in `const` contexts
    #[cfg(loom)]
    fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    /// A snapshot for diagnostics. It may be stale by the time it is inspected.
    fn snapshot(&self) -> StateInner {
        StateInner(self.0.load(Ordering::Relaxed))
    }

    /// Atomically marks the given side as active (and sets the dirty flag in case of write access) and
    /// returns whether the buffers are swapped, as observed in that same transition.
    /// Returns `None` without changing anything if the side is already active.
    fn try_acquire(&self, side: Side, is_write: bool) -> Option<bool> {
        // The failure ordering can be relaxed: on failure we either retry (and the retried compare-and-swap
        // carries the ordering) or return without touching the buffer.
        let prev = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |s: u8| {
                let s = StateInner(s);
                if s.is_active(side) {
                    return None;
                }
                let s = s.with_active(side);
                Some(if is_write { s.with_dirty() } else { s }.0)
            })
            .ok()?;
        // SWAPPED is not changed by the transition above, so the previous value is the current one.
        Some(StateInner(prev).is_swapped())
    }

    /// Atomically marks the given side as inactive. If the other side is inactive too and there has been a
    /// write access since the last swap, the buffers are swapped in the same transition.
    fn release(&self, side: Side) {
        // The closure never returns `None`, so this cannot fail.
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |s| {
                let s = StateInner(s);
                debug_assert!(s.is_active(side), "release implies the accessor is active");
                let s = s.with_inactive(side);
                Some(
                    if !s.is_any_active() && s.is_dirty() {
                        s.with_swapped()
                    } else {
                        s
                    }
                    .0,
                )
            });
    }
}

/// A thread-safe double buffer with automatic swapping. See [module-level documentation](`crate`) for general notes.
/// The usual workflow is as follows:
/// - Construct using [`new`](`DoubleBuf::new`), [`new_with`](`DoubleBuf::new_with`) or [`from_values`](`DoubleBuf::from_values`).
/// - Call [`init`](`DoubleBuf::init`) once to obtain the [`Accessors`](`Accessor`).
/// - On the `Accessor`s, call [`read`](`Accessor::read`) and [`write`](`Accessor::write`) as required. It is advisable
///   to hold the guards returned by those methods for as short as possible.
///
/// # Examples
/// ```
/// # use doublebuf::*;
/// let mut db: DoubleBuf<u8> = DoubleBuf::new();
/// let (back, front) = db.init();
/// // use read() and write() as required
/// ```
pub struct DoubleBuf<T> {
    initialized: bool,
    state: State,
    buf1: UnsafeCell<T>,
    buf2: UnsafeCell<T>,
}

/// A user of the double buffer. There can only ever be exactly two Accessors associated with one initialized double buffer.
/// The accessor is exclusively associated to one of the two buffers, and can obtain access using the [`Accessor::read`] or [`Accessor::write`]
/// methods.
///
/// ## Thread Safety
/// `Accessor` is `Send` (and `Sync`) for `T: Send`, so you can send it to another thread (which is one of the primary driving use cases for a double buffer).
/// Only one guard per accessor can exist at a time; this is checked at runtime.
/// Sharing a single `Accessor` between threads is possible. In that case, the first `read` / `write` call wins.
pub struct Accessor<'db, T> {
    inner: &'db DoubleBuf<T>,
    side: Side,
}
pub struct WriteGuard<'ac, 'db, T> {
    // Conceptually, this should be a &mut reference, but we want to enable this check to be done during runtime: `Accessor::try_acquire` guarantees that there is at most one guard per accessor, and therefore per buffer.
    accessor: &'ac Accessor<'db, T>,
    inner: *mut T,
}

pub struct ReadGuard<'ac, 'db, T> {
    // Conceptually, this should be a &mut reference, but we want to enable this check to be done during runtime: `Accessor::try_acquire` guarantees that there is at most one guard per accessor, and therefore per buffer.
    accessor: &'ac Accessor<'db, T>,
    inner: *const T,
}

// Safety: We need to show that the (1) reference points to valid data and that (2) Rust's aliasing rules are upheld.
// (1) The pointer was created from valid data inside an `UnsafeCell` inside
// `DoubleBuf`. The data has not been moved away or invalidated in any way because the guards have a reference to the Accessor which in turn has a reference to the `DoubleBuf`.
// (2) This consists of two parts:
// - At most one guard per accessor: `try_acquire` sets the accessor's ACTIVE bit in a compare-and-swap that
//   fails if the bit is already set, and the bit is only cleared when the guard is dropped.
// - No two accessors are associated with the same buffer: the two accessors are initially associated with
//   different buffers, and the SWAPPED bit only flips in the same atomic transition that clears the last
//   ACTIVE bit. Every acquire reads SWAPPED in the same atomic transition that sets its ACTIVE bit. Hence all
//   guards that are alive at the same time were created under the same value of SWAPPED and therefore point to
//   different buffers.
// - Taken together, these two imply that there is at most one guard (regardless of type) per buffer, which is a stronger
//   requirement than Rust's aliasing rules.
// Memory ordering: every transition is an AcqRel read-modify-write on the same atomic. Each one therefore
// synchronizes-with the transition that produced the value it read, so a release (which follows all accesses to
// the buffer through the guard) happens-before every later acquire, including those that find the buffers swapped.

impl<T> Deref for ReadGuard<'_, '_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner }
    }
}

impl<T> Deref for WriteGuard<'_, '_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner }
    }
}

impl<T> DerefMut for WriteGuard<'_, '_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.inner }
    }
}

impl<T> DoubleBuf<T> {
    /// Constructs a new double buffer. The default values are obtained via [`Default::default`].
    pub fn new() -> DoubleBuf<T>
    where
        T: Default,
    {
        Self::new_with(T::default, T::default)
    }

    /// Constructs a new double buffer from the two provided values. Useful in `const` contexts.
    /// Note that by default, `buf1` will be associated with the user `init().0` and `buf2` with `init().1` (until they are swapped).
    #[cfg(not(loom))]
    pub const fn from_values(buf1: T, buf2: T) -> DoubleBuf<T> {
        DoubleBuf {
            initialized: false,
            state: State::new(),
            buf1: UnsafeCell::new(buf1),
            buf2: UnsafeCell::new(buf2),
        }
    }

    // loom's atomics cannot be constructed in `const` contexts
    #[cfg(loom)]
    pub fn from_values(buf1: T, buf2: T) -> DoubleBuf<T> {
        DoubleBuf {
            initialized: false,
            state: State::new(),
            buf1: UnsafeCell::new(buf1),
            buf2: UnsafeCell::new(buf2),
        }
    }

    /// Constructs a new double buffer with the two buffers containing the values produced by the predicates.
    /// Note that by default, `buf1` will be associated with the user `init().0` and `buf2` with `init().1` (until they are swapped).
    pub fn new_with(buf1: impl FnOnce() -> T, buf2: impl FnOnce() -> T) -> DoubleBuf<T> {
        Self::from_values(buf1(), buf2())
    }

    /// Initializes the double buffer, returning the accessors.
    /// This method must not be called more than once.
    ///
    /// # Panics
    ///
    /// Will panic if called more than once.
    pub const fn init(&mut self) -> (Accessor<'_, T>, Accessor<'_, T>) {
        if self.initialized {
            panic!("DoubleBuf::init should be only called once");
        }
        self.initialized = true;
        (
            Accessor {
                side: Side::Buf1,
                inner: self,
            },
            Accessor {
                side: Side::Buf2,
                inner: self,
            },
        )
    }
}

// Safety: all access to the two buffers go through guards of the accessors.
// We guarantee that, for any of the two buffers,
// at most one guard can be dereferenced into a reference to it at a time (see above).
// Note that this implies that `Accessor: Send`, which is what we want!

/// [`DoubleBuf`] provides synchronization for its data.
/// The [`Send`] bound on `T` is needed for a similar reason that it is needed on the stdlib
/// Mutex; see the docs there for an explanation (in short, it would otherwise allow us to send
/// `!Send` types across boundaries).
unsafe impl<T: Send> Sync for DoubleBuf<T> {}

impl<T: Default> Default for DoubleBuf<T> {
    fn default() -> Self {
        Self::new()
    }
}

// The contents are not printed, could be a data race
impl<T> fmt::Debug for DoubleBuf<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.snapshot();
        f.debug_struct("DoubleBuf")
            .field("initialized", &self.initialized)
            .field("active_1", &state.is_active(Side::Buf1))
            .field("active_2", &state.is_active(Side::Buf2))
            .field("is_dirty", &state.is_dirty())
            .field("swapped", &state.is_swapped())
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for Accessor<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state.snapshot();
        f.debug_struct("Accessor")
            .field("default_buffer", &self.side.number())
            .field("is_active", &state.is_active(self.side))
            .finish_non_exhaustive()
    }
}

// Like `MutexGuard`, the guards format as the value they point to.
impl<T: fmt::Debug> fmt::Debug for ReadGuard<'_, '_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: fmt::Debug> fmt::Debug for WriteGuard<'_, '_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<'db, T> Accessor<'db, T> {
    /// Marks this accessor as active and returns the buffer it is currently associated with.
    ///
    /// Panics (without changing anything) if the accessor is already active.
    fn acquire(&self, is_write: bool) -> *mut T {
        let swapped = self.inner.state.try_acquire(self.side, is_write).expect(
            "Accessor is already active. The read / write methods must not be called reentrantly",
        );
        if (self.side == Side::Buf1) ^ swapped {
            self.inner.buf1.get()
        } else {
            self.inner.buf2.get()
        }
    }

    /// Accesses the current associated buffer for writing. The accessor is _active_ as long as the returned guard exists.
    ///
    /// Note that it is strongly advised to actually
    /// write to the buffer at least once. This is because the double buffer is marked dirty (ready to be swapped)
    /// as soon as this method is called. This means that if you call this method but do not write a value, upon release
    /// of the [`WriteGuard`], the buffers will be swapped, and the reader will get an old value.
    ///
    /// ## Panics:
    /// If the accessor is already active. There can only be one guard; you must drop the old one before calling `read` or `write` again.
    pub fn write<'ac>(&'ac self) -> WriteGuard<'ac, 'db, T> {
        let inner = self.acquire(true);
        WriteGuard {
            accessor: self,
            inner,
        }
    }

    /// Accesses the current associated buffer for reading. The accessor is _active_ as long as the returned guard exists.
    ///
    /// ## Panics:
    /// If the accessor is already active. There can only be one guard; you must drop the old one before calling `read` or `write` again.
    pub fn read<'ac>(&'ac self) -> ReadGuard<'ac, 'db, T> {
        let inner = self.acquire(false);
        ReadGuard {
            accessor: self,
            inner,
        }
    }
}

impl<T> Drop for WriteGuard<'_, '_, T> {
    fn drop(&mut self) {
        self.accessor.inner.state.release(self.accessor.side);
    }
}

impl<T> Drop for ReadGuard<'_, '_, T> {
    fn drop(&mut self) {
        self.accessor.inner.state.release(self.accessor.side);
    }
}

// Run the README code example as a doctest so that it cannot drift from the crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_doctest {}

// The unit tests use real threads and `const` construction, which do not work under loom.
#[cfg(all(test, not(loom)))]
mod tests {
    extern crate std;

    use super::*;

    use std::{
        sync::Barrier,
        thread::{self},
    };

    // `Accessor` can be sent to and shared with other threads (the atomic active bit arbitrates)
    static_assertions::assert_impl_all!(Accessor<u8>: Send, Sync);
    // ... but not for `!Send` types like `Rc`, which must not cross thread boundaries (it's unclear why anyone would even use an Rc here)
    static_assertions::assert_not_impl_any!(Accessor<std::rc::Rc<u8>>: Send, Sync);

    // The guards hold raw pointers into the buffer and are deliberately kept on the thread that created them
    static_assertions::assert_not_impl_any!(ReadGuard<'static, 'static, u8>: Send, Sync);
    static_assertions::assert_not_impl_any!(WriteGuard<'static, 'static, u8>: Send, Sync);

    // `DoubleBuf` and `Accessor` are `Debug` regardless of `T` (they never print the buffers),
    // while the guards print the value and therefore need `T: Debug`.
    struct NoDebug;
    static_assertions::assert_impl_all!(DoubleBuf<NoDebug>: fmt::Debug);
    static_assertions::assert_impl_all!(Accessor<NoDebug>: fmt::Debug);
    static_assertions::assert_not_impl_any!(ReadGuard<'static, 'static, NoDebug>: fmt::Debug);
    static_assertions::assert_not_impl_any!(WriteGuard<'static, 'static, NoDebug>: fmt::Debug);
    static_assertions::assert_impl_all!(ReadGuard<'static, 'static, u8>: fmt::Debug);
    static_assertions::assert_impl_all!(WriteGuard<'static, 'static, u8>: fmt::Debug);

    #[test]
    fn test_both_write() {
        let mut db: DoubleBuf<u8> = DoubleBuf::new();
        let (a, b) = db.init();

        // a writes buf1 and triggers a swap on drop; b now sees it
        *a.write() = 1;
        assert_eq!(*b.read(), 1);

        // b writes it back; another swap, a sees it, b is left with the never-written buffer
        *b.write() = 2;
        assert_eq!(*a.read(), 2);
        assert_eq!(*b.read(), 0);

        // both hold write guards at the same time: no swap until the last one is dropped
        let mut wa = a.write();
        let mut wb = b.write();
        *wa = 10;
        *wb = 20;
        drop(wa);
        assert_eq!(*wb, 20);
        drop(wb);
        assert_eq!(*a.read(), 20);
        assert_eq!(*b.read(), 10);
    }

    #[test]
    #[should_panic]
    fn test_double_init() {
        let mut db: DoubleBuf<u8> = DoubleBuf::new();
        let _ = db.init();
        // the accessors are gone, so the borrow checker allows this; the runtime check must not
        let _ = db.init();
    }

    #[test]
    fn test_const_context() {
        // `DoubleBuf<T: Send>` is `Sync`, so it can live in a `static` ...
        static STATIC_DB: DoubleBuf<u8> = DoubleBuf::from_values(1, 2);
        // ... and `from_values` / `init` are usable in `const` contexts
        const _: () = {
            let mut db = DoubleBuf::from_values(1u8, 2u8);
            let (_a, _b) = db.init();
        };

        let _ = &STATIC_DB;
        let mut db = DoubleBuf::from_values(1u8, 2u8);
        let (a, b) = db.init();
        // `buf1` is associated with `init().0` and `buf2` with `init().1`, as documented
        assert_eq!(*a.read(), 1);
        assert_eq!(*b.read(), 2);
    }

    #[test]
    fn test_swap() {
        let mut db: DoubleBuf<u8> = DoubleBuf::new();
        let (back, front) = db.init();

        let barrier = Barrier::new(2);
        let barrier_ref = &barrier;
        thread::scope(|s| {
            let jh = s.spawn(move || {
                let barrier = barrier_ref;
                let reader = front.read();
                // barrier to make sure both are (concurrently!) accessing their respective parts of the double buffer
                barrier.wait();

                // default value
                assert_eq!(*reader, 0);

                barrier.wait();

                // still not changed...
                assert_eq!(*reader, 0);

                drop(reader);

                let reader = front.read();
                // still not changed...
                assert_eq!(*reader, 0);
                drop(reader);

                barrier.wait();
                // the other thread drops now the writer
                barrier.wait();
                // now both are guaranteed to be gone, so the buffers are swapped

                let reader = front.read();
                assert_eq!(*reader, 17);

                barrier.wait();
                drop(reader);
            });

            let mut writer = back.write();
            barrier.wait();
            assert_eq!(*writer, 0);
            *writer = 17;
            barrier.wait();
            barrier.wait();
            // NOW (strictly speaking after the barrier at the latest) the buffers swap
            drop(writer);
            barrier.wait();

            // specifically access as read as to not trigger a swap
            let writer_read_mode = back.read();
            assert_eq!(*writer_read_mode, 0);
            drop(writer_read_mode);
            // this time let the writer finish first
            barrier.wait();

            jh.join().unwrap();
        });
    }

    // this test is mainly useful for miri to simulate different concurrency schedules
    #[test]
    fn test_swap_nosync() {
        let mut db: DoubleBuf<u8> = DoubleBuf::new();
        let (back, front) = db.init();

        thread::scope(|s| {
            let jh = s.spawn(move || {
                let reader = front.read();
                assert!(*reader == 0 || *reader == 17 || *reader == 18);
                drop(reader);
                let reader = front.read();
                assert!(*reader == 0 || *reader == 17 || *reader == 18);
                drop(reader);

                let reader = front.read();
                assert!(*reader == 0 || *reader == 17 || *reader == 18);
                drop(reader);
            });

            let mut writer = back.write();
            *writer = 17;
            drop(writer);
            let reader = back.read();
            assert!(*reader == 0 || *reader == 17 || *reader == 18);
            drop(reader);

            jh.join().unwrap();
        });
    }

    #[cfg(miri)]
    fn test_miri_longrun(second_should_write: bool) {
        let mut db: DoubleBuf<u8> = DoubleBuf::new();
        let (back, front) = db.init();
        let writer = move || {
            for i in 0..64 {
                let mut w = back.write();
                *w = i;
            }
        };
        let reader = move || {
            for i in 0..64 {
                if second_should_write {
                    let mut w = front.write();
                    *w = i;
                } else {
                    let r = front.read();
                    std::hint::black_box(*r);
                }
            }
        };
        thread::scope(|s| {
            let join1 = s.spawn(writer);
            let join2 = s.spawn(reader);
            join1.join().unwrap();
            join2.join().unwrap();
        });
    }

    #[cfg(miri)]
    #[test]
    fn test_miri_longrun_second_reads() {
        test_miri_longrun(false);
    }

    #[cfg(miri)]
    #[test]
    fn test_miri_longrun_second_writes() {
        test_miri_longrun(true);
    }

    #[test]
    #[should_panic]
    fn test_reentrant_read() {
        let mut db: DoubleBuf<u8> = DoubleBuf::new();
        let (back, _) = db.init();
        let g1 = back.read();
        let g2 = back.read();
        // both guards active at the same time, should be impossible
        drop(g1);
        drop(g2);
    }
}
