# Concurrent auto-swapping zero-copy double buffer

This crate provides a double buffer. Differences to existing implementations:

- Unlike channels, the access is by-reference and not by-value
- Thread-safe (no need for additional synchronization, with the double buffer providing interior-mutability by default)
- Zero-copy, no `Clone` required; any type `T` can be buffered
- Auto-swapping: Whenever a write access finishes, the buffers are swapped on the next possible opportunity (when neither side is active)
- Symmetric: both sides can read and write, there is no fixed producer / consumer role
- Lock-free: all bookkeeping is a single atomic byte, on targets with an atomic compare-and-swap
- `no_std` compatible

See the [documentation](https://docs.rs/doublebuf) for the exact swap semantics and caveats.

## Usage

```toml
doublebuf = "0.2"
```

The atomics are provided by [`portable-atomic`](https://docs.rs/portable-atomic). On targets with a native atomic
compare-and-swap nothing else is needed. On targets without one (e.g. `thumbv6m`, AVR), the application must configure
one of [`portable-atomic`'s fallbacks](https://docs.rs/portable-atomic#optional-features), e.g. its `critical-section`
feature together with a `critical-section` implementation:

```toml
portable-atomic = { version = "1", features = ["critical-section"] }
critical-section = "1" # plus an implementation
```

## Example

```rust
use doublebuf::*;
let db: DoubleBuf<u8> = DoubleBuf::new();
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
