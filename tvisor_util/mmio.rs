//! Trapped stage-2 MMIO decoding and the Phase 10 virtual PL011.

use core::fmt;

use crate::guest_platform::{GUEST_GICD, GUEST_PL011};

const ESR_ISV: u32 = 1 << 24;
const ESR_SAS_SHIFT: u32 = 22;
const ESR_SSE: u32 = 1 << 21;
const ESR_SRT_SHIFT: u32 = 16;
const ESR_SF: u32 = 1 << 15;
const ESR_WNR: u32 = 1 << 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioDecodeError {
    InstructionSyndromeInvalid,
    SignExtendingRead,
    InvalidAccessSize,
}

impl fmt::Display for MmioDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstructionSyndromeInvalid => {
                f.write_str("stage-2 abort has no valid instruction syndrome")
            }
            Self::SignExtendingRead => f.write_str("sign-extending MMIO read is unsupported"),
            Self::InvalidAccessSize => f.write_str("invalid stage-2 MMIO access size"),
        }
    }
}

/// A load or store reconstructed from a stage-2 Data Abort syndrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioAccess {
    pub ipa: u64,
    pub is_write: bool,
    pub width: u8,
    pub register: u8,
    pub register_is_64bit: bool,
}

impl MmioAccess {
    pub fn decode_data_abort(esr_el2: u64, ipa: u64) -> Result<Self, MmioDecodeError> {
        let iss = esr_el2 as u32 & 0x01ff_ffff;
        if iss & ESR_ISV == 0 {
            return Err(MmioDecodeError::InstructionSyndromeInvalid);
        }
        let width = match (iss >> ESR_SAS_SHIFT) & 0x3 {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 8,
            _ => return Err(MmioDecodeError::InvalidAccessSize),
        };
        let is_write = iss & ESR_WNR != 0;
        if !is_write && iss & ESR_SSE != 0 {
            return Err(MmioDecodeError::SignExtendingRead);
        }
        Ok(Self {
            ipa,
            is_write,
            width,
            register: ((iss >> ESR_SRT_SHIFT) & 0x1f) as u8,
            register_is_64bit: iss & ESR_SF != 0,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pl011Error {
    UnsupportedWidth(u8),
    UnsupportedRead(u64),
    UnsupportedWrite(u64),
}

impl fmt::Display for Pl011Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedWidth(width) => {
                write!(f, "PL011 does not support {width}-byte accesses")
            }
            Self::UnsupportedRead(offset) => {
                write!(f, "unsupported PL011 read at offset {offset:#x}")
            }
            Self::UnsupportedWrite(offset) => {
                write!(f, "unsupported PL011 write at offset {offset:#x}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pl011Result {
    Read(u64),
    Write(Option<u8>),
}

/// Emulated PrimeCell PL011 with a bounded RX FIFO. The host Mini UART remains
/// owned by tvisor; received bytes are routed into this virtual device.
#[derive(Debug)]
pub struct VirtualPl011 {
    imsc: u32,
    rx: [u8; 64],
    rx_head: usize,
    rx_len: usize,
}

impl Default for VirtualPl011 {
    fn default() -> Self {
        Self {
            imsc: 0,
            rx: [0; 64],
            rx_head: 0,
            rx_len: 0,
        }
    }
}

impl VirtualPl011 {
    const DR: u64 = 0x00;
    const FR: u64 = 0x18;
    const IBRD: u64 = 0x24;
    const FBRD: u64 = 0x28;
    const LCR_H: u64 = 0x2c;
    const CR: u64 = 0x30;
    const IMSC: u64 = 0x38;
    const RIS: u64 = 0x3c;
    const MIS: u64 = 0x40;
    const ICR: u64 = 0x44;
    const PID0: u64 = 0xfe0;
    const PID1: u64 = 0xfe4;
    const PID2: u64 = 0xfe8;
    const PID3: u64 = 0xfec;
    const CID0: u64 = 0xff0;
    const CID1: u64 = 0xff4;
    const CID2: u64 = 0xff8;
    const CID3: u64 = 0xffc;
    const RXIM: u32 = 1 << 4;
    const FR_TX_READY: u64 = 1 << 7;
    const FR_RXFE: u64 = 1 << 4;

    pub fn enqueue_rx(&mut self, byte: u8) -> bool {
        if self.rx_len == self.rx.len() {
            return false;
        }
        let tail = (self.rx_head + self.rx_len) % self.rx.len();
        self.rx[tail] = byte;
        self.rx_len += 1;
        true
    }

    fn dequeue_rx(&mut self) -> u8 {
        if self.rx_len == 0 {
            return 0;
        }
        let byte = self.rx[self.rx_head];
        self.rx_head = (self.rx_head + 1) % self.rx.len();
        self.rx_len -= 1;
        byte
    }

    fn rx_irq_pending(&self) -> bool {
        self.rx_len != 0 && self.imsc & Self::RXIM != 0
    }

    fn access(
        &mut self,
        offset: u64,
        is_write: bool,
        width: u8,
        value: u64,
    ) -> Result<Pl011Result, Pl011Error> {
        if !matches!(width, 1 | 2 | 4) {
            return Err(Pl011Error::UnsupportedWidth(width));
        }
        if is_write {
            match offset {
                Self::DR => Ok(Pl011Result::Write(Some(value as u8))),
                // Linux earlycon may program these before transmitting. Phase
                // 10.2 accepts them without exposing the physical Mini UART.
                Self::IBRD | Self::FBRD | Self::LCR_H | Self::CR | Self::ICR => {
                    Ok(Pl011Result::Write(None))
                }
                Self::IMSC => {
                    self.imsc = value as u32;
                    Ok(Pl011Result::Write(None))
                }
                _ => Ok(Pl011Result::Write(None)),
            }
        } else {
            match offset {
                Self::DR => Ok(Pl011Result::Read(self.dequeue_rx() as u64)),
                Self::FR => Ok(Pl011Result::Read(
                    Self::FR_TX_READY | if self.rx_len == 0 { Self::FR_RXFE } else { 0 },
                )),
                Self::IMSC => Ok(Pl011Result::Read(self.imsc as u64)),
                Self::RIS => Ok(Pl011Result::Read(if self.rx_len != 0 {
                    Self::RXIM as u64
                } else {
                    0
                })),
                Self::MIS => Ok(Pl011Result::Read(if self.rx_irq_pending() {
                    Self::RXIM as u64
                } else {
                    0
                })),
                // The AMBA bus verifies these IDs before it binds the PL011
                // driver. They encode ARM PrimeCell PL011 (0x0004_1011)
                // and the AMBA component signature (0xb105_f00d).
                Self::PID0 => Ok(Pl011Result::Read(0x11)),
                Self::PID1 => Ok(Pl011Result::Read(0x10)),
                Self::PID2 => Ok(Pl011Result::Read(0x04)),
                Self::PID3 => Ok(Pl011Result::Read(0x00)),
                Self::CID0 => Ok(Pl011Result::Read(0x0d)),
                Self::CID1 => Ok(Pl011Result::Read(0xf0)),
                Self::CID2 => Ok(Pl011Result::Read(0x05)),
                Self::CID3 => Ok(Pl011Result::Read(0xb1)),
                _ => Ok(Pl011Result::Read(0)),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioEmulationError {
    UnmappedIpa(u64),
    Pl011(Pl011Error),
    GicDistributor,
}

impl From<Pl011Error> for MmioEmulationError {
    fn from(error: Pl011Error) -> Self {
        Self::Pl011(error)
    }
}

impl fmt::Display for MmioEmulationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnmappedIpa(ipa) => write!(f, "no virtual MMIO device at IPA {ipa:#x}"),
            Self::Pl011(error) => write!(f, "PL011 emulation failed: {error}"),
            Self::GicDistributor => f.write_str("unsupported virtual GIC distributor access"),
        }
    }
}

/// Generic trapped-MMIO dispatcher. Devices never receive host physical MMIO.
#[derive(Debug, Default)]
pub struct MmioDispatcher {
    pl011: VirtualPl011,
    gicd: VirtualGicDistributor,
}

/// Minimal single-vCPU GICv2 distributor. It exposes only the discovery and
/// configuration state Linux needs before using the hardware-backed GICV CPU
/// interface; it never maps the host distributor into the guest.
#[derive(Debug, Default)]
struct VirtualGicDistributor {
    ctlr: u32,
}

impl VirtualGicDistributor {
    fn access(&mut self, offset: u64, is_write: bool, width: u8, value: u64) -> Result<u64, ()> {
        if !matches!(width, 1 | 2 | 4) {
            return Err(());
        }
        match (is_write, offset) {
            (false, 0x000) => Ok(self.ctlr as u64),
            // Two banks describe private interrupts plus the virtual PL011
            // SPI, which Linux needs before binding ttyAMA0.
            (false, 0x004) => Ok(1),
            (false, 0x008) => Ok(0x0200_0043),
            // Linux discovers the CPU target mask from this register bank
            // before programming its distributor state. Every byte targets
            // the only guest vCPU.
            (false, 0x800..=0x8ff) => Ok(0x0101_0101),
            // The remaining distributor state starts clear. Linux performs
            // read-modify-write setup for these banks, so expose their reset
            // value even though the single-vCPU timer path does not retain
            // their programmed state yet.
            (
                false,
                0x080..=0x0ff | 0x100..=0x1ff | 0x280..=0x2ff | 0x400..=0x7ff | 0xc00..=0xcff,
            ) => Ok(0),
            (true, 0x000) => {
                self.ctlr = value as u32 & 1;
                Ok(0)
            }
            // Linux initializes these banked registers; their state is not
            // needed for the single timer PPI currently injected through LR0.
            (
                true,
                0x080..=0x0ff
                | 0x100..=0x1ff
                | 0x280..=0x2ff
                | 0x400..=0x7ff
                | 0x800..=0x8ff
                | 0xc00..=0xcff,
            ) => Ok(0),
            // The virtual distributor has no shared interrupt sources. Its
            // remaining registers are reserved for future emulation, so
            // model them as RAZ/WI rather than terminating a Linux boot for
            // an otherwise harmless discovery or initialization access.
            (false, _) => Ok(0),
            (true, _) => Ok(0),
        }
    }
}

impl MmioDispatcher {
    pub fn enqueue_pl011_rx(&mut self, byte: u8) -> bool {
        self.pl011.enqueue_rx(byte)
    }

    pub fn pl011_rx_irq_pending(&self) -> bool {
        self.pl011.rx_irq_pending()
    }
    /// Emulates one access. A returned byte is forwarded by the EL2 caller to
    /// the host console only after this method succeeds.
    pub fn emulate(
        &mut self,
        access: MmioAccess,
        registers: &mut [u64; 31],
    ) -> Result<Option<u8>, MmioEmulationError> {
        if let Some(offset) = access
            .ipa
            .checked_sub(GUEST_GICD.start())
            .filter(|offset| *offset < GUEST_GICD.size())
        {
            let value = if access.register == 31 {
                0
            } else {
                registers[access.register as usize]
            };
            let result = self
                .gicd
                .access(offset, access.is_write, access.width, value)
                .map_err(|_| MmioEmulationError::GicDistributor)?;
            if !access.is_write && access.register != 31 {
                registers[access.register as usize] = if access.register_is_64bit {
                    result
                } else {
                    result as u32 as u64
                };
            }
            return Ok(None);
        }
        let offset = access
            .ipa
            .checked_sub(GUEST_PL011.start())
            .filter(|offset| *offset < GUEST_PL011.size())
            .ok_or(MmioEmulationError::UnmappedIpa(access.ipa))?;
        let value = if access.register == 31 {
            0
        } else {
            registers[access.register as usize]
        };
        match self
            .pl011
            .access(offset, access.is_write, access.width, value)?
        {
            Pl011Result::Read(value) => {
                if access.register != 31 {
                    registers[access.register as usize] = if access.register_is_64bit {
                        value
                    } else {
                        value as u32 as u64
                    };
                }
                Ok(None)
            }
            Pl011Result::Write(byte) => Ok(byte),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn data_abort_iss(is_write: bool, width_encoding: u32, register: u32) -> u64 {
        (ESR_ISV
            | (width_encoding << ESR_SAS_SHIFT)
            | (register << ESR_SRT_SHIFT)
            | if is_write { ESR_WNR } else { 0 }) as u64
    }

    #[test]
    fn decodes_access_direction_width_and_register() {
        let access =
            MmioAccess::decode_data_abort(data_abort_iss(true, 2, 7), GUEST_PL011.start()).unwrap();
        assert_eq!(
            access,
            MmioAccess {
                ipa: GUEST_PL011.start(),
                is_write: true,
                width: 4,
                register: 7,
                register_is_64bit: false
            }
        );
    }

    #[test]
    fn pl011_tx_and_status_update_guest_registers() {
        let mut dispatcher = MmioDispatcher::default();
        let mut registers = [0_u64; 31];
        registers[3] = b'A' as u64;
        let tx = dispatcher
            .emulate(
                MmioAccess::decode_data_abort(data_abort_iss(true, 0, 3), GUEST_PL011.start())
                    .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(tx, Some(b'A'));
        let status = dispatcher
            .emulate(
                MmioAccess::decode_data_abort(
                    data_abort_iss(false, 2, 5),
                    GUEST_PL011.start() + 0x18,
                )
                .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(status, None);
        assert_eq!(registers[5], (1 << 7) | (1 << 4));

        for (register, offset, expected) in [(0, 0xfe0, 0x11), (1, 0xfe4, 0x10), (2, 0xfe8, 0x04)] {
            dispatcher
                .emulate(
                    MmioAccess::decode_data_abort(
                        data_abort_iss(false, 2, register),
                        GUEST_PL011.start() + offset,
                    )
                    .unwrap(),
                    &mut registers,
                )
                .unwrap();
            assert_eq!(registers[register as usize], expected);
        }
    }

    #[test]
    fn pl011_rx_fifo_drives_status_and_masked_interrupt() {
        let mut uart = VirtualPl011::default();
        assert!(uart.enqueue_rx(b'x'));
        assert_eq!(
            uart.access(VirtualPl011::FR, false, 4, 0).unwrap(),
            Pl011Result::Read(VirtualPl011::FR_TX_READY)
        );
        assert_eq!(
            uart.access(VirtualPl011::RIS, false, 4, 0).unwrap(),
            Pl011Result::Read(VirtualPl011::RXIM as u64)
        );
        assert!(!uart.rx_irq_pending());
        uart.access(VirtualPl011::IMSC, true, 4, VirtualPl011::RXIM as u64)
            .unwrap();
        assert!(uart.rx_irq_pending());
        assert_eq!(
            uart.access(VirtualPl011::DR, false, 4, 0).unwrap(),
            Pl011Result::Read(b'x' as u64)
        );
        assert!(!uart.rx_irq_pending());
        assert_eq!(
            uart.access(VirtualPl011::FR, false, 4, 0).unwrap(),
            Pl011Result::Read(VirtualPl011::FR_TX_READY | VirtualPl011::FR_RXFE)
        );
    }

    #[test]
    fn gic_distributor_discovery_and_configuration_are_emulated() {
        let mut dispatcher = MmioDispatcher::default();
        let mut registers = [0_u64; 31];

        dispatcher
            .emulate(
                MmioAccess::decode_data_abort(
                    data_abort_iss(false, 2, 3),
                    GUEST_GICD.start() + 0x004,
                )
                .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(registers[3], 1);

        registers[4] = 1;
        dispatcher
            .emulate(
                MmioAccess::decode_data_abort(data_abort_iss(true, 2, 4), GUEST_GICD.start())
                    .unwrap(),
                &mut registers,
            )
            .unwrap();
        dispatcher
            .emulate(
                MmioAccess::decode_data_abort(data_abort_iss(false, 2, 5), GUEST_GICD.start())
                    .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(registers[5], 1);

        dispatcher
            .emulate(
                MmioAccess::decode_data_abort(
                    data_abort_iss(false, 2, 6),
                    GUEST_GICD.start() + 0x800,
                )
                .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(registers[6], 0x0101_0101);

        dispatcher
            .emulate(
                MmioAccess::decode_data_abort(
                    data_abort_iss(false, 2, 7),
                    GUEST_GICD.start() + 0xc00,
                )
                .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(registers[7], 0);

        dispatcher
            .emulate(
                MmioAccess::decode_data_abort(
                    data_abort_iss(true, 2, 7),
                    GUEST_GICD.start() + 0xc00,
                )
                .unwrap(),
                &mut registers,
            )
            .unwrap();
    }

    #[test]
    fn rejects_unknown_devices_and_tolerates_pl011_control_accesses() {
        let mut dispatcher = MmioDispatcher::default();
        let mut registers = [0_u64; 31];
        let access =
            MmioAccess::decode_data_abort(data_abort_iss(false, 2, 0), 0x0802_0000).unwrap();
        assert_eq!(
            dispatcher.emulate(access, &mut registers),
            Err(MmioEmulationError::UnmappedIpa(0x0802_0000))
        );
        let access =
            MmioAccess::decode_data_abort(data_abort_iss(false, 2, 0), GUEST_PL011.start())
                .unwrap();
        assert_eq!(dispatcher.emulate(access, &mut registers), Ok(None));
        assert_eq!(registers[0], 0);
    }
}
