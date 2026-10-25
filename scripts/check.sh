#!/usr/bin/env bash
set -euo pipefail

source ~/.cargo/env
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features --no-fail-fast
