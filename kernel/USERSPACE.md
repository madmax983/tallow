# Tallow Userspace Contract

This document is the contract between the Tallow kernel and its tasks.
The Rust API in `src/ipc.rs` implements this document exactly; where they
could disagree, this document wins and the code is wrong.

Status: **v0.4 "ipc"** — synchronous rendezvous IPC + notifications.
The *v0.5 preview* section at the end is normative for v0.5 and
informational for v0.4 (the `PartnerFaulted` path exists in the API but
cannot trigger until faults exist).

## 1. Task model

- Tasks are **static**. There is no dynamic task creation: the task table
  in `src/task.rs` is fixed at compile time. Each task is an
  `extern "C" fn() -> !` infinite loop plus a private, statically
  allocated 4 KiB stack.
- Scheduling is **cooperative**. The 1 kHz polled tick runs every `Ready`
  task once per tick, in task-index order. A task runs a *slice* of work
  and then yields explicitly through the symmetric coroutine switch
  (`sched::yield_now!()`); the yield preserves the task's full register
  state, so it resumes after the yield with locals and call depth intact.
  Yielding is always explicit — never a return, never preemption.
- A task is always in exactly one state: `Ready` or `Blocked`. The
  scheduler runs only `Ready` tasks. There is no preemption, no time
  slicing inside a tick, and no priorities in v0.4.
- Task IDs are small integers: `TASK_A = 0`, `TASK_B = 1`
  (`src/ipc.rs`). The idle task is the scheduler loop itself, not a task.

## 2. Endpoints

- Endpoints are **static consts**, `u8` IDs in `0..N_ENDPOINTS`
  (`N_ENDPOINTS = 8`). They are rendezvous points, not queues: nothing is
  ever stored *in* an endpoint.
- `EP_PING = 0`: the demo ping-pong endpoint between tasks A and B.
  Endpoints 1–7 are reserved for future use.

## 3. Messages

- A message is a byte string, `&[u8]`, of at most `MSG_MAX = 64` bytes.
- Messages are **copied directly sender → receiver** at rendezvous. There
  are no queues, no heap, no allocator: each task owns one static
  64-byte staging buffer inside the kernel.
- On receive, the kernel copies `min(message length, buffer length)` bytes
  and reports how many it copied. Size your buffers `MSG_MAX`.

## 4. Calls

```rust
pub fn send(ep: u8, msg: &[u8]) -> SendResult;
pub fn recv(ep: u8, out: &mut [u8]) -> RecvResult;
pub fn call(ep: u8, msg: &[u8], reply_out: &mut [u8]) -> CallResult;
pub fn reply(msg: &[u8]) -> ReplyResult;
pub fn notify(task: usize, bits: u32) -> NotifyResult;
pub fn wait() -> WaitResult;
```

```rust
pub enum SendResult   { Delivered, Blocked, Failed(IpcError) }
pub enum RecvResult   { Received(usize), Blocked, Failed(IpcError) }
pub enum CallResult   { Replied(usize), Blocked, Failed(IpcError) }
pub enum ReplyResult  { Replied, Failed(IpcError) }   // never blocks
pub enum NotifyResult { Notified, Failed(IpcError) }  // never blocks
pub enum WaitResult   { Signaled(u32), Blocked, Failed(IpcError) }
```

- `send(ep, msg)`: rendezvous with a task blocked in `recv` on `ep`.
  Completes when the receiver takes the message.
- `recv(ep, out)`: rendezvous with a task blocked in `send`/`call` on
  `ep`. Completes with the message length when a sender arrives.
- `call(ep, msg, reply_out)`: `send` followed by waiting for a reply —
  one atomic operation from the caller's view. The receiver gets the
  message via its own `recv`, then answers with `reply`. Completes with
  the reply length.
- `reply(msg)`: answer the outstanding `call` that this task received.
  Only valid after this task's `recv` rendezvoused with a `call` (not a
  plain `send`); otherwise `Failed(NoPartner)`. Never blocks.
- `notify(task, bits)`: OR `bits` into `task`'s pending notification
  word and wake it if it is blocked in `wait`. Never blocks.
- `wait()`: if any notification bits are pending, take and return them;
  otherwise block until `notify`ed. The only error is `Busy`.

### Error model

```rust
pub enum IpcError {
    BadEndpoint,    // ep >= N_ENDPOINTS
    BadTask,        // notify target >= N_TASKS
    Oversize,       // msg.len() > MSG_MAX (64)
    Busy,           // another blocking call is still outstanding
    NoPartner,      // reply() with nobody waiting for a reply
    PartnerFaulted, // rendezvous partner died (v0.5; see below)
}
```

Errors are **immediate**: they are returned without blocking. A task bug
(wrong endpoint, oversize message, reply without partner) never hangs the
kernel — it gets an error value. A *kernel* bug (unreachable protocol
state in the demo tasks) panics, which halts; that is deliberate: a panic
is a kernel bug, full stop.

## 5. Blocking semantics (cooperative)

This is the heart of the contract. The coroutine switch preserves a
task's full register state across yields, so a task *could* suspend
mid-function — but the blocking syscalls below don't: they follow a
register-and-reap protocol instead, which keeps every task a small,
auditable state machine:

1. **One outstanding blocking call per task.** `send`/`recv`/`call`/
   `wait` either complete immediately (rendezvous partner already
   waiting) or register the operation and return `Blocked`.
2. **`Blocked` means: yield back to the scheduler NOW** (via
   `sched::yield_now!()`). The kernel guarantees the task will not run
   again until the operation completes (or fails). A task that keeps
   executing after `Blocked` is a task bug; the kernel will keep
   reporting `Blocked` for that operation.
3. **Completion is observed by re-invoking the same call.** When the task
   runs again, it calls the same function again; the kernel then returns
   the completed result (`Delivered`, `Received(n)`, `Replied(n)`,
   `Signaled(bits)`) — or `Failed(e)`. Arguments on the reaping call are
   ignored, except that argument *validation* still applies.
4. Invoking a *different* blocking call while one is outstanding returns
   `Failed(Busy)`. Finish or reap what you started first.

The yield must happen at the top level of the task's loop, with no
calls outstanding: the switch declares every non-current window dead
(see `src/sched.rs`), so yielding from inside a nested call would strand
the caller's window. One syscall per slice, then yield at the top.

The intended shape of a task is therefore a small state machine, one
syscall per slice:

```rust
static mut PHASE: Phase = Phase::Ask;
static mut N: u32 = 0;

extern "C" fn task_a() -> ! {
    loop {
        unsafe {
            match PHASE {
                Phase::Ask => match ipc::call(EP_PING, &ping_msg(N), &mut rbuf) {
                    CallResult::Replied(_) => { N += 1; PHASE = Phase::Wait; }
                    CallResult::Blocked => {}            // yield; we run again later
                    CallResult::Failed(e) => panic!("A: {:?}", e),
                },
                Phase::Wait => match ipc::wait() {
                    WaitResult::Signaled(_) => PHASE = Phase::Ask,
                    WaitResult::Blocked => {}
                    WaitResult::Failed(e) => panic!("A: {:?}", e),
                },
            }
        }
        sched::yield_now!(); // one syscall per slice, then yield at the top
    }
}
```

## 6. Notifications

- Each task owns a `u32` pending word. `notify` is level-triggered and
  never lost: bits OR in and stay until `wait` takes them.
- `wait` returns immediately if bits are already pending. There is no
  race between "check then block": the check and the block are one call.
- Convention (not enforced): bit `n` of a task's word belongs to
  endpoint `n`-ish usage; the demo uses bit 0 as "reply sent".

## 7. What tasks may rely on

- Rendezvous is **pairwise and unbuffered**: a `send` completes if and
  only if a receiver took the message. No message is ever duplicated,
  reordered, or silently dropped.
- A task blocked in `send`/`recv`/`call`/`wait` consumes no CPU: the
  scheduler skips it until it is woken.
- `reply` always reaches the task whose `call` you received — calls are
  not anonymous to the replier.
- Everything is bounded and static: at most one outstanding op per task,
  64-byte messages, 8 endpoints. If it can't be bounded at build time, it
  doesn't exist.

## 8. v0.5 — faults (implemented)

- If a task faults (e.g. an MPU violation), the kernel **kills and
  restarts** it: the faulting task is reset to its entry point with fresh
  IPC state (no outstanding op, empty notification word, no reply
  partner); the kernel and all other tasks are unaffected.
- Any task blocked in IPC **with the faulting task as partner** has its
  operation completed with `Failed(PartnerFaulted)` and is woken. (The
  only partner-tracked operation is `call` awaiting a reply.)
- **Tasks must be written restart-tolerant.** Keep protocol state such
  that a retried operation is harmless: the demo ping-pong carries the
  sequence number *in the message* (`ping {n}` → `pong {n}`), so if task
  B restarts mid-exchange, task A's retry of `n` simply gets answered
  again — no sequence gap, no desync. A task that cannot tolerate its
  partner restarting is a task bug.
- The kernel counts restarts per task and reports them on the console
  (`[fault] task 1: <reason>; restart #k`). There is no restart backoff
  in v0.5; a task that faults in a tight loop will visibly spin its
  restart counter, which is the honest signal.
- **QEMU gap (honest):** QEMU's ESP32-S3 model does not enforce the PMS
  — PMS register writes are accepted but never enforced — so a real MPU
  violation cannot be produced in QEMU. Worse, the ROM exception vectors
  are read-only (verified: VECBASE=0x40000000, KExc=VECBASE+0x300, writes
  do not stick), so a genuine CPU exception cannot be hooked in QEMU.
  The fault path is verified by deterministic synthetic fault injection:
  task B explicitly requests the kernel's fault path at fixed exchange
  numbers (300, 700, 1100, ...); the kill, restart, `PartnerFaulted`
  wake, and gap-free retry are all real. On real hardware, the PMS must
  be programmed per the ESP32-S3 Technical Reference Manual and a
  proper exception vector installed — that hardware path is documented
  but not implemented or tested in v0.5.
