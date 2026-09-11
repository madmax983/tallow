//! v0.3 "tasks": cooperative tasks on separate stacks.
//!
//! Two worker tasks (A and B) plus the idle task. Each worker owns a
//! private 4 KiB stack; the scheduler (`sched.rs`) runs each task via
//! `call_on_stack`, which swaps `a1` for the duration of the call.
//! Tasks are plain `extern "C" fn()`s: they do a slice of work and
//! return. No preemption — scheduling is cooperative, driven by the
//! 1 kHz polled tick.

/// Stack size per task: 4 KiB.
const STACK_WORDS: usize = 1024;
const STACK_BYTES: u32 = STACK_WORDS as u32 * 4;

/// Task A's private stack.
static mut STACK_A: [u32; STACK_WORDS] = [0; STACK_WORDS];
/// Task B's private stack.
static mut STACK_B: [u32; STACK_WORDS] = [0; STACK_WORDS];

/// A task: its stack top (initial `a1`) and entry point.
pub struct Task {
    pub stack_top: u32,
    pub entry: extern "C" fn(),
}

/// The task table. Fixed at two workers; the scheduler loop itself is
/// the idle task (it polls the timer and prints the heartbeat).
pub static mut TASKS: [Task; 2] = [
    Task {
        stack_top: 0,
        entry: task_a,
    },
    Task {
        stack_top: 0,
        entry: task_b,
    },
];

/// Initialize the task table: point each task at its stack.
///
/// Must be called once, before the scheduler runs. Interrupts are off
/// (we never enable them in v0.3), so the `static mut` writes are safe.
pub unsafe fn init() {
    unsafe {
        // addr_of! (not `.as_ptr()`): taking a shared reference to a
        // `static mut` is UB-adjacent and trips `static_mut_refs`.
        // The Xtensa windowed ABI needs 16-byte-aligned SP (`entry`
        // faults otherwise); the stacks are only 4-aligned, so round
        // each top *down* to 16 bytes — the stack grows down, so this
        // just costs us up to 12 bytes of each 4 KiB stack.
        TASKS[0].stack_top = ((core::ptr::addr_of!(STACK_A) as u32) + STACK_BYTES) & !0xF;
        TASKS[1].stack_top = ((core::ptr::addr_of!(STACK_B) as u32) + STACK_BYTES) & !0xF;
        debug_assert!(TASKS[0].stack_top & 0xF == 0);
        debug_assert!(TASKS[1].stack_top & 0xF == 0);
    }
}

/// Task A: one slice of work — emit our letter.
extern "C" fn task_a() {
    crate::print!("A");
}

/// Task B: one slice of work — emit our letter.
extern "C" fn task_b() {
    crate::print!("B");
}
