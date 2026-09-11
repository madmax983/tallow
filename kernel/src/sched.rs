//! v0.3 "tasks": the cooperative round-robin scheduler.
//!
//! The 1 kHz polled tick drives everything. Each tick, the scheduler
//! runs task A then task B (each on its own stack, via `call_on_stack`),
//! then does the idle task's work: a heartbeat line every 1000 ticks.
//!
//! Output contract (verified by `kernel/regression.py`):
//! - `AB` repeats once per tick: A and B demonstrably interleave,
//!   and both run because the tick fires.
//! - `[t=N]` every 100 ticks: the tick counter is monotonic.
//! - `[heartbeat] ticks = N (idle)` every 1000 ticks: the idle task runs.
//!
//! Why cooperative, not preemptive: the timer interrupt *fires* (the
//! exception vector was reached in testing), but the tested return paths
//! (`rfe`, manual `EPS` restore, `jx EPC1`) could not be made to resume
//! under the current ESP32-S3 QEMU setup. Preemption needs a working
//! exception return; until then, the tick is polled and tasks yield by
//! returning.

use core::arch::naked_asm;

/// Call `func` with the task's stack, then restore the caller's stack.
///
/// Naked: the `entry` sets up our register window (required even with a
/// 0-byte frame — without it, the subsequent `callx8` hangs), then we do
/// the stack switch with a single balanced `callx8`/`retw` pair.
/// Argument registers (windowed ABI): `a2` = `new_sp`, `a3` = `func`,
/// `a4` = `old_sp` (out-pointer).
///
/// # Safety
/// - `new_sp` must point to the top of writable memory the task may use
///   as its stack (16-byte aligned).
/// - `old_sp` must be a valid writable `*mut u32`.
/// - `func` must be a valid `extern "C" fn()` that returns (it runs with
///   interrupts disabled; it must not switch stacks itself).
/// - Callers must ensure the window depth is balanced: exactly one
///   `callx8` executes and returns via `retw` before our own `retw`.
///
/// Window mechanics (Xtensa windowed ABI): `callx8` rotates the window
/// by 8, so the callee's `a0`-`a7` are our `a8`-`a15`. In particular the
/// callee's `a1` (its stack pointer) is OUR `a9` — not our `a1`! So we
/// set `a9`, not `a1`, to the task's stack. Our own `a1` (the scheduler's
/// stack, visible to our caller as its `a9`) is saved/restored via
/// `old_sp`. The single `callx8`/`retw` pair leaves the window depth
/// unchanged on return.
#[unsafe(naked)]
unsafe extern "C" fn call_on_stack(new_sp: u32, func: extern "C" fn(), old_sp: *mut u32) {
    naked_asm!(
        "entry a1, 0",    // validate window (0-byte frame; required for callx8)
        "s32i a1, a4, 0", // *old_sp = our a1 (scheduler's stack)
        "mov a9, a2",     // our a9 = new_sp → callee's a1 (task's stack!)
        "callx8 a3",      // func() — new window, balanced
        "l32i a1, a4, 0", // restore our a1 (a9 was task-scoped, invisible to caller)
        "retw",           // back to scheduler
    )
}

/// Run one task (by index) on its own stack.
fn run_task(idx: usize) {
    let (stack_top, entry) = unsafe {
        let t = &crate::task::TASKS[idx];
        (t.stack_top, t.entry)
    };
    let mut old_sp: u32 = 0;
    unsafe { call_on_stack(stack_top, entry, &mut old_sp) };
    // old_sp is discarded: the scheduler never migrates stacks.
    let _ = old_sp;
}

/// The scheduler. Never returns.
///
/// Each 1 kHz tick: run A, run B (round-robin), then idle bookkeeping.
/// Between ticks we spin on the timer — that spin *is* the idle task.
pub fn run() -> ! {
    let mut ticks: u32 = 0;
    loop {
        // Idle: wait for the next tick.
        while !(unsafe { crate::timer::poll_tick() }) {}

        ticks = ticks.wrapping_add(1);

        // Round-robin: A then B, each on its own stack.
        run_task(0);
        run_task(1);

        // Tick marker every 100 ticks — proves monotonicity.
        if ticks % 100 == 0 {
            crate::println!(" [t={}]", ticks);
        }

        // Idle heartbeat every 1000 ticks.
        if ticks % 1000 == 0 {
            crate::println!("[heartbeat] ticks = {} (idle)", ticks);
        }
    }
}
