# Tallow 🕯️

**The little OS that could.** A tiny, best-in-breed operating system kernel for
the ESP32-S3, written in Rust.

Tallow is a from-scratch kernel for Espressif's ESP32-S3 (dual-core Xtensa LX7,
512 KiB SRAM, no MMU). It exists to answer one question: *how good can a tiny
kernel be?* Small enough to audit in a weekend, serious enough to bet a product
on — static task graphs, synchronous IPC, MPU-enforced compartments, and
build-time proofs where they matter most.

## Status

**v0.1 bring-up in progress** — toolchain + QEMU environment being built,
first serial banner next.

## Design

The [research](https://github.com/madmax983/tallow) behind Tallow surveyed the
state of the art in small-kernel techniques (seL4, Tock, Hubris, Zephyr, NuttX,
Theseus, …) anchored to the ESP32-S3. The conclusions that shape this kernel:

- **Static everything.** Tasks, memory regions, IPC endpoints are declared at
  compile time (Hubris-style). No heap in the kernel, no dynamic allocation at
  runtime — if it can't be proven bounded at build time, it doesn't ship.
- **MPU compartments, not flat memory.** Every surveyed ESP32-S3 OS runs flat.
  Tallow's differentiator: per-task MPU regions on a chip with no MMU.
- **Synchronous IPC.** seL4-style rendezvous calls with notifications — no
  unbounded queues, no priority inversion by accident.
- **Drivers as capsules.** Tock-style: drivers are untrusted components with
  narrow, grant-based access to hardware.
- **Proofs where they count.** Rust's type system everywhere; Verus/Kani
  proofs at build time for the 2–3 critical invariants (IPC core, MPU setup).
  Nothing verified *needs* to run on-device.
- **Crash-forward.** Ordered effect journaling and restart domains — a dead
  task restarts; the kernel never pretends the past didn't happen.

## Roadmap

| Milestone | Goal |
|-----------|------|
| v0.1 spark | Boot in QEMU, UART banner, panic handler |
| v0.2 tick | Timer interrupt, heartbeat, preemptive tick |
| v0.3 tasks | Static task table, context switch, idle task |
| v0.4 ipc | Synchronous IPC + notifications |
| v0.5 mpu | Per-task MPU compartments |
| v0.6 drivers | GPIO/LED, UART driver as capsule |
| v0.7 shell | Serial console shell |
| v0.8 ota | Signed A/B OTA update |

See [docs/ROADMAP.md](docs/ROADMAP.md) for detail.

## Building

Requires the Espressif Rust toolchain (`espup`) and Espressif's QEMU fork.
Full instructions in [docs/BUILDING.md](docs/BUILDING.md) once the environment
recipe is nailed down (coming with v0.1).

## License

Dual-licensed under MIT and Apache 2.0 — see `LICENSE-MIT` and `LICENSE-APACHE`.
