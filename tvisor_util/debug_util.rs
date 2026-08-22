use core::fmt::{self, Write};
use core::sync::atomic::{AtomicUsize, Ordering};

static GLOBAL_DEBUG_MEM: DebugMem = DebugMem::new();

pub fn debug_mem_error(err: DebugMemError) {
    GLOBAL_DEBUG_MEM.push_err(err)
}

struct DebugMem {
    cur: AtomicUsize,
}

#[repr(u8)]
pub enum DebugMemError {
    InvalidEL2State = 0x1,
    WaitUartIoComplete = 0x2,
    UartTxTimeout = 0x3,
}

impl DebugMem {
    const DEBUG_MEM_START: *mut u8 = 0x00001000 as *mut u8;
    const DEBUG_MEM_SIZE: usize = 0x00000100;

    pub const fn new() -> Self {
        Self {
            cur: AtomicUsize::new(0),
        }
    }

    pub fn init(&self) {
        self.cur.store(0, Ordering::Relaxed);

        for offset in 0..Self::DEBUG_MEM_SIZE {
            unsafe { core::ptr::write_volatile(Self::DEBUG_MEM_START.add(offset), 0) }
        }
    }

    pub fn push_err(&self, code: DebugMemError) {
        let result = self
            .cur
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current < Self::DEBUG_MEM_SIZE {
                    Some(current + 1)
                } else {
                    None
                }
            });

        if let Ok(offset) = result {
            unsafe { core::ptr::write_volatile(Self::DEBUG_MEM_START.add(offset), code as u8) }
        }
    }
}

struct AuxRegister;

#[allow(unused)]
impl AuxRegister {
    const LSR_TX_READY: u32 = 0x1 << 5;
    const LSR_IDLE: u32 = 0x1 << 6;
    const AUX_BASE: usize = 0xFE21_5000;
    const IRQ: usize = AuxRegister::AUX_BASE;
    const ENABLES: usize = AuxRegister::AUX_BASE + 0x04;
    const MU_IO: usize = AuxRegister::AUX_BASE + 0x40;
    const MU_IER: usize = AuxRegister::AUX_BASE + 0x44;
    const MU_IIR: usize = AuxRegister::AUX_BASE + 0x48;
    const MU_LCR: usize = AuxRegister::AUX_BASE + 0x4C;
    const MU_MCR: usize = AuxRegister::AUX_BASE + 0x50;
    const MU_LSR: usize = AuxRegister::AUX_BASE + 0x54;
    const MU_MSR: usize = AuxRegister::AUX_BASE + 0x58;
    const MU_SCRATCH: usize = AuxRegister::AUX_BASE + 0x5C;
    const MU_CNTL: usize = AuxRegister::AUX_BASE + 0x60;
    const MU_STAT: usize = AuxRegister::AUX_BASE + 0x64;
    const MU_BAUD: usize = AuxRegister::AUX_BASE + 0x68;

    pub fn wait_io_completed(max_pool_cnt: Option<usize>) -> bool {
        let poll_cnt = max_pool_cnt.unwrap_or(1_000_000);

        for _ in 0..poll_cnt {
            if unsafe { mmio_read32(AuxRegister::MU_LSR) } & AuxRegister::LSR_IDLE != 0 {
                return true;
            }

            core::hint::spin_loop();
        }

        false
    }

    pub fn is_tx_ready(max_pool_cnt: Option<usize>) -> bool {
        let poll_cnt = max_pool_cnt.unwrap_or(1_000_000);

        for _ in 0..poll_cnt {
            if unsafe { mmio_read32(AuxRegister::MU_LSR) } & AuxRegister::LSR_TX_READY != 0 {
                return true;
            }

            core::hint::spin_loop();
        }

        false
    }

    pub fn write_byte_raw(byte: u8) -> Result<(), ()> {
        if AuxRegister::is_tx_ready(None) {
            unsafe { mmio_write32(AuxRegister::MU_IO, byte as u32) };
            Ok(())
        } else {
            debug_mem_error(DebugMemError::UartTxTimeout);
            Err(())
        }
    }

    pub fn write_byte(byte: u8) -> Result<(), ()> {
        if byte == b'\n' {
            AuxRegister::write_byte_raw(b'\r')?;
        }

        AuxRegister::write_byte_raw(byte)
    }
}

impl core::fmt::Write for AuxRegister {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        for byte in text.bytes() {
            AuxRegister::write_byte(byte).map_err(|_| core::fmt::Error)?;
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

pub fn print(args: core::fmt::Arguments<'_>) -> fmt::Result {
    let mut uart = AuxRegister;
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

pub fn debug_init() {
    GLOBAL_DEBUG_MEM.init();
}

pub fn debug_fini() {
    println!("Wait IO completed...");
    if !AuxRegister::wait_io_completed(None) {
        debug_mem_error(DebugMemError::WaitUartIoComplete);
    }
}
