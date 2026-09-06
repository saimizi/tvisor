use crate::halt;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicUsize, Ordering};

const UART_TX_POLL_COUNT: usize = 1_000_000;

static MINI_UART_BASE: AtomicUsize = AtomicUsize::new(0);

struct MiniUart {
    register_base: usize,
}

impl MiniUart {
    const IO_OFFSET: usize = 0x00;
    const LSR_OFFSET: usize = 0x14;
    const LSR_TX_READY: u32 = 1 << 5;
    const LSR_IDLE: u32 = 1 << 6;

    fn configured() -> Option<Self> {
        let register_base = MINI_UART_BASE.load(Ordering::Relaxed);
        (register_base != 0).then_some(Self { register_base })
    }

    fn register(&self, offset: usize) -> usize {
        self.register_base + offset
    }

    fn wait_io_completed(&self) -> bool {
        for _ in 0..UART_TX_POLL_COUNT {
            if unsafe { mmio_read32(self.register(Self::LSR_OFFSET)) } & Self::LSR_IDLE != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    fn is_tx_ready(&self) -> bool {
        for _ in 0..UART_TX_POLL_COUNT {
            if unsafe { mmio_read32(self.register(Self::LSR_OFFSET)) } & Self::LSR_TX_READY != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    fn write_byte_raw(&self, byte: u8) -> Result<(), ()> {
        if self.is_tx_ready() {
            unsafe { mmio_write32(self.register(Self::IO_OFFSET), u32::from(byte)) };
            Ok(())
        } else {
            Err(())
        }
    }

    fn write_byte(&self, byte: u8) -> Result<(), ()> {
        if byte == b'\n' {
            self.write_byte_raw(b'\r')?;
        }
        self.write_byte_raw(byte)
    }
}

impl Write for MiniUart {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            self.write_byte(byte).map_err(|_| fmt::Error)?;
        }
        Ok(())
    }
}

#[inline(always)]
unsafe fn mmio_read32(address: usize) -> u32 {
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

#[inline(always)]
unsafe fn mmio_write32(address: usize, value: u32) {
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

pub fn print(args: fmt::Arguments<'_>) -> fmt::Result {
    let mut uart = MiniUart::configured().ok_or(fmt::Error)?;
    uart.write_fmt(args)
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        let _ = $crate::debug_util::print(core::format_args!($($arg)*));
    }}
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };

    ($($arg:tt)*) => {
        $crate::print!("{}\n", core::format_args!($($arg)*))
    };
}

/// Enables diagnostic output through a DTB-discovered Mini UART register
/// window. No MMIO is accessed before this function is called.
pub fn debug_init(mini_uart_register_base: usize) {
    MINI_UART_BASE.store(mini_uart_register_base, Ordering::Relaxed);
}

pub fn debug_fini() {
    println!("Wait IO completed...");
    if let Some(uart) = MiniUart::configured() {
        let _ = uart.wait_io_completed();
    }
}

pub fn stop() -> ! {
    debug_fini();
    halt()
}
