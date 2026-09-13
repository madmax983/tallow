//! v0.6 "drivers": static tasks plus Tock-style driver capsules.
//!
//! Six tasks: A and B hold the v0.4/v0.5 ping-pong conversation; C is a
//! blink driver; GPIO, LED, and UART are driver capsules. Each capsule
//! owns one peripheral exclusively — every access goes through
//! synchronous IPC to its endpoint (`kernel/USERSPACE.md` §9):
//!
//! - GPIO (`EP_GPIO`): owns the GPIO peripheral. Configure/set/clear/
//!   toggle/read, with a software shadow of direction and level.
//! - LED (`EP_LED`): owns no hardware; a *client* of the GPIO capsule
//!   (the layering demonstration: C → LED → GPIO, all IPC). Drives the
//!   LED pin and logs `[led] on|off` through the UART capsule.
//! - UART (`EP_UART`): owns UART0. All task output flows through it.
//!
//! The kernel itself (scheduler, banner, panic) is not an IPC client
//! and still writes UART0 directly — see USERSPACE.md §9 for why.
//! Scheduling is unchanged: cooperative round-robin, one slice per
//! Ready task per 1 kHz polled tick, explicit `yield_now!()` at the top
//! of each task's loop.

use crate::ipc::{
    CallResult, IpcError, NotifyResult, PendingOp, RecvResult, ReplyResult, WaitResult, EP_GPIO,
    EP_LED, EP_PING, EP_UART, MSG_MAX, N_TASKS, TASK_A,
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
/// Task C's private stack (blink driver).
static mut STACK_C: [u32; STACK_WORDS] = [0; STACK_WORDS];
/// GPIO capsule's private stack.
static mut STACK_GPIO: [u32; STACK_WORDS] = [0; STACK_WORDS];
/// LED capsule's private stack.
static mut STACK_LED: [u32; STACK_WORDS] = [0; STACK_WORDS];
/// UART capsule's private stack.
static mut STACK_UART: [u32; STACK_WORDS] = [0; STACK_WORDS];

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

/// The task table. Fixed at six: A, B, C (blink driver), and the
/// GPIO, LED, and UART capsules. The scheduler loop itself is the idle
/// task (it polls the timer and prints the heartbeat).
pub static mut TASKS: [Task; N_TASKS] = [
    blank_task(task_a, "A"),
    blank_task(task_b, "B"),
    blank_task(task_c, "C"),
    blank_task(task_gpio, "GPIO"),
    blank_task(task_led, "LED"),
    blank_task(task_uart, "UART"),
];

/// Raw pointer to task `idx`'s state.
///
/// `addr_of_mut!` (not a `&mut` to a `static mut`): taking a reference
/// is UB-adjacent and trips `static_mut_refs`. Callers must uphold the
/// kernel's single-threaded discipline (interrupts never enabled).
pub fn task_ptr(idx: usize) -> *mut Task {
    debug_assert!(idx < N_TASKS);
    unsafe { core::ptr::addr_of_mut!(TASKS[idx]) }
}

/// Stack top (16-byte aligned, rounded down) for task `idx`'s stack.
fn stack_top_for(idx: usize) -> u32 {
    let base = match idx {
        0 => core::ptr::addr_of!(STACK_A) as u32,
        1 => core::ptr::addr_of!(STACK_B) as u32,
        2 => core::ptr::addr_of!(STACK_C) as u32,
        3 => core::ptr::addr_of!(STACK_GPIO) as u32,
        4 => core::ptr::addr_of!(STACK_LED) as u32,
        _ => core::ptr::addr_of!(STACK_UART) as u32,
    };
    // The Xtensa windowed ABI needs 16-byte-aligned SP (`entry` faults
    // otherwise); the stacks are only 4-aligned, so round each top
    // *down* to 16 bytes — the stack grows down, so this just costs up
    // to 12 bytes of each 4 KiB stack.
    (base + STACK_BYTES) & !0xF
}

/// Initialize the task table: point each task at its stack and craft its
/// initial coroutine context (first resume jumps to the entry point).
///
/// Must be called once, before the scheduler runs. Interrupts are off
/// (we never enable them), so the `static mut` writes are safe.
pub unsafe fn init() {
    unsafe {
        let mut idx = 0;
        while idx < N_TASKS {
            let top = stack_top_for(idx);
            debug_assert!(top & 0xF == 0);
            (*task_ptr(idx)).stack_top = top;
            crate::sched::init_task_context(idx, task_entry_addr(idx), top);
            idx += 1;
        }
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
    /// The exchange completed; emit the `[ipc NNNN]` line through the
    /// UART capsule, then wait for B's notification.
    Print,
    Wait,
}

static mut PHASE_A: PhaseA = PhaseA::Call;
static mut COUNT_A: u32 = 0;

/// Write `prefix` + `n` in decimal, zero-padded to *at least* 4 digits,
/// into `buf`. Returns the number of bytes written. `buf` must fit
/// prefix + 10 digits.
fn tag4(buf: &mut [u8], prefix: &[u8], n: u32) -> usize {
    let mut len = 0;
    for &b in prefix {
        buf[len] = b;
        len += 1;
    }
    let mut div = 1000u32;
    while div <= n / 10 {
        div *= 10;
    }
    while div > 0 {
        buf[len] = b'0' + ((n / div) % 10) as u8;
        len += 1;
        div /= 10;
    }
    len
}

/// Task A: the initiator. `call`s B with `ping {n}`, prints one
/// `[ipc NNNN] ping -> pong` line per completed exchange *through the
/// UART capsule*, then `wait`s for B's "reply sent" notification.
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
                            PHASE_A = PhaseA::Print;
                        }
                        CallResult::Blocked => {}
                        // v0.5: the partner died mid-call; retry the same n —
                        // the sequence number rides in the message, so the
                        // retry is answered again with no gap.
                        CallResult::Failed(IpcError::PartnerFaulted) => {}
                        CallResult::Failed(e) => panic!("task A: call failed: {:?}", e),
                    }
                }
                PhaseA::Print => {
                    // v0.6: tasks never touch uart::Writer; the line goes
                    // through the UART capsule. The retry is safe: a
                    // PartnerFaulted means no reply was sent, so the line
                    // was not printed.
                    let n = COUNT_A;
                    let mut line = [0u8; MSG_MAX];
                    let mut llen = tag4(&mut line, b"[ipc ", n);
                    for &b in b"] ping -> pong\n" {
                        line[llen] = b;
                        llen += 1;
                    }
                    let mut rbuf = [0u8; MSG_MAX];
                    match call(EP_UART, &line[..llen], &mut rbuf) {
                        CallResult::Replied(_) => {
                            COUNT_A = n.wrapping_add(1);
                            PHASE_A = PhaseA::Wait;
                        }
                        CallResult::Blocked => {}
                        CallResult::Failed(IpcError::PartnerFaulted) => {}
                        CallResult::Failed(e) => panic!("task A: uart call failed: {:?}", e),
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

// ---------------------------------------------------------------------------
// v0.6 "drivers": Tock-style capsules
// ---------------------------------------------------------------------------

// ESP32-S3 GPIO hardware access (Technical Reference Manual: GPIO base
// 0x6000_4000, IO MUX base 0x6000_8000). Owned exclusively by the GPIO
// capsule below — no other code in the kernel touches these registers.
mod gpio_hw {
    use core::ptr::{read_volatile, write_volatile};

    const GPIO_BASE: usize = 0x6000_4000;
    const GPIO_OUT_W1TS: *mut u32 = (GPIO_BASE + 0x04) as *mut u32;
    const GPIO_OUT_W1TC: *mut u32 = (GPIO_BASE + 0x08) as *mut u32;
    const GPIO_ENABLE_W1TS: *mut u32 = (GPIO_BASE + 0x24) as *mut u32;
    const GPIO_ENABLE_W1TC: *mut u32 = (GPIO_BASE + 0x28) as *mut u32;
    const GPIO_IN: *const u32 = (GPIO_BASE + 0x3C) as *const u32;
    const IOMUX_BASE: usize = 0x6000_8000;
    /// `IO_MUX_GPIOx_REG`: `MCU_SEL` field [14:12] = 1 selects the GPIO
    /// function; `FUN_IE` bit 9 enables the input path.
    const MCU_SEL_GPIO: u32 = 1 << 12;
    const FUN_IE: u32 = 1 << 9;

    pub fn configure(pin: u8, output: bool) {
        let iomux = (IOMUX_BASE + 4 * pin as usize) as *mut u32;
        let mask = 1u32 << pin;
        unsafe {
            if output {
                write_volatile(iomux, MCU_SEL_GPIO);
                write_volatile(GPIO_ENABLE_W1TS, mask);
                write_volatile(GPIO_OUT_W1TC, mask); // drive low
            } else {
                write_volatile(iomux, MCU_SEL_GPIO | FUN_IE);
                write_volatile(GPIO_ENABLE_W1TC, mask);
            }
        }
    }

    pub fn set(pin: u8) {
        unsafe { write_volatile(GPIO_OUT_W1TS, 1u32 << pin) };
    }

    pub fn clear(pin: u8) {
        unsafe { write_volatile(GPIO_OUT_W1TC, 1u32 << pin) };
    }

    pub fn read_input(pin: u8) -> u8 {
        unsafe { ((read_volatile(GPIO_IN) >> pin) & 1) as u8 }
    }
}

/// GPIO capsule state: software shadow of pin direction (1 = output)
/// and output level, one bit per pin. The shadow makes toggle/read
/// atomic w.r.t. read-modify-write and independent of MMIO read-back —
/// which QEMU does not implement (see `kernel/USERSPACE.md` §9.5).
static mut GPIO_DIRS: u32 = 0;
static mut GPIO_LEVELS: u32 = 0;

/// Status codes for the GPIO capsule (`kernel/USERSPACE.md` §9.1).
const GPIO_OK: u8 = 0;
const GPIO_BAD_PIN: u8 = 1;
const GPIO_BAD_REQ: u8 = 2;

/// Handle one `EP_GPIO` request. Returns the reply bytes and their
/// length (1 or 2). Never touches the scheduler — pure decode + MMIO.
fn gpio_handle(req: &[u8]) -> ([u8; 2], usize) {
    if req.is_empty() {
        return ([GPIO_BAD_REQ, 0], 1);
    }
    let op = req[0];
    let ok_len = match op {
        b'C' => req.len() == 3,
        b'S' | b'c' | b'T' | b'R' => req.len() == 2,
        _ => false,
    };
    if !ok_len {
        return ([GPIO_BAD_REQ, 0], 1);
    }
    let pin = req[1];
    if pin > 31 {
        // Only the low GPIO bank is modeled (no OUT1/IN1/ENABLE1 yet).
        return ([GPIO_BAD_PIN, 0], 1);
    }
    let bit = 1u32 << pin;
    unsafe {
        match op {
            b'C' => {
                let dir = req[2];
                if dir > 1 {
                    ([GPIO_BAD_REQ, 0], 1)
                } else {
                    gpio_hw::configure(pin, dir == 1);
                    if dir == 1 {
                        GPIO_DIRS |= bit;
                        GPIO_LEVELS &= !bit; // outputs drive low
                    } else {
                        GPIO_DIRS &= !bit;
                    }
                    ([GPIO_OK, 0], 1)
                }
            }
            b'S' => {
                gpio_hw::set(pin);
                GPIO_LEVELS |= bit;
                ([GPIO_OK, 0], 1)
            }
            b'c' => {
                gpio_hw::clear(pin);
                GPIO_LEVELS &= !bit;
                ([GPIO_OK, 0], 1)
            }
            b'T' => {
                let level = if GPIO_LEVELS & bit != 0 {
                    gpio_hw::clear(pin);
                    GPIO_LEVELS &= !bit;
                    0
                } else {
                    gpio_hw::set(pin);
                    GPIO_LEVELS |= bit;
                    1
                };
                ([GPIO_OK, level], 2)
            }
            // b'R': outputs read the shadow, inputs read the hardware.
            _ => {
                let level = if GPIO_DIRS & bit != 0 {
                    ((GPIO_LEVELS >> pin) & 1) as u8
                } else {
                    gpio_hw::read_input(pin)
                };
                ([GPIO_OK, level], 2)
            }
        }
    }
}

/// GPIO capsule: owns the GPIO peripheral. A pure server — `recv` →
/// handle → `reply`, never an IPC client. A restart re-zeroes the
/// shadow (all pins inputs), which is the honest post-fault state.
extern "C" fn task_gpio() -> ! {
    use crate::ipc::{recv, reply};
    unsafe {
        GPIO_DIRS = 0;
        GPIO_LEVELS = 0;
    }
    loop {
        // All operations here are safe (`recv`/`reply`/`gpio_handle` are
        // safe functions with `unsafe` interiors), so no `unsafe` block.
        let mut rbuf = [0u8; MSG_MAX];
        match recv(EP_GPIO, &mut rbuf) {
            RecvResult::Received(rlen) => {
                let (rep, replen) = gpio_handle(&rbuf[..rlen]);
                match reply(&rep[..replen]) {
                    ReplyResult::Replied => {}
                    ReplyResult::Failed(e) => panic!("gpio: reply failed: {:?}", e),
                }
            }
            RecvResult::Blocked => {}
            RecvResult::Failed(e) => panic!("gpio: recv failed: {:?}", e),
        }
        crate::sched::yield_now!();
    }
}

/// The LED pin: GPIO 8 — a safe general-purpose pin (not strapping, not
/// USB serial/JTAG).
const LED_PIN: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PhaseLed {
    /// (Re)configure the LED pin as output through the GPIO capsule.
    Init,
    /// Serve `EP_LED` requests.
    Serve,
    /// A GPIO op is outstanding for the saved `LED_REQ`.
    Gpio,
    /// Log the new state through the UART capsule, then reply.
    Log,
}

static mut PHASE_LED: PhaseLed = PhaseLed::Init;
static mut LED_REQ: u8 = 0;
/// 0 = off, 1 = on: the state after the last completed op.
static mut LED_STATE: u8 = 0;

/// LED capsule: owns no hardware; drives the LED pin through the GPIO
/// capsule and logs state changes through the UART capsule — the
/// layering demonstration (C → LED → GPIO, all synchronous IPC).
/// Restart-tolerant: the entry point re-runs `Init`, and a
/// `PartnerFaulted` from either capsule retries the idempotent phase.
extern "C" fn task_led() -> ! {
    use crate::ipc::{call, recv, reply};
    unsafe {
        PHASE_LED = PhaseLed::Init;
    }
    loop {
        unsafe {
            match PHASE_LED {
                PhaseLed::Init => {
                    let mut rbuf = [0u8; MSG_MAX];
                    match call(EP_GPIO, &[b'C', LED_PIN, 1], &mut rbuf) {
                        CallResult::Replied(rlen) => {
                            if rlen < 1 || rbuf[0] != GPIO_OK {
                                panic!("led: gpio configure failed");
                            }
                            LED_STATE = 0;
                            PHASE_LED = PhaseLed::Serve;
                        }
                        CallResult::Blocked => {}
                        CallResult::Failed(IpcError::PartnerFaulted) => {}
                        CallResult::Failed(e) => panic!("led: gpio call failed: {:?}", e),
                    }
                }
                PhaseLed::Serve => {
                    let mut rbuf = [0u8; MSG_MAX];
                    match recv(EP_LED, &mut rbuf) {
                        RecvResult::Received(rlen) => {
                            if rlen == 1 && (rbuf[0] == b'+' || rbuf[0] == b'-' || rbuf[0] == b'^')
                            {
                                LED_REQ = rbuf[0];
                                PHASE_LED = PhaseLed::Gpio;
                            } else {
                                // Bad request: honest error reply, keep serving.
                                // (NoPartner: the sender wasn't waiting for a
                                // reply — a plain send — so just drop it.)
                                match reply(&[1u8]) {
                                    ReplyResult::Replied => {}
                                    ReplyResult::Failed(IpcError::NoPartner) => {}
                                    ReplyResult::Failed(e) => {
                                        panic!("led: error reply failed: {:?}", e)
                                    }
                                }
                            }
                        }
                        RecvResult::Blocked => {}
                        RecvResult::Failed(e) => panic!("led: recv failed: {:?}", e),
                    }
                }
                PhaseLed::Gpio => {
                    // Map the LED op onto the GPIO op. The `_` arm is `^`:
                    // `Serve` only admits `+`, `-`, `^`.
                    let gop = match LED_REQ {
                        b'+' => b'S',
                        b'-' => b'c',
                        _ => b'T',
                    };
                    let mut rbuf = [0u8; MSG_MAX];
                    match call(EP_GPIO, &[gop, LED_PIN], &mut rbuf) {
                        CallResult::Replied(rlen) => {
                            let ok = rlen >= 1 && rbuf[0] == GPIO_OK && (gop != b'T' || rlen >= 2);
                            if !ok {
                                panic!("led: gpio op failed");
                            }
                            LED_STATE = if LED_REQ == b'+' {
                                1
                            } else if LED_REQ == b'-' {
                                0
                            } else {
                                rbuf[1]
                            };
                            PHASE_LED = PhaseLed::Log;
                        }
                        CallResult::Blocked => {}
                        // Idempotent retry: no reply sent yet, so the GPIO
                        // op simply runs again.
                        CallResult::Failed(IpcError::PartnerFaulted) => {}
                        CallResult::Failed(e) => panic!("led: gpio call failed: {:?}", e),
                    }
                }
                PhaseLed::Log => {
                    let line: &[u8] = if LED_STATE == 1 {
                        b"[led] on\n"
                    } else {
                        b"[led] off\n"
                    };
                    let mut rbuf = [0u8; MSG_MAX];
                    match call(EP_UART, line, &mut rbuf) {
                        CallResult::Replied(_) => {
                            let rep = [0u8, LED_STATE];
                            match reply(&rep) {
                                ReplyResult::Replied => PHASE_LED = PhaseLed::Serve,
                                ReplyResult::Failed(e) => panic!("led: reply failed: {:?}", e),
                            }
                        }
                        CallResult::Blocked => {}
                        // Safe retry: no reply was sent, so the line was
                        // not printed. (In the demo the UART capsule never
                        // faults; this is defensive.)
                        CallResult::Failed(IpcError::PartnerFaulted) => {}
                        CallResult::Failed(e) => panic!("led: uart call failed: {:?}", e),
                    }
                }
            }
        }
        crate::sched::yield_now!();
    }
}

/// UART capsule: owns UART0. **All task output flows through it** —
/// `recv` a line, write it with `uart::Writer`, reply with the byte
/// count. Never an IPC client; never blocks except in `recv`.
extern "C" fn task_uart() -> ! {
    use crate::ipc::{recv, reply};
    loop {
        // All operations here are safe (`recv`/`reply`/`putc` are safe
        // functions with `unsafe` interiors), so no `unsafe` block.
        let mut rbuf = [0u8; MSG_MAX];
        match recv(EP_UART, &mut rbuf) {
            RecvResult::Received(rlen) => {
                let mut i = 0;
                while i < rlen {
                    crate::uart::Writer::putc(rbuf[i]);
                    i += 1;
                }
                let rep = [rlen as u8];
                match reply(&rep) {
                    ReplyResult::Replied => {}
                    ReplyResult::Failed(e) => panic!("uart: reply failed: {:?}", e),
                }
            }
            RecvResult::Blocked => {}
            RecvResult::Failed(e) => panic!("uart: recv failed: {:?}", e),
        }
        crate::sched::yield_now!();
    }
}

/// Slices between LED toggles: the blink cadence (~200 ticks at the
/// nominal 1 kHz tick).
const BLINK_SLICES: u32 = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PhaseC {
    Count,
    Toggle,
}

static mut PHASE_C: PhaseC = PhaseC::Count;
static mut SLICES_C: u32 = 0;

/// Task C: the blink driver. Every `BLINK_SLICES` slices it `call`s the
/// LED capsule with `^` (toggle); otherwise silent. The `[led]` lines in
/// the log are the observable proof it ran. Retries on `PartnerFaulted`.
extern "C" fn task_c() -> ! {
    use crate::ipc::call;
    unsafe {
        PHASE_C = PhaseC::Count;
        SLICES_C = 0;
    }
    loop {
        unsafe {
            match PHASE_C {
                PhaseC::Count => {
                    SLICES_C = SLICES_C.wrapping_add(1);
                    if SLICES_C >= BLINK_SLICES {
                        SLICES_C = 0;
                        PHASE_C = PhaseC::Toggle;
                    }
                }
                PhaseC::Toggle => {
                    let mut rbuf = [0u8; MSG_MAX];
                    match call(EP_LED, b"^", &mut rbuf) {
                        CallResult::Replied(rlen) => {
                            if rlen < 2 || rbuf[0] != 0 {
                                panic!("task C: led toggle failed");
                            }
                            PHASE_C = PhaseC::Count;
                        }
                        CallResult::Blocked => {}
                        CallResult::Failed(IpcError::PartnerFaulted) => {}
                        CallResult::Failed(e) => panic!("task C: led call failed: {:?}", e),
                    }
                }
            }
        }
        crate::sched::yield_now!();
    }
}
