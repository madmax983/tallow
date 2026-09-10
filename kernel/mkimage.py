#!/usr/bin/env python3
"""Build a QEMU-bootable SPI flash image for the Tallow kernel (ESP32-S3).

Recipe:
  1. `esptool elf2image` -> converts the ELF's LOAD segments into the ESP
     image format (magic 0xE9, segment headers, entry point, checksum) that
     the S3 ROM bootloader understands.
  2. Assemble a 4 MiB flash image (0xFF = erased) with that image at offset
     0x0.

Why offset 0x0: on ESP32-S2/S3/C3 the ROM loads the second-stage image from
flash offset 0x0 (unlike the original ESP32, which uses 0x1000). Our kernel
*is* a valid boot image, so the ROM loads it directly — no IDF bootloader
or partition table needed.

QEMU only accepts flash sizes 2/4/8/16 MiB (it maps them to real flash
models); we use 4 MiB.
"""
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _ensure_esptool() -> None:
    """Make `python3 -m esptool` importable.

    esptool lives in a workspace-local pip target dir (the sandbox rootfs
    can be wiped by VM replacements, so we never rely on /usr/local).
    If it is missing, install it there on the spot.
    """
    import importlib.util

    pylibs = Path.home() / "workspace" / "esptools" / "py"
    if importlib.util.find_spec("esptool") is None:
        sys.path.insert(0, str(pylibs))
    if importlib.util.find_spec("esptool") is None:
        env = {
            "PIP_DISABLE_PIP_VERSION_CHECK": "1",
            "PIP_NO_INPUT": "1",
        }
        subprocess.run(
            [
                sys.executable, "-m", "pip", "install",
                "--quiet", "--target", str(pylibs), "esptool",
            ],
            check=True,
            env={**os.environ, **env},
        )
        sys.path.insert(0, str(pylibs))
    if importlib.util.find_spec("esptool") is None:
        sys.exit("could not make esptool importable; install it manually")


_ensure_esptool()
ELF = HERE / "target" / "xtensa-esp32s3-none-elf" / "debug" / "tallow"
BIN = HERE / "build" / "tallow.bin"
FLASH = HERE / "build" / "flash_image.bin"
FLASH_SIZE = 4 * 1024 * 1024
IMAGE_OFFSET = 0x0  # S3 ROM loads the boot image from flash offset 0


def main() -> None:
    (HERE / "build").mkdir(exist_ok=True)
    if not ELF.exists():
        sys.exit(f"ELF not found: {ELF} (run ./build.sh first)")

    # The esptool subprocess needs the workspace-local pylibs on its path too.
    pylibs = Path.home() / "workspace" / "esptools" / "py"
    env = dict(os.environ)
    env["PYTHONPATH"] = str(pylibs) + os.pathsep + env.get("PYTHONPATH", "")

    subprocess.run(
        [
            sys.executable, "-m", "esptool",
            "--chip", "esp32s3",
            "elf2image",
            "--flash-mode", "dio",
            "--flash-freq", "80m",
            "--flash-size", "4MB",
            "-o", str(BIN),
            str(ELF),
        ],
        check=True,
        env=env,
    )
    app = BIN.read_bytes()
    print(f"boot image: {len(app)} bytes")

    flash = bytearray(b"\xff" * FLASH_SIZE)
    flash[IMAGE_OFFSET : IMAGE_OFFSET + len(app)] = app
    FLASH.write_bytes(flash)
    print(f"flash image: {FLASH} ({FLASH_SIZE} bytes, app at 0x{IMAGE_OFFSET:x})")


if __name__ == "__main__":
    main()
