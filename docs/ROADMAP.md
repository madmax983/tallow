# Tallow roadmap

Living document. Checked boxes are verified in QEMU (`qemu-system-xtensa
-machine esp32s3`) unless noted.

## v0.1 — spark (done, verified in QEMU)
- [x] `esp` Rust toolchain builds `xtensa-esp32s3-none-elf` in this environment
- [x] Espressif QEMU fork boots an image on the `esp32s3` machine
- [x] Minimal kernel: entry point, UART0 banner, panic handler
- [x] Serial log captured from QEMU proves the boot

## v0.2 — tick (done, verified in QEMU)
- [x] Timer Group 0, Timer 0 configured for 1 kHz (80 MHz APB / 80 = 1 MHz, alarm every 1000)
- [x] Heartbeat on serial: `[heartbeat] ticks = 1000`, `2000`, … (proof the kernel is alive)
- [x] Polled, not interrupt-driven: QEMU's esp32s3 interrupt matrix model proved
      unreliable (ROM leaves stale mappings; CCOUNT not recognized by assembler).
      Proper interrupt-driven tick moves to v0.3.

## v0.3 — tasks (done, verified in QEMU)
- [x] Static task table declared at compile time (no dynamic task creation)
- [x] Two tasks (A, B) on private 4 KiB stacks, via `call_on_stack`
      (naked `entry a1, 0` + `mov a9, a2` + `callx8`: the callee's `a1`
      is the caller's `a9` in the windowed ABI — the two-line fix that
      unblocked the whole milestone)
- [x] Cooperative round-robin scheduler: each 1 kHz polled tick runs A
      then B; the scheduler loop itself is the idle task (heartbeat)
- [x] `ABAB…` on UART with `[t=N]` every 100 ticks (monotonic) and
      `[heartbeat] ticks = N (idle)` every 1000 ticks — two tasks
      demonstrably interleaving
- [x] `kernel/regression.py`: deterministic PASS against a fresh image
      (banner gate, ≥500 AB pairs, tick/heartbeat monotonicity, stale-ELF
      rejection, strict round-robin check)
- [x] Cooperative, not preemptive, by design for this milestone: the
      timer interrupt *reached the exception vector*, but the tested
      return paths did not resume under the current ESP32-S3 QEMU setup.
      Preemptive scheduling stays future work — the polled tick is the
      honest primitive until exception return is proven.

## v0.4 — ipc (done, verified in QEMU)
- [x] seL4-style synchronous rendezvous IPC between tasks
- [x] Notifications (async signals) for driver events
- [x] Zero-copy where possible; bounded message sizes, always
- [x] Symmetric coroutine context switch (`jx`, no cross-stack calls)

## v0.5 — mpu
- [ ] ESP32-S3 MPU regions programmed per task at switch time
- [ ] Faulting task is killed and restarted; kernel survives
- [ ] **This is the differentiator — no other S3 OS does this**

## v0.6 — drivers
- [ ] Driver model: Tock-style capsules, untrusted, grant-based MMIO
- [ ] GPIO + LED driver, UART driver as a capsule

## v0.7 — shell
- [ ] Serial console: line editing, `help`, `tasks`, `mem`, `uptime`

## v0.8 — ota
- [ ] Signed A/B partition update over serial (then Wi-Fi)
- [ ] Rollback on failed boot (watchdog + boot counter)

## Later / research
- [ ] Verus proofs for the IPC core and MPU programming
- [ ] Wi-Fi via esp-radio (evaluate binary-blob cost honestly)
- [ ] Second core: static AMP split (net core / app core)
- [ ] Power: light-sleep with RTC wake, measured current numbers
