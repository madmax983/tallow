#!/usr/bin/env python3
"""Tallow v0.3 regression: deterministic proof of tick + tasks + idle.

Runs the kernel in QEMU, captures UART0, and checks the output contract:
  - banner "Tallow v0.3" present (boot)
  - "AB" repeats once per tick: tasks A and B interleave and both run
  - "[t=N]" every 100 ticks with N = 100, 200, ... (monotonic tick count)
  - "[heartbeat] ticks = N (idle)" every 1000 ticks (idle task runs)

Usage: python3 regression.py [--seconds N]
Exit 0 on PASS, 1 on FAIL.
"""

import re
import subprocess
import sys
import os

KERNEL = os.path.dirname(os.path.abspath(__file__))
LOG = os.path.join(KERNEL, "build", "uart0.log")
ELF = os.path.join(KERNEL, "target", "xtensa-esp32s3-none-elf", "debug", "tallow")
SRC = os.path.join(KERNEL, "src")

BANNER_TITLE = 'Tallow v0.3 "tasks"'


def check_fresh() -> str | None:
    """Refuse to test a stale image: the ELF must be newer than every source.

    mkimage.py packages whatever ELF is on disk; if the last build failed,
    we'd otherwise "pass" against yesterday's kernel. Also covers the
    build script and linker script: a build-flag change without a source
    change must still trigger a rebuild.
    """
    if not os.path.exists(ELF):
        return f"ELF missing: {ELF} (run ./build.sh first)"
    elf_mtime = os.path.getmtime(ELF)
    inputs = []
    for name in sorted(os.listdir(SRC)):
        if name.endswith(".rs"):
            inputs.append(os.path.join(SRC, name))
    for extra in ("build.sh", "link.ld", "mkimage.py"):
        p = os.path.join(KERNEL, extra)
        if os.path.exists(p):
            inputs.append(p)
    for p in sorted(inputs):
        if os.path.getmtime(p) > elf_mtime:
            return (f"stale ELF: {os.path.basename(p)} is newer than "
                    f"target/.../debug/tallow (run ./build.sh first)")
    return None


def run_qemu(seconds: int) -> str:
    subprocess.run(["python3", "mkimage.py"], cwd=KERNEL,
                   check=True, capture_output=True)
    # Truncate the log first: a stale log from a previous run would
    # otherwise contaminate the counts (QEMU's file: backend truncates
    # on open, but belt-and-braces costs nothing).
    open(LOG, "w").close()
    subprocess.run(["./run-qemu.sh", str(seconds)],
                   cwd=KERNEL, capture_output=True)
    with open(LOG, errors="replace") as f:
        return f.read()


def scheduler_output(out: str) -> str | None:
    """Return only the scheduler's output: everything after the banner block.

    The ROM bootloader and the banner itself contain stray uppercase A/B
    letters (e.g. "SPI_FAST_FLASH_BOOT", "tasks: A, B"); the strict
    round-robin check below must not see them.
    """
    idx = out.find(BANNER_TITLE)
    if idx < 0:
        return None
    rest = out[idx:]
    # Skip to the first blank line after the banner block.
    m = re.search(r"\n\s*\n", rest)
    if not m:
        return None
    return rest[m.end():]


def main() -> int:
    seconds = int(sys.argv[sys.argv.index("--seconds") + 1]) \
        if "--seconds" in sys.argv else 15
    failures = []

    stale = check_fresh()
    if stale:
        print("FAIL")
        print(f"  - {stale}")
        return 1

    out = run_qemu(seconds)

    # 1. Boot banner.
    body = scheduler_output(out)
    if body is None:
        failures.append("banner missing or banner block malformed")

    # 2. AB interleave: count AB pairs in the scheduler's output.
    # Strip the marker/heartbeat lines first so we only count task output.
    ab_pairs = 0
    letters = ""
    if body is not None:
        task_out = re.sub(r"\[t=\d+\].*", "", body)
        task_out = re.sub(r"\[heartbeat\].*", "", task_out)
        ab_pairs = task_out.count("AB")
        if ab_pairs < 500:
            failures.append(f"only {ab_pairs} AB pairs (want >= 500)")
        letters = re.sub(r"[^AB]", "", task_out)

    # 3. Tick markers monotonic: [t=100], [t=200], ...
    ticks = [int(m) for m in re.findall(r"\[t=(\d+)\]", out)]
    if len(ticks) < 3:
        failures.append(f"only {len(ticks)} tick markers")
    elif any(b - a != 100 for a, b in zip(ticks, ticks[1:])):
        failures.append(f"tick markers not monotonic by 100: {ticks[:8]}")
    elif ticks != sorted(ticks):
        failures.append("tick markers not sorted")

    # 4. Heartbeats: [heartbeat] ticks = 1000 (idle), 2000, ...
    beats = [int(m) for m in
             re.findall(r"\[heartbeat\] ticks = (\d+) \(idle\)", out)]
    if not beats:
        failures.append("no heartbeat lines")
    elif any(b - a != 1000 for a, b in zip(beats, beats[1:])):
        failures.append(f"heartbeats not monotonic by 1000: {beats}")
    elif beats[0] != 1000:
        failures.append(f"first heartbeat at {beats[0]}, want 1000")

    # 5. No stray task letters outside the AB pattern: every A must be
    # followed by B (tasks run strictly round-robin, one slice each).
    # A trailing lone "A" is fine — timeout may land mid-tick.
    if body is not None:
        if letters.endswith("A"):
            letters = letters[:-1]
        if re.search(r"A(?!B)", letters):
            failures.append("found an 'A' not followed by 'B' (round-robin broken)")
        if letters and not letters.startswith("AB"):
            failures.append("task output does not start with AB")

    if failures:
        print("FAIL")
        for f in failures:
            print(f"  - {f}")
        return 1
    print(f"PASS: {ab_pairs} AB pairs, {len(ticks)} tick markers "
          f"(t={ticks[0]}..{ticks[-1]}), {len(beats)} heartbeats "
          f"(ticks={beats[0]}..{beats[-1]})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
