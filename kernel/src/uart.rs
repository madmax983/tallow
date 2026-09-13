//! UART0 driver for the ESP32-S3 (Technical Reference Manual, UART chapter).
//!
//! The ROM bootloader leaves UART0 clocked and configured, so the kernel
//! just writes bytes into the TX FIFO. No initialization, no interrupts
//! yet — that comes with the driver model in v0.6.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};

const UART0_BASE: usize = 0x6000_0000;
const UART_FIFO: *mut u32 = UART0_BASE as *mut u32; // FIFO write register
const UART_STATUS: *const u32 = (UART0_BASE + 0x1C) as *const u32; // STATUS register
const TXFIFO_CNT_SHIFT: u32 = 16; // STATUS[25:16] = bytes currently in TX FIFO
const UART_TX_FIFO_LEN: u32 = 128; // ESP32-S3 UART TX FIFO depth

/// UART0 console writer. There is exactly one; it is a unit struct because
/// there is no state to keep — the hardware holds it all.
pub struct Writer;

impl Writer {
    fn txfifo_count() -> u32 {
        (unsafe { read_volatile(UART_STATUS) } >> TXFIFO_CNT_SHIFT) & 0xFF
    }

    /// Block until there is room in the TX FIFO and emit one byte.
    pub fn putc(byte: u8) {
        while Self::txfifo_count() >= UART_TX_FIFO_LEN {}
        unsafe { write_volatile(UART_FIFO, u32::from(byte)) };
    }

    /// Spin until the TX FIFO is fully shifted out. Call before parking the
    /// CPU or resetting, so no output is lost.
    pub fn drain() {
        while Self::txfifo_count() != 0 {
            core::hint::spin_loop();
        }
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            // The S3 UART does no newline translation; do it here.
            if byte == b'\n' {
                Self::putc(b'\r');
            }
            Self::putc(byte);
        }
        Ok(())
    }
}

/// Backend for the `print!` / `println!` macros.
pub fn _print(args: fmt::Arguments) {
    use fmt::Write as _;
    let _ = Writer.write_fmt(args);
}
