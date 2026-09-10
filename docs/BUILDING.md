# Building Tallow

Tallow targets the ESP32-S3 (Xtensa LX7). Mainline Rust does not support
Xtensa, so we use Espressif's Rust fork; mainline QEMU does not emulate the
S3, so we use Espressif's QEMU fork.

## Prerequisites (one-time setup)

1. **Espressif Rust toolchain** via [espup](https://github.com/esp-rs/espup):
   ```bash
   # install espup, then:
   espup install
   ```
   This installs the `esp` toolchain into `~/.rustup/toolchains/esp`
   (rustc 1.97.0-nightly esp fork, Xtensa GCC `esp-15.2.0`, LLVM
   `esp-20.1.1`, rust-src for `-Z build-std`).
2. **Espressif's QEMU fork** (prebuilt `qemu-xtensa-softmmu` tarball from
   [espressif/qemu releases](https://github.com/espressif/qemu/releases),
   version `9.2.2 esp_develop_9.2.2_20260417`), extracted to
   `~/workspace/tooling/qemu-esp32/qemu/`. It needs `libslirp.so.0`, which
   is *not* bundled — extract it from the Ubuntu `libslirp0` .deb into
   `~/workspace/tooling/qemu-esp32/libs/` (see `kernel/run-qemu.sh`).
3. **esptool** 5.x: `pip install esptool` (invoke as `python3 -m esptool`).

Activate the toolchain in any fresh shell before building:

```bash
source ~/export-esp.sh                      # GCC on PATH, LIBCLANG_PATH
export PATH="$HOME/.rustup/toolchains/esp/bin:$PATH"
```

## Build, image, boot

```bash
cd kernel
./build.sh        # cargo build (private CARGO_HOME, build-std core)
python3 mkimage.py   # esptool elf2image -> 4 MiB flash image, app at offset 0x0
./run-qemu.sh        # boots in QEMU esp32s3 machine, checks the banner
```

`run-qemu.sh` captures UART0 to `build/uart0.log` and greps for the boot
banner; it exits non-zero if the banner is missing.

## How the boot works

- `kernel/memory.x` links everything into HP SRAM via the IRAM bus
  (`0x40370000`, 448 KiB, executable). **Xtensa literal pools (`.literal*`)
  must precede the code that references them** — the linker hard-errors
  otherwise. `ENTRY(_start)`; the ROM jumps to the ELF entry address.
- `_start` installs the stack at `0x3FCFFFE0` (DRAM top). The stack constant
  is materialized by the *compiler*, never hand-written as `l32r` with an
  inline literal (the linker misresolves those).
- On ESP32-S2/S3/C3 the ROM loads the boot image from **flash offset 0x0**
  (not 0x1000 like the original ESP32). Our kernel is itself a valid
  `0xE9`-magic image, so no IDF second-stage bootloader is needed.
- UART0 (base `0x60000000`, FIFO `+0x00`, STATUS `+0x1C`,
  TXFIFO_CNT = bits [25:16], 128-byte FIFO) is left clocked by the ROM —
  the kernel just writes bytes.

## Sandbox gotchas

- Keep tooling under `~/workspace` or `$HOME`: VM replacements can wipe
  `/usr` and `/tmp` mid-task.
- Use a private `CARGO_HOME` per project (see `build.sh`) — the shared
  `~/.cargo` package-cache lock contends badly when several builds run.
- `asm!` on Xtensa needs `#![feature(asm_experimental_arch)]` even on the
  esp nightly.
