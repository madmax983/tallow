#!/bin/bash
# Build the Tallow kernel with a private CARGO_HOME so this build does not
# contend on the shared ~/.cargo package-cache lock with other builds.
set -euo pipefail
source ~/export-esp.sh
export PATH="$HOME/.rustup/toolchains/esp/bin:$PATH"
export CARGO_HOME="$HOME/workspace/tallow/kernel/.cargo-home"
mkdir -p "$CARGO_HOME"
cd "$(dirname "$0")"
mkdir -p build
cargo build 2>&1 | tee build/build-cargo.log
