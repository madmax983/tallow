//! v0.3 "tasks": the 1 kHz timer, polled.
//!
//! Clock tree: TIMG0 runs off the 80 MHz APB clock. Timer 0 is configured
//! with a divider of 80 (1 MHz timer clock), counts upward from 0 with
//! autoreload, and raises an alarm every 1000 ticks — 1 kHz nominal.
//!
//! v0.3 polls the alarm (like v0.2 did). The RAW/CLEAR addresses below
//! are the ones v0.2 proved in QEMU (0x74 raw, 0x7C clear) — note the
//! raw register is NOT at the TRM's nominal 0x68 on this QEMU model;
//! trust the experiment, not the datasheet, until proven otherwise.
//!
//! An interrupt-driven tick was attempted: the alarm *does* reach the
//! CPU (the exception vector fires), but QEMU's ESP32-S3 model never
//! returns from the handler — `rfe`, manual `EPS` restore, and
//! `jx EPC1` all hang. Until the model (or our understanding of its
//! exception return) is fixed, the tick is polled and scheduling is
//! cooperative. The timer hardware itself is proven: v0.2 produced
//! thousands of monotonic polled ticks.

use core::ptr::{read_volatile, write_volatile};

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
const INT_RAW_TIMERS: *mut u32 = (TIMG0_BASE + 0x74) as *mut u32;
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

/// Running T0CONFIG: EN|INCREASE|AUTORELOAD | DIVIDER=80 | ALARM_EN.
const CFG_RUNNING: u32 = CFG_EN | CFG_INCREASE | CFG_AUTORELOAD | CFG_DIVIDER_80 | CFG_ALARM_EN;

// ---------------------------------------------------------------------------
// Init + poll
// ---------------------------------------------------------------------------

/// Bring up the 1 kHz tick.
///
/// Configures Timer 0 while stopped, then starts it with the first alarm
/// armed. No interrupt matrix, no CPU interrupt line — the tick is polled
/// via [`poll_tick`].
pub unsafe fn init() {
    unsafe {
        // Configure Timer 0 while stopped: up-counting, autoreload, 1 MHz.
        write_volatile(T0CONFIG, CFG_INCREASE | CFG_AUTORELOAD | CFG_DIVIDER_80);
        write_volatile(T0LOADLO, 0);
        write_volatile(T0LOADHI, 0);
        write_volatile(T0LOAD, 1); // any write latches the load value
        write_volatile(T0ALARMLO, ALARM_USEC);
        write_volatile(T0ALARMHI, 0);

        // Clear any stale T0 interrupt.
        write_volatile(INT_CLR_TIMERS, 1);

        // Start the timer and arm the first alarm.
        write_volatile(T0CONFIG, CFG_RUNNING);
    }
}

/// Check for a timer tick. Returns `true` once per 1 kHz alarm.
///
/// On a tick: clears the peripheral interrupt and re-arms `ALARM_EN`,
/// which self-clears when the alarm fires (TRM 12.3.3) even with
/// `AUTORELOAD` set. Forget the re-arm and the timer fires exactly once.
pub unsafe fn poll_tick() -> bool {
    unsafe {
        if read_volatile(INT_RAW_TIMERS) & 1 != 0 {
            write_volatile(INT_CLR_TIMERS, 1);
            write_volatile(T0CONFIG, CFG_RUNNING);
            true
        } else {
            false
        }
    }
}
