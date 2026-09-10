//! Tallow — a tiny best-in-breed OS kernel for the ESP32-S3.
//!
//! v0.1 "spark": boot to a UART banner, then idle. The nervous system
//! (timer tick, tasks, IPC, MPU compartments) arrives in later milestones.
//!
//! Boot path: the ESP32-S3 ROM loads the app image from flash offset 0x0
//! and jumps to its entry point. `_start` below installs the stack pointer
//! first (before any Rust code runs), then calls into `kernel_main`.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

mod print;
mod uart;

use core::arch::asm;
use core::panic::PanicInfo;

// Top of HP SRAM as seen through the DRAM bus (0x3FC88000..0x3FD08000),
// with 32 bytes of slack. Installed as the stack in `_start`.
const STACK_TOP: u32 = 0x3FCF_FFE0;

/// Kernel entry point. The ROM jumps here with `a1` pointing at its own
/// (small but valid) stack, so a normal Rust prologue is fine — we then
/// install our own stack pointer and enter `kernel_main`, never to return.
///
/// NOTE: the stack address is materialized by the *compiler* (it loads the
/// constant into a scratch register via its own literal-pool machinery, and
/// we `mov` it into `a1`). Do NOT hand-write `l32r a1, <inline literal>`
/// here: the Xtensa linker's literal-pool handling mangles relocations for
/// hand-placed literals in `.text` (observed resolving 0x40000 off, causing
/// a LoadStoreError on the very first instruction).
#[no_mangle]
pub extern "C" fn _start() -> ! {
    unsafe {
        asm!(
            "mov a1, {top}",
            top = in(reg) STACK_TOP,
            options(nostack, nomem),
        );
    }
    // Now running on our own stack.
    unsafe { kernel_main() }
}

/// Rust entry point. Prints the boot banner, then parks the CPU.
#[no_mangle]
pub unsafe extern "C" fn kernel_main() -> ! {
    println!();
    println!("Tallow v0.1 \"spark\" -- the little OS that could");
    println!("target: ESP32-S3 (Xtensa LX7) | no_std | no heap | no mercy");
    println!("uart0 console up; kernel alive");
    println!();

    // Make sure the whole banner is shifted out before we go quiet.
    uart::Writer::drain();

    loop {
        // waiti 0: lowest-power idle. Wakes on any interrupt (none enabled
        // yet — the timer tick in v0.2 will be the first).
        unsafe { asm!("waiti 0", options(nomem, nostack)) };
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
