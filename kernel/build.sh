#!/bin/bash
# Build the Tallow kernel with a private CARGO_HOME so this build does not
# contend on the shared ~/.cargo package-cache lock with other builds.
set -euo pipefail
source ~/export-esp.sh
export PATH="$HOME/.rustup/toolchains/esp/bin:$PATH"
export CARGO_HOME="$HOME/workspace/tallow/kernel/.cargo-home"
# The esp Rust fork (1.97.0-nightly, 2026-07-08) mis-validates the `callx8`
# in sched.rs's `naked_asm!` ("instruction use requires an option to be
# enabled") when `-C incremental` is combined with `--emit=link` — i.e.
# every normal `cargo build`. Either flag alone is fine (direct
# `rustc --emit=obj` with incremental passes; `cargo build` with
# incremental disabled passes). Until the toolchain is fixed, builds run
# with incremental disabled (~1 min full build; nothing to cache anyway).
export CARGO_INCREMENTAL=0
mkdir -p "$CARGO_HOME"
cd "$(dirname "$0")"
mkdir -p build
cargo build 2>&1 | tee build/build-cargo.log
