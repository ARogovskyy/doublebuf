# Mimic CI
export RUSTFLAGS := "-D warnings"
export RUSTDOCFLAGS := "-D warnings"

# Read MSRV from Cargo.toml
msrv := `sed -nE 's/^rust-version *= *"([^"]+)".*/\1/p' Cargo.toml`

# List the recipes
default:
    @just --list --unsorted

# Run everything that the CI also runs
ci: test lint semver msrv minimal-versions miri loom

# Unit tests and doctests
test:
    cargo test

# rustfmt, clippy and rustdoc, with warnings denied
lint:
    cargo fmt --check
    cargo clippy --all-targets
    cargo doc --no-deps

# Public API compatibility against the latest release on crates.io (needs `cargo install cargo-semver-checks`)
semver:
    cargo semver-checks

# Build everything with the minimum supported Rust version (needs `rustup toolchain install {{msrv}}`)
msrv:
    cargo +{{msrv}} check --all-targets

# Test against the oldest dependency versions allowed by Cargo.toml (needs nightly); Cargo.lock is briefly changed and restored afterwards
minimal-versions:
    #!/usr/bin/env bash
    set -euo pipefail
    backup="$(mktemp)"
    cp Cargo.lock "$backup"
    trap 'mv "$backup" Cargo.lock' EXIT
    cargo +nightly -Z minimal-versions update
    cargo +nightly test

# Check for undefined behaviour with Miri (needs `rustup +nightly component add miri`)
miri:
    cargo +nightly miri test

# Run tests with loom (see tests/loom.rs)
loom:
    RUSTFLAGS="--cfg loom -D warnings" cargo test --release --test loom
