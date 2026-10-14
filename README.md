# Concurrent auto-swapping zero-copy double buffer

> Warning: Until this crate reaches version `1.0` it should be considered WIP-grade.

This crate provides a double buffer. Differences to existing implementations:

- Unlike channels, the access is by-reference and not by-value
- Thread-safe (no need for additional synchronization, with the double buffer providing interior-mutability by default)
- Zero-copy, no `Clone` required
- Auto-swapping: Whenever the writer finishes writing, the buffers are swapped on the next possible opportunity (when no readers or writers are active)
- Lock-free: all bookkeeping is a single atomic byte
- `no_std` compatible

## Usage:

```toml
doublebuf = "0.1"
```

The atomics are provided by [`portable-atomic`](https://docs.rs/portable-atomic). On targets with a native atomic
compare-and-swap nothing else is needed. On targets without a native compare-and-swap (e.g. `thumbv6m`, AVR), the application must configure one of [`portable-atomic`'s fallbacks](https://docs.rs/portable-atomic), e.g. its `critical-section` feature together with a `critical-section` implementation.

```toml
portable-atomic = { version = "1", features = ["critical-section"] }
critical-section = "1" # plus an implementation
```

See the [`portable-atomic` documentation](https://docs.rs/portable-atomic#optional-features) for the available options.

## Example

```rust
use doublebuf::*;
let mut db: DoubleBuf<u8> = DoubleBuf::new();
let (back, front) = db.init();
let mut writer = back.write();
let reader = front.read();
// Both are initialized with the default value
assert_eq!(*reader, 0);
assert_eq!(*writer, 0);
*writer = 5;
drop(writer);
drop(reader);
// the buffers are swapped now!
let reader = front.read();
assert_eq!(*reader, 5);
```
