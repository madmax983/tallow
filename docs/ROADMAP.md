# Tallow roadmap

Living document. Checked boxes are verified in QEMU (`qemu-system-xtensa
-machine esp32s3`) unless noted.

## v0.1 — spark (done, verified in QEMU)
- [x] `esp` Rust toolchain builds `xtensa-esp32s3-none-elf` in this environment
- [x] Espressif QEMU fork boots an image on the `esp32s3` machine
- [x] Minimal kernel: entry point, UART0 banner, panic handler
- [x] Serial log captured from QEMU proves the boot

## v0.2 — tick
- [ ] Timer group interrupt wired, periodic tick at 1 kHz
- [ ] Heartbeat on serial (proof the kernel is alive, not just booted)
- [ ] Tickless idle: `waiti` in the idle path, wake on interrupt

## v0.3 — tasks
- [ ] Static task table declared at compile time (no dynamic task creation)
- [ ] Xtensa context switch (windowed ABI save/restore)
- [ ] Fixed-priority preemptive scheduler, idle task
- [ ] Two tasks demonstrably interleaving in QEMU

## v0.4 — ipc
- [ ] seL4-style synchronous rendezvous IPC between tasks
- [ ] Notifications (async signals) for driver events
- [ ] Zero-copy where possible; bounded message sizes, always

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
