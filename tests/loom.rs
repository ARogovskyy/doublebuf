//! Exhaustive model checking of the atomic swap protocol with [loom](https://docs.rs/loom).
//!
//! Loom explores every interleaving of the atomic operations of the threads below. The buffers hold a
//! `loom::cell::UnsafeCell`, which makes loom also track every access to the buffer contents and fail the
//! model on data races on them.
//!
//! Run with:
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom
//! ```
#![cfg(loom)]

use doublebuf::{Accessor, DoubleBuf};
use loom::cell::UnsafeCell;
use loom::thread;

type Buf = UnsafeCell<u8>;

fn new_db() -> &'static DoubleBuf<Buf> {
    // Loom has no scoped threads and the accessors borrow the double buffer, so leak it.
    // This can be fixed when there is progress e.g. in https://github.com/tokio-rs/loom/pull/312
    Box::leak(Box::new(DoubleBuf::new_with(
        || UnsafeCell::new(0),
        || UnsafeCell::new(0),
    )))
}

fn write(ac: &Accessor<'_, Buf>, value: u8) {
    let guard = ac.write();
    guard.with_mut(|p| unsafe { *p = value });
}

fn read(ac: &Accessor<'_, Buf>) -> u8 {
    let guard = ac.read();
    guard.with(|p| unsafe { *p })
}

/// One side writes, the other reads: the reader must only ever observe values the writer has fully
/// published (never a torn or in-progress value, which loom would flag as a concurrent access).
#[test]
fn loom_writer_reader() {
    loom::model(|| {
        let db = new_db();
        let (a, b) = db.init();
        let ta = thread::spawn(move || {
            write(&a, 1);
            write(&a, 2);
        });
        let tb = thread::spawn(move || {
            let v1 = read(&b);
            let v2 = read(&b);
            assert!(matches!(v1, 0..=2));
            assert!(matches!(v2, 0..=2));
            // Reads never go backwards: an older buffer can only come back through another swap,
            // and nothing marks the state dirty after the writer's last write.
            assert!(v1 <= v2);
            b
        });
        ta.join().unwrap();
        let b = tb.join().unwrap();
        // A finished write becomes visible once both sides are idle: the writer's last release set the
        // dirty flag, and whichever side released last with nobody active performed the swap.
        assert_eq!(read(&b), 2);
    });
}

/// Both sides write and read: exercises swaps triggered from either side.
#[test]
fn loom_both_write() {
    loom::model(|| {
        let db = new_db();
        let (a, b) = db.init();
        let ta = thread::spawn(move || {
            write(&a, 1);
            let _ = read(&a);
        });
        let tb = thread::spawn(move || {
            write(&b, 2);
            let _ = read(&b);
        });
        ta.join().unwrap();
        tb.join().unwrap();
    });
}

/// `Accessor: Sync`: both threads share the *same* accessor via `try_write`. At most one of them may hold a
/// guard at a time, and a failed attempt must leave the state untouched (in particular not dirty).
#[test]
fn loom_shared_accessor() {
    loom::model(|| {
        let db = new_db();
        let (a, b) = db.init();
        let a: &'static Accessor<'static, Buf> = Box::leak(Box::new(a));
        let t1 = thread::spawn(move || {
            if let Some(g) = a.try_write() {
                g.with_mut(|p| unsafe { *p = 1 });
            }
        });
        let t2 = thread::spawn(move || {
            if let Some(g) = a.try_write() {
                g.with_mut(|p| unsafe { *p = 2 });
            }
            let _ = read(&b);
            b
        });
        t1.join().unwrap();
        let b = t2.join().unwrap();
        // Both threads are done, so `a` is free again.
        assert!(a.try_read().is_some());
        // A `try_write` can only fail while the other thread holds the guard, so at least one of them
        // succeeded and marked the state dirty; the last release (by whichever side) swapped, and `b` now
        // owns the buffer that was written.
        assert!(matches!(read(&b), 1 | 2));
    });
}
