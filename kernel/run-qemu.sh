#!/usr/bin/env bash
# Boot the Tallow kernel in QEMU's esp32s3 machine and capture UART0.
# Usage: ./run-qemu.sh [seconds]   (expects build/flash_image.bin to exist;
#   optional timeout in seconds, default 15)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
QEMU="$HOME/workspace/tooling/qemu-esp32/qemu/bin/qemu-system-xtensa"
# libslirp is extracted from the Ubuntu .deb into the workspace (the sandbox
# rootfs can be wiped by VM replacements, so we don't rely on /usr).
export LD_LIBRARY_PATH="$HOME/workspace/tooling/qemu-esp32/libs/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
LOG="$HERE/build/uart0.log"
SECS="${1:-15}"

# -nographic: no GUI; first serial port would go to stdio, but we redirect it
#   explicitly to a file so the monitor doesn't mux into the capture.
# -no-reboot: exit (rather than reboot-loop) if the guest resets.
timeout "$SECS" "$QEMU" \
  -nographic \
  -machine esp32s3 \
  -drive file="$HERE/build/flash_image.bin",if=mtd,format=raw \
  -serial file:"$LOG" \
  -monitor none \
  -no-reboot || true

echo "--- uart0.log ---"
cat "$LOG"
echo "-----------------"
if grep -q "the little OS that could" "$LOG"; then
  echo "BOOT OK: banner found"
else
  echo "BOOT FAIL: banner missing"
  exit 1
fi
