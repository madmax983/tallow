//! Tallow — a tiny best-in-breed OS kernel for the ESP32-S3.
//!
//! v0.4 "ipc": the kernel brings up Timer Group 0, Timer 0 for a 1 kHz
//! tick (polled), initializes two worker tasks (A and B) on their own
//! stacks, then enters the cooperative scheduler. A and B hold a real
//! conversation over synchronous rendezvous IPC (`call`/`recv`/`reply`
//! plus `notify`/`wait`); each exchange prints `[ipc NNNN] ping -> pong`.
//! The idle task (the scheduler loop itself) prints a heartbeat every
//! 1000 ticks. Output shows the numbered exchanges with `[t=N]`
//! markers — deterministic proof that IPC, the tick, and both tasks work.
//!
//! Tasks run as coroutines: the scheduler resumes each task through a
//! symmetric register-window switch (`sched::ctx_switch!`) that never
//! lets a windowed call or return cross stacks. A task runs one slice,
//! yields explicitly, and is resumed after the yield with its locals and
//! call depth intact.
//!
//! Boot path: the ESP32-S3 ROM loads the app image from flash offset 0x0
//! and jumps to its entry point. `_start` below establishes the machine
//! state the ROM does not guarantee (interrupts off, known `PS`),
//! installs the stack pointer, then calls into `kernel_main`.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

mod ipc;
mod print;
mod sched;
mod task;
mod timer;
mod uart;

use core::arch::asm;
use core::panic::PanicInfo;

// Top of HP SRAM as seen through the DRAM bus (0x3FC88000..0x3FD08000),
// with 32 bytes of slack. Installed as the stack in `_start`.
const STACK_TOP: u32 = 0x3FCF_FFE0;

/// Kernel entry point.
///
/// The ROM jumps here with `a1` pointing at its own (small but valid) stack,
/// so a normal Rust prologue is fine. We establish the machine state we
/// depend on — `INTENABLE = 0` (no interrupt can fire while we set up),
/// `PS = 0x40000` (`WOE = 1`, `INTLEVEL = 0`, `EXCM = 0`, `UM = 0`) —
/// then install our own stack and enter `kernel_main`, never to return.
///
/// NOTE: addresses are materialized by the *compiler* (`in(reg)` operands;
/// it emits its own literal-pool loads). Do NOT hand-write `l32r` with an
/// inline literal: the Xtensa linker's literal-pool handling mangles
/// relocations for hand-placed literals in `.text` (observed resolving
/// 0x40000 off, causing a LoadStoreError on the very first instruction).
#[no_mangle]
pub extern "C" fn _start() -> ! {
    unsafe {
        asm!(
            "movi {t}, 0",
            "wsr {t}, INTENABLE", // deaf until we're ready (we never enable)
            "movi {t}, 0x40",
            "slli {t}, {t}, 12",  // {t} = 0x40000 = PS.WOE
            "wsr {t}, PS",
            "rsync",
            "mov a1, {top}",     // install our stack
            t = out(reg) _,
            top = in(reg) STACK_TOP,
            options(nostack, nomem),
        );
    }
    // Now running on our own stack: interrupts off, PS known.
    // Window spill/fill uses the ROM handlers (custom handlers are v0.5's
    // problem, when faults need catching).
    unsafe { kernel_main() }
}

/// Rust entry point: timer, tasks, banner, then the scheduler. Never returns.
///
/// # Safety
///
/// Call exactly once, from `_start` after it has switched to the boot
/// stack with interrupts disabled. The caller must guarantee no other
/// code is running.
#[no_mangle]
pub unsafe extern "C" fn kernel_main() -> ! {
    // The 1 kHz tick (polled).
    unsafe { timer::init() };
    // Task table: A and B with their own stacks.
    unsafe { task::init() };

    println!();
    println!("Tallow v0.4 \"ipc\" -- the little OS that could");
    println!("target: ESP32-S3 (Xtensa LX7) | no_std | no heap | no mercy");
    println!("timg0 1 kHz tick (polled) | tasks: A, B, idle (cooperative)");
    println!("ipc: rendezvous EP_PING=0, MSG_MAX=64, notify/wait");
    // The static task table, as the kernel sees it.
    let mut i = 0;
    while i < ipc::N_TASKS {
        let (name, stack_top) = unsafe {
            let t = &*task::task_ptr(i);
            (t.name, t.stack_top)
        };
        println!(
            "task {} ({}): stack top {:#010x} (4 KiB)",
            i, name, stack_top
        );
        i += 1;
    }
    println!();

    // The scheduler never returns. A and B interleave below.
    sched::run()
}

/// A panic is a kernel bug, full stop. Print where and why on the console,
/// drain the FIFO so the message actually escapes, then halt.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!();
    println!("!!! KERNEL PANIC !!!");
    if let Some(location) = info.location() {
        println!(
            "at {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }
    println!("{}", info.message());
    uart::Writer::drain();
    loop {
        unsafe { asm!("waiti 0", options(nomem, nostack)) };
    }
}
