# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-10-22

### Changed

- **Breaking:** `DoubleBuf::init` now takes `&self` instead of `&mut self` and is no longer `const`.
  Together with the `const` `from_values`, this allows a `DoubleBuf` to live in a `static`.
- **Breaking:** The implementation no longer depends on `critical-section`, instead relying on atomics using `portable-atomic`, making every access and release lock-free. On targets without a native atomic compare-and-swap, the
  application must configure one of `portable-atomic`'s fallbacks, e.g. its `critical-section` feature.
- `Accessor` is now `Sync` (for `T: Send`), so a single accessor can be shared between threads.
- Commit `Cargo.lock` instead of ignoring it, so the CI is deterministic. Note that this has no effect on consumers of this library.

### Added

- `Accessor::try_read` and `Accessor::try_write`: non-panicking variants of `read` and `write`.
- `Debug` implementations for `DoubleBuf`, `Accessor`, `ReadGuard` and `WriteGuard`.
- Added [loom test cases](https://docs.rs/loom) (`tests/loom.rs`).
- Added Github CI
- Added `Justfile` mirroring the same tasks the CI does for local use.
- Added `CHANGELOG.md`
- Added `LICENCE`

### Fixed

- `DoubleBuf<T>` was `Sync` for every `T`, which made `Accessor<T>` `Send` even for `!Send` types such as
  `Rc`. The `Sync` impl now requires `T: Send`.

## [0.1.0] - 2025-08-15

Initial release.
