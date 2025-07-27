//! This crate implements a thread-safe double buffer with automatic swapping.
//!
//! [`DoubleBuf`] is a data structure with two buffers (buffer 1 and buffer 2) and two [users](Accessor),
//! each having exclusive access to one of the buffers. The users can request [read](Accessor::read) or [write](Accessor::write) access
//! to the underlying buffer (we say that a user is _active_ while it keeps this access), such that the following holds:
//!
//! _Whenever both users are inactive with at least one having requested [write](Accessor::write) access since the last swap,
//!  the two buffers are atomically swapped._
//!
//! Note that while we use the term "buffer", any type `T` is allowed.
//!
//! # Usage
//! ```
//! use doublebuf::*;
//! let mut db: DoubleBuf<u8> = DoubleBuf::new();
//! let (mut back, mut front) = db.init();
//! let mut writer = back.write();
//! let reader = front.read();
//! // Both are initialized with the default value
//! assert_eq!(*reader, 0);
//! assert_eq!(*writer, 0);
//!
//! *writer = 5;
//! drop(writer);
//! drop(reader);
//! // the buffers are swapped now!
//! let reader = front.read();
//! assert_eq!(*reader, 5);
//! ```
//!
//! # Notes
//! - `no_std` usage is explicitly allowed, but a [`critical_section`] implementation must be provided.
//! - In `std` contexts, you may enable the `std` feature of [`critical_section`].
//! - While double buffers conventionally have a "front" and a "back" buffer (the back buffer being used by the producer and
//!   the front buffer by the consumer), our implementation is completely symmetric.
//! - The buffers are not swapped if there has been no [write](Accessor::write) access since the last swap.
//! - The semantics allow for a live-lock: If the two accessors are continuously overlapping their accesses (that is, if there is no point in time when neither is inactive),
//!   the buffers will never get swapped.

#![no_std]

use core::{
    cell::{Cell, UnsafeCell},
    ops::{Deref, DerefMut},
};

use critical_section::Mutex;

#[derive(Clone, Copy, Debug)]
struct StateInner {
    is_dirty: bool,
    counter: u8,
    swapped: bool,
}

// use critical_section::Mutex instead of embassy's blocking_mutex::CriticalSectionMutex
// They are pretty similar, but we can use the former in std environments too, making the
// double buffer better testable
struct State(critical_section::Mutex<Cell<StateInner>>);

/// A thread-safe double buffer with automatic swapping. See [module-level documentation](`doublebuf`) for general notes.
/// The usual workflow is as follows:
/// - Construct using [`new`](`DoubleBuf::new`) or [`new_with`](`DoubleBuf::new_with`).
/// - Call [`init`](`DoubleBuf::init`) once to obtain the [`Accessors`](`Accessor`).
/// - On the `Accessor`s, call [`read`](`Accessor::read`) and [`write`](`Accessor::write`) as required. It is advisable
///   to hold the guards returned by those methods for as short as possible.
///
/// # Examples
/// ```
/// let db: DoubleBuf<u8> = DoubleBuf::new();
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
pub struct Accessor<'a, T> {
    inner: &'a DoubleBuf<T>,
    access_buf1_by_def: bool,
}

pub struct WriteGuard<'a, T> {
    state: &'a State,
    inner: &'a mut T,
}

pub struct ReadGuard<'a, T> {
    state: &'a State,
    inner: &'a T,
}

impl<T> Deref for ReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<T> Deref for WriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<T> DerefMut for WriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
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

    /// Constructs a new double buffer with the two buffers containing the values produced by the predicates.
    /// Note that by default, `buf1` will be associated with the user `init().0` and `buf2` with `init().1` (until they are swapped).
    pub fn new_with(buf1: impl FnOnce() -> T, buf2: impl FnOnce() -> T) -> DoubleBuf<T> {
        DoubleBuf {
            initialized: false,
            state: State(Mutex::new(Cell::new(StateInner {
                is_dirty: false,
                counter: 0,
                swapped: false,
            }))),
            buf1: UnsafeCell::new(buf1()),
            buf2: UnsafeCell::new(buf2()),
        }
    }

    /// Initializes the double buffer, returning the accessors.
    /// This method must not be called more than once.
    ///
    /// # Panics
    ///
    /// Will panic if called more than once.
    pub fn init(&mut self) -> (Accessor<'_, T>, Accessor<'_, T>) {
        if self.initialized {
            panic!("DoubleBuf::init should be only called once");
        }
        self.initialized = true;
        (
            Accessor {
                access_buf1_by_def: true,
                inner: self,
            },
            Accessor {
                access_buf1_by_def: false,
                inner: self,
            },
        )
    }
}

unsafe impl<T> Sync for DoubleBuf<T> {}

impl<T: Default> Default for DoubleBuf<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, T> Accessor<'a, T> {
    fn prepare_access(&mut self, is_write: bool) -> &'a mut T {
        let swapped = critical_section::with(|cs| {
            let s = self.inner.state.0.borrow(cs);
            let mut state = s.get();
            if state.counter >= 2 {
                unreachable!("max one writer")
            }
            state.counter += 1;
            if is_write {
                state.is_dirty = true;
            }
            s.set(state);
            state.swapped
        });
        if self.access_buf1_by_def ^ swapped {
            unsafe { &mut *self.inner.buf1.get() }
        } else {
            unsafe { &mut *self.inner.buf2.get() }
        }
    }

    /// Accesses the current associated buffer for writing. The accessor is _active_ as long as the returned guard exists.
    ///
    /// Note that it is strongly advised to actually
    /// write to the buffer at least once. This is because the double buffer is marked dirty (ready to be swapped)
    /// as soon as this method is called. This means that if you call this method but do not write a value, upon release
    /// of the [`WriteGuard`], the buffers will be swapped, and the reader will get an old value.
    pub fn write(&mut self) -> WriteGuard<'a, T> {
        let buf = self.prepare_access(true);
        WriteGuard {
            state: &self.inner.state,
            inner: buf,
        }
    }

    /// Accesses the current associated buffer for reading. The accessor is _active_ as long as the returned guard exists.
    pub fn read(&mut self) -> ReadGuard<'a, T> {
        let buf = self.prepare_access(false);
        ReadGuard {
            state: &self.inner.state,
            inner: buf,
        }
    }
}

fn drop_guard(state: &State) {
    critical_section::with(|cs| {
        let s = state.0.borrow(cs);
        let mut state = s.get();
        if state.counter == 0 {
            unreachable!("guard drop implies counter >= 1")
        }
        state.counter -= 1;
        if state.counter == 0 && state.is_dirty {
            state.is_dirty = false;
            state.swapped = !state.swapped;
        }
        s.set(state);
    });
}

impl<T> Drop for WriteGuard<'_, T> {
    fn drop(&mut self) {
        drop_guard(self.state);
    }
}

impl<T> Drop for ReadGuard<'_, T> {
    fn drop(&mut self) {
        drop_guard(self.state);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    use std::{
        sync::Barrier,
        thread::{self},
    };

    static_assertions::assert_impl_one!(Accessor<u8>: Send);

    #[test]
    fn test_swap() {
        let mut db: DoubleBuf<u8> = DoubleBuf::new();
        let (mut back, mut front) = db.init();

        let barrier = Barrier::new(2);
        thread::scope(|s| {
            let jh = s.spawn(|| {
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
        let (mut back, mut front) = db.init();

        thread::scope(|s| {
            let jh = s.spawn(|| {
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
}
