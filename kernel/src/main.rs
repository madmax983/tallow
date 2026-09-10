//! Tallow — a tiny best-in-breed OS kernel for the ESP32-S3.
//!
//! v0.2 "tick": the kernel brings up Timer Group 0, Timer 0 for a 1 kHz
//! tick, prints its banner, then loops printing a heartbeat every 1000
//! ticks. The tick is polled, not interrupt-driven — the main loop checks
//! the timer's interrupt RAW bit, bumps the tick counter, clears the
//! interrupt, and re-arms the alarm. The first proof the kernel is alive,
//! not merely booted.
//!
//! Boot path: the ESP32-S3 ROM loads the app image from flash offset 0x0
//! and jumps to its entry point. `_start` below establishes the machine
//! state the ROM does not guarantee (interrupts off, known `PS`), installs
//! the stack pointer, then calls into `kernel_main`.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

mod print;
mod timer;
mod uart;

use core::arch::asm;
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

// Top of HP SRAM as seen through the DRAM bus (0x3FC88000..0x3FD08000),
// with 32 bytes of slack. Installed as the stack in `_start`.
const STACK_TOP: u32 = 0x3FCF_FFE0;

/// Kernel entry point.
///
/// The ROM jumps here with `a1` pointing at its own (small but valid) stack,
/// so a normal Rust prologue is fine. We establish the machine state we
/// depend on — `INTENABLE = 0` (no interrupt can fire while we set up),
/// `PS = 0x40000` (`WOE = 1`, `INTLEVEL = 0`, `EXCM = 0`, `UM = 0`), and
/// `VECBASE` pointing at our vector table — then install our own stack and
/// enter `kernel_main`, never to return.
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
            "wsr {t}, INTENABLE", // deaf until we're ready
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
    unsafe { kernel_main() }
}

/// Rust entry point: timer tick, banner, then the heartbeat loop.
#[no_mangle]
pub unsafe extern "C" fn kernel_main() -> ! {
    // The 1 kHz tick, polled. On return the timer is running.
    unsafe { timer::init() };

    println!();
    println!("Tallow v0.2 \"tick\" -- the little OS that could");
    println!("target: ESP32-S3 (Xtensa LX7) | no_std | no heap | no mercy");
    println!("timg0 1 kHz tick (polled)");
    println!();

    // Heartbeat: poll_tick() bumps TICKS at 1 kHz; report every 1000 ticks.
    let mut last_beat = 0u32;
    loop {
        timer::poll_tick();
        let now = timer::TICKS.load(Ordering::Relaxed);
        if now.wrapping_sub(last_beat) >= 1000 {
            last_beat = now;
            println!("[heartbeat] ticks = {}", now);
        }
    }
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
