//! v0.4 "ipc": synchronous rendezvous IPC + notifications.
//!
//! seL4-style, minus the kernel-mode switch: a `send` completes if and
//! only if a receiver takes the message — the kernel copies it directly
//! sender → receiver. No queues, no heap, no allocator: each task owns
//! one static [`MSG_MAX`]-byte staging buffer, and at most one
//! outstanding blocking operation.
//!
//! Cooperative blocking (see `kernel/USERSPACE.md` §5): a syscall that
//! cannot rendezvous immediately registers the operation, marks the task
//! `Blocked`, and returns `Blocked`. The task must return to the
//! scheduler; when it runs again it re-invokes the same call, which then
//! returns the completed result. The scheduler only runs `Ready` tasks,
//! so a blocked task consumes no CPU.
//!
//! Soundness: all task state lives in `static mut` storage touched only
//! with interrupts disabled and never reentrantly (the kernel never
//! enables interrupts in v0.4, and syscalls never block inside
//! themselves). The public syscalls are therefore safe functions with
//! `unsafe` interiors.

use core::cmp::min;

use crate::task::{task_ptr, TaskState};

// ---------------------------------------------------------------------------
// Userspace contract (mirrors kernel/USERSPACE.md exactly)
// ---------------------------------------------------------------------------

/// Maximum message size in bytes.
pub const MSG_MAX: usize = 64;
/// Endpoint IDs are `0..N_ENDPOINTS`.
pub const N_ENDPOINTS: u8 = 8;
/// Demo endpoint: task A <-> task B ping-pong.
pub const EP_PING: u8 = 0;

/// Task IDs (indices into the static task table). Part of the userspace
/// ABI (`kernel/USERSPACE.md`); the current demo tasks don't use every
/// constant, which is fine — tasks are not required to use every call.
pub const TASK_A: usize = 0;
#[allow(dead_code)]
pub const TASK_B: usize = 1;
pub const N_TASKS: usize = 2;

/// Immediate (non-blocking) IPC errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// Endpoint ID `>= N_ENDPOINTS`.
    BadEndpoint,
    /// Notify target `>= N_TASKS`.
    BadTask,
    /// Message longer than [`MSG_MAX`].
    Oversize,
    /// Another blocking call is still outstanding on this task.
    Busy,
    /// `reply()` with nobody waiting for a reply.
    NoPartner,
    /// Rendezvous partner died before completing (v0.5 faults).
    PartnerFaulted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // userspace ABI (USERSPACE.md); the demo uses call/reply
pub enum SendResult {
    Delivered,
    Blocked,
    Failed(IpcError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvResult {
    Received(usize),
    Blocked,
    Failed(IpcError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallResult {
    Replied(usize),
    Blocked,
    Failed(IpcError),
}

/// `reply` never blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyResult {
    Replied,
    Failed(IpcError),
}

/// `notify` never blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyResult {
    Notified,
    Failed(IpcError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResult {
    Signaled(u32),
    Blocked,
    Failed(IpcError),
}

// ---------------------------------------------------------------------------
// Outstanding-operation machinery (kernel-internal)
// ---------------------------------------------------------------------------

/// What kind of blocking operation a task has outstanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Send,
    Recv,
    Call,
    Wait,
}

/// One task's outstanding blocking operation, if any.
#[derive(Debug, Clone, Copy)]
pub struct PendingOp {
    pub kind: OpKind,
    /// For `Call`: 0 = send phase, 1 = awaiting reply.
    pub phase: u8,
    pub ep: u8,
    pub done: bool,
    pub err: Option<IpcError>,
}

impl PendingOp {
    const fn new(kind: OpKind, ep: u8) -> Self {
        PendingOp {
            kind,
            phase: 0,
            ep,
            done: false,
            err: None,
        }
    }
}

/// The task invoking this syscall (set by the scheduler per slice).
fn me() -> usize {
    unsafe { crate::sched::current_task() }
}

/// Take a completed operation of kind `kind`: clear it, mark the task
/// ready, and return `Some(err)` — or `None` if it isn't done.
unsafe fn reap(tp: *mut crate::task::Task, kind: OpKind) -> Option<Option<IpcError>> {
    unsafe {
        match (*tp).op {
            Some(op) if op.kind == kind && op.done => {
                (*tp).op = None;
                (*tp).state = TaskState::Ready;
                Some(op.err)
            }
            _ => None,
        }
    }
}

/// Copy the sender's staged message into the receiver's staging buffer.
unsafe fn stage_copy(from: *mut crate::task::Task, to: *mut crate::task::Task) {
    unsafe {
        let len = (*from).msg_len as usize;
        (&mut (*to).msg)[..len].copy_from_slice(&(&(*from).msg)[..len]);
        (*to).msg_len = len as u8;
    }
}

/// Stage an outgoing message into task `t`'s buffer (`data.len() <= MSG_MAX`).
unsafe fn stage_write(t: *mut crate::task::Task, data: &[u8]) {
    unsafe {
        let len = data.len();
        (&mut (*t).msg)[..len].copy_from_slice(data);
        (*t).msg_len = len as u8;
    }
}

/// Copy task `t`'s staged message into `out`; returns bytes copied.
unsafe fn stage_read(t: *mut crate::task::Task, out: &mut [u8]) -> usize {
    unsafe {
        let n = min((*t).msg_len as usize, out.len());
        out[..n].copy_from_slice(&(&(*t).msg)[..n]);
        n
    }
}

/// Perform the rendezvous between sender `s` and receiver `r`.
///
/// Completes `r`'s `Recv` immediately. A plain `Send` completes too; a
/// `Call` advances to the reply-wait phase and records the partnership
/// both ways (`reply_to` / `reply_from`) so `reply` and v0.5's
/// `PartnerFaulted` can find it.
unsafe fn rendezvous(s: usize, r: usize) {
    unsafe {
        let sp = task_ptr(s);
        let rp = task_ptr(r);
        stage_copy(sp, rp);
        if let Some(op) = (*rp).op {
            (*rp).op = Some(PendingOp { done: true, ..op });
        }
        (*rp).state = TaskState::Ready;
        match (*sp).op {
            Some(op) if op.kind == OpKind::Send => {
                (*sp).op = Some(PendingOp { done: true, ..op });
                (*sp).state = TaskState::Ready;
            }
            Some(op) if op.kind == OpKind::Call => {
                (*sp).op = Some(PendingOp { phase: 1, ..op });
                (*rp).reply_to = Some(s);
                (*sp).reply_from = Some(r);
                // sender stays Blocked, awaiting the reply
            }
            _ => {}
        }
    }
}

/// Sender `s` (op `Send`, or `Call` in send phase): find a task blocked
/// in `Recv` on the same endpoint and rendezvous.
unsafe fn match_sender(s: usize) {
    unsafe {
        let ep = match (*task_ptr(s)).op {
            Some(op) => op.ep,
            None => return,
        };
        let mut r = 0;
        while r < N_TASKS {
            if r != s {
                let rp = task_ptr(r);
                if (*rp).state == TaskState::Blocked {
                    if let Some(op) = (*rp).op {
                        if op.kind == OpKind::Recv && !op.done && op.ep == ep {
                            rendezvous(s, r);
                            return;
                        }
                    }
                }
            }
            r += 1;
        }
    }
}

/// Receiver `r` (op `Recv`): find a task blocked in `Send` — or in a
/// `Call`'s send phase — on the same endpoint and rendezvous.
unsafe fn match_receiver(r: usize) {
    unsafe {
        let ep = match (*task_ptr(r)).op {
            Some(op) => op.ep,
            None => return,
        };
        let mut s = 0;
        while s < N_TASKS {
            if s != r {
                let sp = task_ptr(s);
                if (*sp).state == TaskState::Blocked {
                    if let Some(op) = (*sp).op {
                        let sending =
                            op.kind == OpKind::Send || (op.kind == OpKind::Call && op.phase == 0);
                        if sending && !op.done && op.ep == ep {
                            rendezvous(s, r);
                            return;
                        }
                    }
                }
            }
            s += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Syscalls
// ---------------------------------------------------------------------------

/// Blocking send: rendezvous with a task blocked in `recv` on `ep`.
///
/// Either completes immediately (`Delivered`), registers and returns
/// `Blocked` (re-invoke to reap `Delivered`), or fails immediately.
///
/// Part of the userspace ABI; the current demo tasks use `call`/`reply`.
#[allow(dead_code)]
pub fn send(ep: u8, msg: &[u8]) -> SendResult {
    if ep >= N_ENDPOINTS {
        return SendResult::Failed(IpcError::BadEndpoint);
    }
    if msg.len() > MSG_MAX {
        return SendResult::Failed(IpcError::Oversize);
    }
    unsafe {
        let tp = task_ptr(me());
        match (*tp).op {
            None => {}
            Some(op) if op.kind == OpKind::Send => {
                return match reap(tp, OpKind::Send) {
                    Some(None) => SendResult::Delivered,
                    Some(Some(e)) => SendResult::Failed(e),
                    None => SendResult::Blocked,
                };
            }
            Some(_) => return SendResult::Failed(IpcError::Busy),
        }
        stage_write(tp, msg);
        (*tp).op = Some(PendingOp::new(OpKind::Send, ep));
        match_sender(me());
        match reap(tp, OpKind::Send) {
            Some(None) => SendResult::Delivered,
            Some(Some(e)) => SendResult::Failed(e),
            None => {
                (*tp).state = TaskState::Blocked;
                SendResult::Blocked
            }
        }
    }
}

/// Blocking receive: rendezvous with a task blocked in `send`/`call` on
/// `ep`. On `Received(n)`, `out[..n]` holds the message.
pub fn recv(ep: u8, out: &mut [u8]) -> RecvResult {
    if ep >= N_ENDPOINTS {
        return RecvResult::Failed(IpcError::BadEndpoint);
    }
    unsafe {
        let tp = task_ptr(me());
        match (*tp).op {
            None => {}
            Some(op) if op.kind == OpKind::Recv => {
                return match reap(tp, OpKind::Recv) {
                    Some(None) => {
                        let n = stage_read(tp, out);
                        RecvResult::Received(n)
                    }
                    Some(Some(e)) => RecvResult::Failed(e),
                    None => RecvResult::Blocked,
                };
            }
            Some(_) => return RecvResult::Failed(IpcError::Busy),
        }
        (*tp).op = Some(PendingOp::new(OpKind::Recv, ep));
        match_receiver(me());
        match reap(tp, OpKind::Recv) {
            Some(None) => {
                let n = stage_read(tp, out);
                RecvResult::Received(n)
            }
            Some(Some(e)) => RecvResult::Failed(e),
            None => {
                (*tp).state = TaskState::Blocked;
                RecvResult::Blocked
            }
        }
    }
}

/// Blocking call: `send` then wait for the reply, as one operation.
/// On `Replied(n)`, `reply_out[..n]` holds the reply.
pub fn call(ep: u8, msg: &[u8], reply_out: &mut [u8]) -> CallResult {
    if ep >= N_ENDPOINTS {
        return CallResult::Failed(IpcError::BadEndpoint);
    }
    if msg.len() > MSG_MAX {
        return CallResult::Failed(IpcError::Oversize);
    }
    unsafe {
        let tp = task_ptr(me());
        match (*tp).op {
            None => {}
            Some(op) if op.kind == OpKind::Call => {
                return match reap(tp, OpKind::Call) {
                    Some(None) => {
                        (*tp).reply_from = None;
                        let n = stage_read(tp, reply_out);
                        CallResult::Replied(n)
                    }
                    Some(Some(e)) => {
                        (*tp).reply_from = None;
                        CallResult::Failed(e)
                    }
                    None => CallResult::Blocked,
                };
            }
            Some(_) => return CallResult::Failed(IpcError::Busy),
        }
        stage_write(tp, msg);
        (*tp).op = Some(PendingOp::new(OpKind::Call, ep));
        match_sender(me());
        // A call is never done after its send phase alone: either the
        // reply arrived (impossible here — nobody could have replied
        // yet) or we block awaiting it.
        (*tp).state = TaskState::Blocked;
        CallResult::Blocked
    }
}

/// Answer the outstanding `call` this task received. Never blocks.
///
/// Only valid after this task's `recv` rendezvoused with a `call` (not a
/// plain `send`); otherwise `Failed(NoPartner)`. If the caller is no
/// longer waiting (v0.5: it faulted), `Failed(PartnerFaulted)`.
pub fn reply(msg: &[u8]) -> ReplyResult {
    if msg.len() > MSG_MAX {
        return ReplyResult::Failed(IpcError::Oversize);
    }
    unsafe {
        let mp = task_ptr(me());
        let target = match (*mp).reply_to {
            Some(t) => t,
            None => return ReplyResult::Failed(IpcError::NoPartner),
        };
        (*mp).reply_to = None;
        let tp = task_ptr(target);
        match (*tp).op {
            Some(op) if op.kind == OpKind::Call && op.phase == 1 && !op.done => {
                stage_write(tp, msg);
                (*tp).op = Some(PendingOp { done: true, ..op });
                (*tp).state = TaskState::Ready;
                (*tp).reply_from = None;
                ReplyResult::Replied
            }
            _ => ReplyResult::Failed(IpcError::PartnerFaulted),
        }
    }
}

/// Non-blocking signal: OR `bits` into `task`'s pending notification word,
/// waking it if it is blocked in `wait`. Never blocks, never lost.
pub fn notify(task: usize, bits: u32) -> NotifyResult {
    if task >= N_TASKS {
        return NotifyResult::Failed(IpcError::BadTask);
    }
    unsafe {
        let tp = task_ptr(task);
        (*tp).pending |= bits;
        if (*tp).state == TaskState::Blocked {
            if let Some(op) = (*tp).op {
                if op.kind == OpKind::Wait && !op.done {
                    (*tp).op = Some(PendingOp { done: true, ..op });
                    (*tp).state = TaskState::Ready;
                }
            }
        }
        NotifyResult::Notified
    }
}

/// Blocking wait: take pending notification bits, or block until
/// `notify`ed. The only error is `Busy` (another call outstanding).
pub fn wait() -> WaitResult {
    unsafe {
        let tp = task_ptr(me());
        if (*tp).pending != 0 {
            let bits = (*tp).pending;
            (*tp).pending = 0;
            if let Some(op) = (*tp).op {
                if op.kind == OpKind::Wait {
                    (*tp).op = None;
                }
            }
            (*tp).state = TaskState::Ready;
            return WaitResult::Signaled(bits);
        }
        match (*tp).op {
            None => {}
            Some(op) if op.kind == OpKind::Wait => return WaitResult::Blocked,
            Some(_) => return WaitResult::Failed(IpcError::Busy),
        }
        (*tp).op = Some(PendingOp::new(OpKind::Wait, 0));
        (*tp).state = TaskState::Blocked;
        WaitResult::Blocked
    }
}

// ---------------------------------------------------------------------------
// v0.5 fault recovery (kernel-internal)
// ---------------------------------------------------------------------------

/// Reset task `idx`'s IPC state for a post-fault restart, and wake any
/// partner blocked on it with `PartnerFaulted`.
///
/// - The faulted task: outstanding op cleared, marked `Ready`, pending
///   notifications cleared, reply links cleared. It restarts from its
///   entry point with no memory of the interrupted operation.
/// - A task blocked in `Call` phase 1 (awaiting a reply) with
///   `reply_from == idx`: its operation is completed with
///   `Failed(PartnerFaulted)` and it is woken. Re-invoking `call`
///   reports the error; the task can then retry.
/// - A task with `reply_to == idx` (it received the dead task's call and
///   owes a reply): the link is cleared, so its `reply()` honestly
///   reports `NoPartner`.
/// - Phase-0 `Call`/`Send`, `Recv`, and `Wait` ops are left alone: the
///   restarted task re-enters the rendezvous protocol from its entry
///   point, so a pending send/recv simply rendezvouses again — this is
///   the restart-tolerant path (`kernel/USERSPACE.md` §8).
///
/// # Safety
///
/// `idx < N_TASKS`. The faulted task must not be running.
pub(crate) unsafe fn reset_for_restart(faulted: usize) {
    unsafe {
        let fp = task_ptr(faulted);
        (*fp).op = None;
        (*fp).state = TaskState::Ready;
        (*fp).pending = 0;
        (*fp).reply_to = None;
        (*fp).reply_from = None;

        let mut t = 0;
        while t < N_TASKS {
            if t != faulted {
                let tp = task_ptr(t);
                if let Some(op) = (*tp).op {
                    let awaiting_reply = op.kind == OpKind::Call
                        && op.phase == 1
                        && !op.done
                        && (*tp).reply_from == Some(faulted);
                    if awaiting_reply {
                        (*tp).op = Some(PendingOp {
                            done: true,
                            err: Some(IpcError::PartnerFaulted),
                            ..op
                        });
                        (*tp).state = TaskState::Ready;
                    }
                }
                if (*tp).reply_to == Some(faulted) {
                    (*tp).reply_to = None;
                }
            }
            t += 1;
        }
    }
}
