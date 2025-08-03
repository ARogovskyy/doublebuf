# Concurrent auto-swapping double buffer

> Warning: Until this crate reaches version `1.0` it should be considered WIP-grade.

This crate provides a double buffer. Differences to existing implementations:
- Unlike channels, the access is by-reference and not by-value
- Thread-safe (no need for additional synchronization, with the double buffer providing interior-mutability by default)
- Auto-swapping: Whenever the writer finishes writing, the buffers are swapped on the next possible opportunity (when no readers or writers are active)
- `no_std` compatible (needs a `critical_section` implementation)

Usage:

```rust
use doublebuf::*;
let mut db: DoubleBuf<u8> = DoubleBuf::new();
let (mut back, mut front) = db.init();
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