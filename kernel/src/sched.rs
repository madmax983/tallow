//! v0.4 "ipc": the cooperative round-robin scheduler.
//!
//! The 1 kHz polled tick drives everything. Each tick, the scheduler
//! resumes every `Ready` task once, in task-index order, through a
//! symmetric coroutine switch; the task runs one slice and switches back.
//! Blocked tasks are skipped — they consume no CPU until the kernel wakes
//! them at rendezvous.
//!
//! The switch (`ctx_switch!`) is the heart of v0.4. v0.3 entered tasks
//! with a windowed `callx8`/`retw` pair across stacks; the Xtensa window
//! spill/fill handlers locate spilled windows by stack pointer, so the
//! second cross-stack call faulted (`EXC_WINDOW_UNDERFLOW8`). The
//! coroutine switch never lets a windowed call or return cross stacks:
//! it runs inline in the caller's window, saves the full window state
//! (`a0`–`a15`, `SP`, `SAR`, `WindowBase`, plus a resume PC) into the
//! outgoing context, restores the incoming context's, and `jx`s to its
//! resume PC.
//!
//! Soundness rests on one invariant, maintained by construction: **at
//! every switch, only the current window is live.** The macro forces the
//! saved `WindowStart` to `1 << WindowBase` (only the current window
//! live), declaring every caller window dead. That is sound because the declared-dead
//! windows are never resumed: the scheduler loop and the task entries
//! are `-> !`, and the task yield expands inline in the task's own
//! frame (see `yield_now!`), so no task ever has a live caller at yield
//! time. Consequences:
//!
//! - The ROM spill/fill handlers only ever see windows whose `a1`
//!   points at the current stack: every spill lands in the right stack's
//!   spill area, every fill reads it back. Cross-stack window traffic is
//!   impossible by construction.
//! - The handlers can never select a stale cross-stack window as
//!   "oldest live" and spill it through a garbage `a1` — stale windows
//!   are marked dead, not live.
//!
//! Neither the scheduler nor any task ever returns (`run` and the task
//! entries are `-> !` loops), so the switch may freely clobber `a0`: the
//! resume PC is materialized into `a0` with `movi` (the assembler's own
//! literal-pool machinery — the same the compiler uses for `in(reg)`
//! operands; a hand-written `l32r` with an inline literal is mangled by
//! the Xtensa linker, per the v0.3 lesson). A `call0` here would be
//! wrong: it captures the PC but *jumps* to its target, skipping the
//! switch.
//!
//! The `wsr windowbase` renames every `ar` register, so the incoming
//! context pointer rides across in `EXCSAVE1` — a special register,
//! unaffected by window rotation. No exception can fire mid-switch:
//! interrupts are off and this sequence makes no calls and executes no
//! `entry`.
//!
//! From the compiler's side the block is fully opaque (default `asm!`
//! options: memory may be clobbered); `a2`/`a3` carry the pointers as
//! `inout` (the restore overwrites them) and `a4` is scratch. `a1`
//! (`SP`) is changed and then restored to the identical value before
//! any Rust code observes it on the fall-through path; `a5`–`a15` are
//! saved and restored by construction, so the compiler's assumption that
//! they survive holds.
//!
//! Output contract (verified by `kernel/regression.py`):
//! - `[ipc NNNN] ping -> pong`: one line per completed A<->B exchange,
//!   NNNN strictly sequential from 0000 (the conversation is real IPC).
//! - ` [t=N]` every 100 ticks: the tick counter is monotonic.
//! - `[heartbeat] ticks = N (idle)` every 1000 ticks: the idle task runs.

use core::ptr::addr_of_mut;

use crate::ipc::N_TASKS;
use crate::task::{task_ptr, TaskState};

/// Suspended register state: one per task, plus one for the scheduler.
///
/// `#[repr(C)]` with the field order below; the `ctx_switch!` macro
/// addresses fields by these byte offsets:
/// `pc` = 0, `sp` = 4, `a[i]` = 8 + 4*i, `sar` = 72,
/// `windowbase` = 76, `windowstart` = 80.
///
/// `windowstart` is always `1 << windowbase` in a saved context (see the
/// module docs): only the current window is live at switch time.
#[repr(C)]
pub(crate) struct Context {
    /// Resume address (address of the end of the `ctx_switch!` expansion).
    pub pc: u32,
    /// Stack pointer (`a1`).
    pub sp: u32,
    /// The whole current window, `a0`..`a15`.
    pub a: [u32; 16],
    /// Shift-amount register.
    pub sar: u32,
    /// Which physical window is current.
    pub windowbase: u32,
    /// Which windows are live: always 1 (see above).
    pub windowstart: u32,
}

impl Context {
    const fn zero() -> Self {
        Context {
            pc: 0,
            sp: 0,
            a: [0; 16],
            sar: 0,
            windowbase: 0,
            windowstart: 0,
        }
    }

    /// A context for a task that has never run: resuming it `jx`s to
    /// `entry` with a clean window (`WindowBase` = 0, only window 0 live),
    /// `SP` at the stack top, everything else zero. The task's prologue
    /// `entry` finds window 1 free, exactly like a fresh thread.
    const fn initial(entry: u32, stack_top: u32) -> Self {
        Context {
            pc: entry,
            sp: stack_top,
            a: [0; 16],
            sar: 0,
            windowbase: 0,
            windowstart: 1, // 1 << 0: only window 0 live
        }
    }
}

/// The scheduler's own suspended state.
static mut SCHED_CTX: Context = Context::zero();
/// One suspended state per task.
static mut TASK_CTX: [Context; 2] = [Context::zero(), Context::zero()];

/// Raw pointer to the scheduler's context.
pub(crate) fn sched_ctx_ptr() -> *mut Context {
    addr_of_mut!(SCHED_CTX)
}

/// Raw pointer to task `idx`'s context.
pub(crate) fn task_ctx_ptr(idx: usize) -> *mut Context {
    debug_assert!(idx < N_TASKS);
    unsafe { addr_of_mut!(TASK_CTX[idx]) }
}

/// Index of the task currently running its slice. Read by `ipc::me()`
/// so syscalls know who is calling. Written only here, with interrupts
/// disabled and never reentrantly — the `static mut` is safe.
static mut CURRENT_TASK: usize = 0;

/// Which task is running right now. Only valid during a task slice.
pub(crate) unsafe fn current_task() -> usize {
    unsafe { CURRENT_TASK }
}

/// Craft the initial context for task `idx`: first resume jumps to
/// `entry` on a fresh stack. Called once from `task::init()`, before the
/// scheduler runs.
pub(crate) unsafe fn init_task_context(idx: usize, entry: u32, stack_top: u32) {
    unsafe {
        addr_of_mut!(TASK_CTX[idx]).write(Context::initial(entry, stack_top));
    }
}

/// Symmetric coroutine switch: suspend the current window/stack into
/// `*old`, resume `*new`.
///
/// Expands inline in the caller's window — deliberately NOT a function
/// call, so no windowed call or return ever crosses stacks (that was
/// v0.3's fatal flaw). The caller's own `a0` is saved first (the
/// compiler may hold live values there — LLVM forbids declaring `a0`
/// clobbered, so we preserve it by hand); the resume PC is then
/// materialized into `a0` with `movi` (assembler literal pool) and
/// saved as `pc`.
///
/// See the module docs for the soundness invariant (only the current
/// window is live at switch time; saved `WindowStart` is forced to
/// `1 << WindowBase`).
macro_rules! ctx_switch {
    ($old:expr, $new:expr) => {{
        let mut old: *mut crate::sched::Context = $old;
        let mut new: *mut crate::sched::Context = $new;
        unsafe {
            core::arch::asm!(
                // Save the caller's a0 first: `movi` below clobbers it.
                "s32i a0, a2, 8", // a[0] = caller's a0
                // a0 <- resume PC (label 4f, the end of this expansion).
                // Falls through: unlike `call0`, `movi` does not jump.
                "movi a0, 4f",
                // ---- save outgoing state into *old (a2) ----
                "s32i a0, a2, 0",  // pc: resume address
                "s32i a1, a2, 4",  // sp
                "s32i a1, a2, 12", // a[1]
                "s32i a2, a2, 16", // a[2]
                "s32i a3, a2, 20", // a[3]
                "s32i a4, a2, 24",
                "s32i a5, a2, 28",
                "s32i a6, a2, 32",
                "s32i a7, a2, 36",
                "s32i a8, a2, 40",
                "s32i a9, a2, 44",
                "s32i a10, a2, 48",
                "s32i a11, a2, 52",
                "s32i a12, a2, 56",
                "s32i a13, a2, 60",
                "s32i a14, a2, 64",
                "s32i a15, a2, 68",
                "rsr a4, sar",
                "s32i a4, a2, 72",
                "rsr a4, windowbase",
                "s32i a4, a2, 76",
                // WindowStart forced to "only the current window is live".
                // WindowStart is a bitmask over PHYSICAL windows, so that
                // is `1 << WindowBase`, NOT literal 1: a literal 1 with
                // WindowBase=2 would mark physical window 0 live, and the
                // next `entry` rotating into window 0 would raise a
                // spurious window-overflow into the ROM handler. (a5 was
                // already saved above, so it is scratch; SAR was already
                // saved, so `ssl` may clobber it.)
                "movi a5, 1",
                "ssl a4",     // SAR = WindowBase
                "sll a5, a5", // a5 = 1 << WindowBase
                "s32i a5, a2, 80",
                // ---- restore incoming state from *new (a3) ----
                "wsr a3, excsave1", // mailbox the pointer across wsr windowbase
                "l32i a4, a3, 76",  // new windowbase
                "wsr a4, windowbase",
                "rsync",            // ar names now address the new window
                "rsr a3, excsave1", // a3 = new, in the new window's naming
                "l32i a4, a3, 80",  // new windowstart = 1 << windowbase
                "wsr a4, windowstart",
                "l32i a4, a3, 72",  // new sar
                "wsr a4, sar",
                "rsync",
                "l32i a1, a3, 4", // new sp — the old stack is abandoned here
                "l32i a2, a3, 16",
                "l32i a4, a3, 24",
                "l32i a5, a3, 28",
                "l32i a6, a3, 32",
                "l32i a7, a3, 36",
                "l32i a8, a3, 40",
                "l32i a9, a3, 44",
                "l32i a10, a3, 48",
                "l32i a11, a3, 52",
                "l32i a12, a3, 56",
                "l32i a13, a3, 60",
                "l32i a14, a3, 64",
                "l32i a15, a3, 68",
                "l32i a0, a3, 8", // caller's a0 back
                "l32i a4, a3, 0", // resume pc -> scratch
                "l32i a3, a3, 20", // a3 last — it was the base register
                "jx a4",          // resume; never a callx/retw
                "4:",
                inout("a2") old,
                inout("a3") new,
                out("a4") _,
            );
        }
    }};
}

pub(crate) use ctx_switch;

/// Suspend the calling task and resume the scheduler. When the scheduler
/// next resumes this task, execution continues after this macro, inside
/// the task's own window — locals and call depth intact.
///
/// This MUST expand inline in the task function's own frame (it does:
/// both demo tasks invoke it at the top level of their loop body), never
/// inside a helper call. The switch declares every non-current window
/// dead; yielding from inside a nested call would strand the caller's
/// window — the task would resume with a dead window on its return path.
/// One syscall per slice, then yield at the top: that is the userspace
/// contract.
macro_rules! yield_now {
    () => {{
        let idx = unsafe { crate::sched::current_task() };
        crate::sched::ctx_switch!(
            crate::sched::task_ctx_ptr(idx),
            crate::sched::sched_ctx_ptr()
        )
    }};
}

pub(crate) use yield_now;

/// The scheduler. Never returns.
///
/// Each 1 kHz tick: resume every Ready task once, then idle bookkeeping.
/// Between ticks we spin on the timer — that spin *is* the idle task.
pub fn run() -> ! {
    let mut ticks: u32 = 0;
    loop {
        // Idle: wait for the next tick.
        while !(unsafe { crate::timer::poll_tick() }) {}

        ticks = ticks.wrapping_add(1);

        // Round-robin over Ready tasks; blocked tasks are skipped.
        // Each resume runs one slice; the task switches back via yield_now!().
        let mut idx = 0;
        while idx < N_TASKS {
            let ready = unsafe { (*task_ptr(idx)).state == TaskState::Ready };
            if ready {
                unsafe { CURRENT_TASK = idx };
                ctx_switch!(sched_ctx_ptr(), task_ctx_ptr(idx));
            }
            idx += 1;
        }

        // Tick marker every 100 ticks — proves monotonicity.
        if ticks.is_multiple_of(100) {
            crate::println!(" [t={}]", ticks);
        }

        // Idle heartbeat every 1000 ticks.
        if ticks.is_multiple_of(1000) {
            crate::println!("[heartbeat] ticks = {} (idle)", ticks);
        }
    }
}
