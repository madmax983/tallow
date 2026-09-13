//! v0.4 "ipc": static tasks that actually converse.
//!
//! Two worker tasks (A and B) plus the idle task (the scheduler loop
//! itself). Each worker owns a private 4 KiB stack and a suspended
//! register context (`sched::Context`); the scheduler resumes every
//! `Ready` task once per tick through the symmetric coroutine switch.
//! Tasks are `extern "C" fn() -> !` infinite loops: each iteration runs
//! one slice of work, then yields explicitly with `sched::yield_now!()`.
//! The resume continues after the yield with locals and call depth
//! intact — but the v0.4 demo tasks keep the simple re-invocation
//! protocol from the userspace contract (one syscall per slice, `Blocked`
//! means yield and try again), so they stay small state machines. No
//! preemption — scheduling is cooperative, driven by the 1 kHz polled tick.
//!
//! The demo conversation: A `call`s B on `EP_PING` with `ping {n}`, B
//! `recv`s it, `notify`s A (bit 0, "reply sent"), and `reply`s
//! `pong {n}`. A prints one `[ipc nnnn] ping -> pong` line per exchange,
//! then `wait`s for the notification before the next call. The sequence
//! number rides *in the message*, so a retried `n` (v0.5: partner
//! restarted mid-call) is answered again with no gap.

use crate::ipc::{
    CallResult, IpcError, NotifyResult, PendingOp, RecvResult, ReplyResult, WaitResult, EP_PING,
    MSG_MAX, N_TASKS, TASK_A,
};

/// A task's scheduling state: runnable, or suspended in IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Blocked,
}

/// Stack size per task: 4 KiB.
const STACK_WORDS: usize = 1024;
const STACK_BYTES: u32 = STACK_WORDS as u32 * 4;

/// Task A's private stack.
static mut STACK_A: [u32; STACK_WORDS] = [0; STACK_WORDS];
/// Task B's private stack.
static mut STACK_B: [u32; STACK_WORDS] = [0; STACK_WORDS];

/// A task: its stack and entry point, plus its IPC state.
///
/// Everything here is kernel-owned and static. `msg` is the task's
/// single staging buffer: outgoing message while sending, incoming
/// message once a rendezvous completes. The task's suspended register
/// state lives in the scheduler (`sched::Context`), crafted by `init()`.
pub struct Task {
    pub stack_top: u32,
    pub entry: extern "C" fn() -> !,
    pub name: &'static str,
    pub state: TaskState,
    pub op: Option<PendingOp>,
    pub msg: [u8; MSG_MAX],
    pub msg_len: u8,
    pub pending: u32,
    /// Who is waiting for *our* reply (set when our `recv` took a `call`).
    pub reply_to: Option<usize>,
    /// Who owes *us* a reply (set when our `call`'s send phase landed).
    pub reply_from: Option<usize>,
}

const fn blank_task(entry: extern "C" fn() -> !, name: &'static str) -> Task {
    Task {
        stack_top: 0,
        entry,
        name,
        state: TaskState::Ready,
        op: None,
        msg: [0; MSG_MAX],
        msg_len: 0,
        pending: 0,
        reply_to: None,
        reply_from: None,
    }
}

/// The task table. Fixed at two workers; the scheduler loop itself is
/// the idle task (it polls the timer and prints the heartbeat).
pub static mut TASKS: [Task; 2] = [blank_task(task_a, "A"), blank_task(task_b, "B")];

/// Raw pointer to task `idx`'s state.
///
/// `addr_of_mut!` (not a `&mut` to a `static mut`): taking a reference
/// is UB-adjacent and trips `static_mut_refs`. Callers must uphold the
/// kernel's single-threaded discipline (interrupts never enabled).
pub fn task_ptr(idx: usize) -> *mut Task {
    debug_assert!(idx < N_TASKS);
    unsafe { core::ptr::addr_of_mut!(TASKS[idx]) }
}

/// Initialize the task table: point each task at its stack and craft its
/// initial coroutine context (first resume jumps to the entry point).
///
/// Must be called once, before the scheduler runs. Interrupts are off
/// (we never enable them), so the `static mut` writes are safe.
pub unsafe fn init() {
    unsafe {
        // The Xtensa windowed ABI needs 16-byte-aligned SP (`entry`
        // faults otherwise); the stacks are only 4-aligned, so round
        // each top *down* to 16 bytes — the stack grows down, so this
        // just costs us up to 12 bytes of each 4 KiB stack.
        (*task_ptr(0)).stack_top = ((core::ptr::addr_of!(STACK_A) as u32) + STACK_BYTES) & !0xF;
        (*task_ptr(1)).stack_top = ((core::ptr::addr_of!(STACK_B) as u32) + STACK_BYTES) & !0xF;
        debug_assert!((*task_ptr(0)).stack_top & 0xF == 0);
        debug_assert!((*task_ptr(1)).stack_top & 0xF == 0);
        crate::sched::init_task_context(0, task_entry_addr(0), (*task_ptr(0)).stack_top);
        crate::sched::init_task_context(1, task_entry_addr(1), (*task_ptr(1)).stack_top);
    }
}

/// Task `idx`'s entry address as a `u32`.
///
/// The `fn -> u32` cast is deliberate (32-bit target; the context stores a
/// 32-bit PC), so the pedantic `fn_to_numeric_cast` lint is allowed here.
#[allow(clippy::fn_to_numeric_cast)]
pub(crate) fn task_entry_addr(idx: usize) -> u32 {
    unsafe { (*task_ptr(idx)).entry as u32 }
}

/// Write `prefix` + decimal `n` (no leading zeros) into `buf`.
/// Returns the number of bytes written. `buf` must fit prefix + 10 digits.
fn tag(buf: &mut [u8], prefix: &[u8], n: u32) -> usize {
    let mut len = 0;
    for &b in prefix {
        buf[len] = b;
        len += 1;
    }
    let mut div = 1_000_000_000u32;
    let mut started = false;
    while div > 0 {
        let d = (n / div) % 10;
        if d != 0 || started || div == 1 {
            buf[len] = b'0' + d as u8;
            len += 1;
            started = true;
        }
        div /= 10;
    }
    len
}

// ---------------------------------------------------------------------------
// Demo tasks: the ping-pong conversation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum PhaseA {
    Call,
    Wait,
}

static mut PHASE_A: PhaseA = PhaseA::Call;
static mut COUNT_A: u32 = 0;

/// Task A: the initiator. `call`s B with `ping {n}`, prints one line per
/// completed exchange, then `wait`s for B's "reply sent" notification.
/// A coroutine: one slice per scheduler resume, then an explicit yield.
extern "C" fn task_a() -> ! {
    use crate::ipc::{call, wait};
    // Fresh-start initialization. Runs once at boot and again after every
    // fault restart: the entry point is re-entered from a clean context,
    // while yields resume inside the loop below, never here. COUNT_A is
    // deliberately NOT reset — the sequence continues where it left off,
    // so a restarted A retries the same n instead of rewinding.
    unsafe {
        PHASE_A = PhaseA::Call;
    }
    loop {
        unsafe {
            match PHASE_A {
                PhaseA::Call => {
                    let n = COUNT_A;
                    let mut msg = [0u8; MSG_MAX];
                    let mlen = tag(&mut msg, b"ping ", n);
                    let mut rbuf = [0u8; MSG_MAX];
                    match call(EP_PING, &msg[..mlen], &mut rbuf) {
                        CallResult::Replied(rlen) => {
                            let mut expect = [0u8; MSG_MAX];
                            let elen = tag(&mut expect, b"pong ", n);
                            if rlen != elen || rbuf[..rlen] != expect[..elen] {
                                panic!("task A: bad reply to ping {}", n);
                            }
                            crate::println!("[ipc {:04}] ping -> pong", n);
                            COUNT_A = n.wrapping_add(1);
                            PHASE_A = PhaseA::Wait;
                        }
                        CallResult::Blocked => {}
                        // v0.5: the partner died mid-call; retry the same n —
                        // the sequence number rides in the message, so the
                        // retry is answered again with no gap.
                        CallResult::Failed(IpcError::PartnerFaulted) => {}
                        CallResult::Failed(e) => panic!("task A: call failed: {:?}", e),
                    }
                }
                PhaseA::Wait => match wait() {
                    WaitResult::Signaled(_) => PHASE_A = PhaseA::Call,
                    WaitResult::Blocked => {}
                    WaitResult::Failed(e) => panic!("task A: wait failed: {:?}", e),
                },
            }
        }
        crate::sched::yield_now!();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PhaseB {
    Recv,
    Reply,
}

static mut PHASE_B: PhaseB = PhaseB::Recv;
static mut LAST_PING: [u8; MSG_MAX] = [0; MSG_MAX];
static mut LAST_PING_LEN: usize = 0;
/// Exchanges completed by B (incremented per received ping). Drives the
/// v0.5 synthetic fault schedule below.
static mut B_EXCHANGES: u32 = 0;

/// v0.5 synthetic fault injection: request a kernel fault-restart.
///
/// QEMU's ESP32-S3 model does not enforce the PMS, and the ROM exception
/// vectors are read-only (verified empirically: VECBASE=0x40000000,
/// KExc=VECBASE+0x300, writes do not stick), so a real CPU exception
/// cannot be hooked in QEMU. Instead, the task explicitly requests the
/// kernel's fault path: the scheduler kills and restarts this task and
/// wakes the IPC partner with `PartnerFaulted`. The kill/restart logic
/// is identical to what a hardware MPU fault would trigger; only the
/// trigger is synthetic. Deterministic, so the regression can assert
/// exact restart points.
fn fault_inject() -> ! {
    unsafe { crate::sched::request_restart("synthetic fault injection") }
}

/// Task B: the responder. `recv`s the ping, `notify`s A, `reply`s
/// `pong {n}` — the echo is built by swapping the `ping` prefix.
/// A coroutine: one slice per scheduler resume, then an explicit yield.
extern "C" fn task_b() -> ! {
    use crate::ipc::{notify, recv, reply};
    // Fresh-start initialization (see task_a). PHASE_B resets to Recv so
    // a B that faulted mid-Reply re-enters the rendezvous cleanly instead
    // of re-running a reply whose partner link was scrubbed.
    unsafe {
        PHASE_B = PhaseB::Recv;
    }
    loop {
        unsafe {
            match PHASE_B {
                PhaseB::Recv => {
                    let mut rbuf = [0u8; MSG_MAX];
                    match recv(EP_PING, &mut rbuf) {
                        RecvResult::Received(rlen) => {
                            if rlen < 5 || rbuf[..4] != *b"ping" {
                                panic!("task B: malformed ping ({} bytes)", rlen);
                            }
                            LAST_PING[..rlen].copy_from_slice(&rbuf[..rlen]);
                            LAST_PING_LEN = rlen;
                            PHASE_B = PhaseB::Reply;
                        }
                        RecvResult::Blocked => {}
                        RecvResult::Failed(e) => panic!("task B: recv failed: {:?}", e),
                    }
                }
                PhaseB::Reply => {
                    // v0.5 synthetic fault injection: fault here — after
                    // receiving, before notifying/replying — while A is
                    // blocked in `call` awaiting the reply. Deterministic
                    // (exchanges 300, 700, 1100, ...) so the regression
                    // can assert the exact restart points. The handler
                    // restarts B; A wakes with PartnerFaulted and retries
                    // the same n, which the fresh B receives and answers.
                    B_EXCHANGES = B_EXCHANGES.wrapping_add(1);
                    if B_EXCHANGES % 400 == 300 {
                        fault_inject();
                    }
                    let rlen = LAST_PING_LEN;
                    let mut pong = [0u8; MSG_MAX];
                    pong[..4].copy_from_slice(b"pong");
                    pong[4..rlen].copy_from_slice(&LAST_PING[4..rlen]);
                    match notify(TASK_A, 1) {
                        NotifyResult::Notified => {}
                        NotifyResult::Failed(e) => panic!("task B: notify failed: {:?}", e),
                    }
                    match reply(&pong[..rlen]) {
                        ReplyResult::Replied => PHASE_B = PhaseB::Recv,
                        ReplyResult::Failed(e) => panic!("task B: reply failed: {:?}", e),
                    }
                }
            }
        }
        crate::sched::yield_now!();
    }
}
