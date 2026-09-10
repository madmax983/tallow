//! v0.2 "tick": the 1 kHz timer, polled.
//!
//! Clock tree: TIMG0 runs off the 80 MHz APB clock. Timer 0 is configured
//! with a divider of 80 (1 MHz timer clock), counts upward from 0 with
//! autoreload, and raises an alarm every 1000 ticks — 1 kHz nominal.
//!
//! v0.2 polls the timer's interrupt RAW bit in the main loop instead of
//! using CPU interrupts. (QEMU's esp32s3 interrupt matrix model proved
//! unreliable: the ROM leaves stale mappings/pendings on CPU lines, and
//! the CCOUNT special registers aren't recognized by the Espressif
//! assembler. Polling is simple, correct, and gives us the 1 kHz tick.
//! Proper interrupt-driven tick moves to v0.3.)
//!
//! The poll loop ([`poll_tick`]) does the minimum: if the T0 alarm fired,
//! bump [`TICKS`], clear the interrupt (write-1-to-clear), and re-arm the
//! alarm. Re-arming is mandatory — `ALARM_EN` self-clears when the alarm
//! fires (TRM 12.3.3 "Timer as Periodic Alarm"), even with `AUTORELOAD` set.
//! Forget it and the timer fires exactly once.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

/// Millisecond tick counter, bumped by the poll loop and read by the main
/// loop. A 32-bit count at 1 kHz wraps after ~49.7 days; v0.2 accepts that.
#[no_mangle]
pub static TICKS: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------------
// Timer Group 0, Timer 0 (TRM ch. 12, APB clock 80 MHz)
// ---------------------------------------------------------------------------

const TIMG0_BASE: usize = 0x6001_F000;
const T0CONFIG: *mut u32 = (TIMG0_BASE + 0x00) as *mut u32;
const T0ALARMLO: *mut u32 = (TIMG0_BASE + 0x10) as *mut u32;
const T0ALARMHI: *mut u32 = (TIMG0_BASE + 0x14) as *mut u32;
const T0LOADLO: *mut u32 = (TIMG0_BASE + 0x18) as *mut u32;
const T0LOADHI: *mut u32 = (TIMG0_BASE + 0x1C) as *mut u32;
const T0LOAD: *mut u32 = (TIMG0_BASE + 0x20) as *mut u32;
const INT_RAW_TIMERS: *const u32 = (TIMG0_BASE + 0x74) as *const u32;
const INT_CLR_TIMERS: *mut u32 = (TIMG0_BASE + 0x7C) as *mut u32;

/// T0CONFIG fields: EN bit 31, INCREASE bit 30, AUTORELOAD bit 29,
/// DIVIDER bits [28:13], ALARM_EN bit 10. USE_XTAL bit 9 = 0 selects APB.
const CFG_EN: u32 = 1 << 31;
const CFG_INCREASE: u32 = 1 << 30;
const CFG_AUTORELOAD: u32 = 1 << 29;
const CFG_DIVIDER_80: u32 = 80 << 13; // 80 MHz / 80 = 1 MHz timer clock
const CFG_ALARM_EN: u32 = 1 << 10;
/// Alarm value for 1 kHz at a 1 MHz timer clock.
const ALARM_USEC: u32 = 1000;

// ---------------------------------------------------------------------------
// Init and poll
// ---------------------------------------------------------------------------

/// Bring up the 1 kHz tick. Configures Timer 0 while stopped, then starts
/// it with the first alarm armed. No CPU interrupts are used.
pub unsafe fn init() {
    TICKS.store(0, Ordering::Relaxed);

    // Configure Timer 0 while stopped: up-counting, autoreload, 1 MHz.
    unsafe {
        write_volatile(T0CONFIG, CFG_INCREASE | CFG_AUTORELOAD | CFG_DIVIDER_80);
        write_volatile(T0LOADLO, 0);
        write_volatile(T0LOADHI, 0);
        write_volatile(T0LOAD, 1); // any write latches the load value
        write_volatile(T0ALARMLO, ALARM_USEC);
        write_volatile(T0ALARMHI, 0);

        // Clear any stale T0 interrupt.
        write_volatile(INT_CLR_TIMERS, 1);

        // Start the timer and arm the first alarm.
        write_volatile(
            T0CONFIG,
            CFG_EN | CFG_INCREASE | CFG_AUTORELOAD | CFG_DIVIDER_80 | CFG_ALARM_EN,
        );
    }
}

/// Poll the timer. If the 1 kHz alarm fired since the last call, bump
/// [`TICKS`], clear the interrupt, and re-arm. Returns true on a tick.
pub fn poll_tick() -> bool {
    unsafe {
        if read_volatile(INT_RAW_TIMERS) & 1 == 0 {
            return false;
        }
        // Alarm fired: count it, clear it, re-arm it.
        TICKS.fetch_add(1, Ordering::Relaxed);
        write_volatile(INT_CLR_TIMERS, 1);
        let cfg = read_volatile(T0CONFIG);
        write_volatile(T0CONFIG, cfg | CFG_ALARM_EN);
        true
    }
}
