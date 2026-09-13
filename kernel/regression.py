#!/usr/bin/env python3
"""Tallow v0.5 regression: deterministic proof of tick + IPC + fault/restart.

Runs the kernel in QEMU, captures UART0, and checks the output contract:
  - banner 'Tallow v0.5 "mpu"' present (boot)
  - "[ipc NNNN] ping -> pong" lines, NNNN strictly sequential from 0:
    tasks A and B hold a real rendezvous conversation (call/recv/reply
    plus notify/wait); no exchange may be dropped, duplicated, or reordered
  - "[fault] task 1: synthetic fault injection; restart #k" lines with
    k strictly sequential from 1: task B faults at deterministic points,
    is killed and restarted, and the conversation continues gap-free
  - "[t=N]" every 100 ticks with N = 100, 200, ... (monotonic tick count)
  - "[heartbeat] ticks = N (idle)" every 1000 ticks (idle task runs)
  - every scheduler-output line matches one of those four shapes
    (no stray task output)

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

BANNER_TITLE = 'Tallow v0.5 "mpu"'


def check_fresh() -> str | None:
    """Refuse to test a stale image: the ELF must be newer than every source.

    mkimage.py packages whatever ELF is on disk; if the last build failed,
    we'd otherwise "pass" against yesterday's kernel. Also covers the
    build script and linker inputs: a build-flag change without a source
    change must still trigger a rebuild.
    """
    if not os.path.exists(ELF):
        return f"ELF missing: {ELF} (run ./build.sh first)"
    elf_mtime = os.path.getmtime(ELF)
    inputs = []
    for name in sorted(os.listdir(SRC)):
        if name.endswith(".rs"):
            inputs.append(os.path.join(SRC, name))
    for extra in ("build.sh", "link.ld", "memory.x", "mkimage.py"):
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

    The ROM bootloader and the banner itself contain stray text (e.g. the
    task table); the strict per-line check below must not see it.
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

    # 2. IPC conversation: [ipc NNNN] ping -> pong, NNNN = 0, 1, 2, ...
    # strictly sequential — no dropped, duplicated, or reordered exchange.
    # (Faults do not break the sequence: the retry is answered again.)
    nums = []
    if body is not None:
        nums = [int(m) for m in
                re.findall(r"\[ipc (\d+)\] ping -> pong", body)]
        if len(nums) < 500:
            failures.append(f"only {len(nums)} IPC exchanges (want >= 500)")
        elif nums != list(range(len(nums))):
            bad = next(i for i, (a, b) in
                       enumerate(zip(nums, range(len(nums)))) if a != b)
            failures.append(
                f"exchange sequence broken at line {bad}: "
                f"got {nums[bad]}, want {bad} (neighbors: {nums[max(0,bad-2):bad+3]})")

    # 2b. Fault/restarts: [fault] task 1: ...; restart #k, k = 1, 2, 3, ...
    # strictly sequential — every synthetic fault kills and restarts B.
    restarts = []
    if body is not None:
        restarts = [int(m) for m in
                    re.findall(r"\[fault\] task 1: .*?; restart #(\d+)", body)]
        if not restarts:
            failures.append("no [fault] restart lines (fault injection not running?)")
        elif restarts != list(range(1, len(restarts) + 1)):
            failures.append(f"restart counter not sequential: {restarts[:10]}")

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

    # 5. Line discipline: every scheduler-output line is an exchange line,
    # a fault/restart line, a tick marker, or a heartbeat. Anything else
    # is stray task output.
    if body is not None:
        line_re = re.compile(
            r"^(?:\[ipc \d+\] ping -> pong|\[fault\] task \d+: .*; restart #\d+| \[t=\d+\]|\[heartbeat\] ticks = \d+ \(idle\))$")
        for i, line in enumerate(body.splitlines()):
            if line.strip() == "":
                continue
            if not line_re.match(line):
                failures.append(f"stray output line {i}: {line!r:.80}")
                break

    if failures:
        print("FAIL")
        for f in failures:
            print(f"  - {f}")
        return 1
    print(f"PASS: {len(nums)} IPC exchanges (0..{nums[-1]}), "
          f"{len(restarts)} fault/restarts (#1..#{restarts[-1]}), "
          f"{len(ticks)} tick markers (t={ticks[0]}..{ticks[-1]}), "
          f"{len(beats)} heartbeats (ticks={beats[0]}..{beats[-1]})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
