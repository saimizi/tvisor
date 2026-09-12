//! Trapped stage-2 MMIO decoding and the Phase 10 virtual PL011.

use core::fmt;

/// Guest IPA of the first emulated device page.
pub const VIRTUAL_PL011_IPA: u64 = 0x0900_0000;
pub const VIRTUAL_PL011_SIZE: u64 = 0x1000;

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

/// TX-only, polled subset of an emulated PrimeCell PL011.
#[derive(Debug, Default)]
pub struct VirtualPl011;

impl VirtualPl011 {
    const DR: u64 = 0x00;
    const FR: u64 = 0x18;
    const IBRD: u64 = 0x24;
    const FBRD: u64 = 0x28;
    const LCR_H: u64 = 0x2c;
    const CR: u64 = 0x30;
    const IMSC: u64 = 0x38;
    const ICR: u64 = 0x44;
    // TX FIFO empty and RX FIFO empty. In particular TXFF is clear.
    const FR_TX_READY: u64 = (1 << 7) | (1 << 4);

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
                Self::IBRD | Self::FBRD | Self::LCR_H | Self::CR | Self::IMSC | Self::ICR => {
                    Ok(Pl011Result::Write(None))
                }
                _ => Err(Pl011Error::UnsupportedWrite(offset)),
            }
        } else {
            match offset {
                Self::FR => Ok(Pl011Result::Read(Self::FR_TX_READY)),
                _ => Err(Pl011Error::UnsupportedRead(offset)),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioEmulationError {
    UnmappedIpa(u64),
    Pl011(Pl011Error),
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
        }
    }
}

/// Generic trapped-MMIO dispatcher. Devices never receive host physical MMIO.
#[derive(Debug, Default)]
pub struct MmioDispatcher {
    pl011: VirtualPl011,
}

impl MmioDispatcher {
    /// Emulates one access. A returned byte is forwarded by the EL2 caller to
    /// the host console only after this method succeeds.
    pub fn emulate(
        &mut self,
        access: MmioAccess,
        registers: &mut [u64; 31],
    ) -> Result<Option<u8>, MmioEmulationError> {
        let offset = access
            .ipa
            .checked_sub(VIRTUAL_PL011_IPA)
            .filter(|offset| *offset < VIRTUAL_PL011_SIZE)
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
            MmioAccess::decode_data_abort(data_abort_iss(true, 2, 7), VIRTUAL_PL011_IPA).unwrap();
        assert_eq!(
            access,
            MmioAccess {
                ipa: VIRTUAL_PL011_IPA,
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
                MmioAccess::decode_data_abort(data_abort_iss(true, 0, 3), VIRTUAL_PL011_IPA)
                    .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(tx, Some(b'A'));
        let status = dispatcher
            .emulate(
                MmioAccess::decode_data_abort(
                    data_abort_iss(false, 2, 5),
                    VIRTUAL_PL011_IPA + 0x18,
                )
                .unwrap(),
                &mut registers,
            )
            .unwrap();
        assert_eq!(status, None);
        assert_eq!(registers[5], (1 << 7) | (1 << 4));
    }

    #[test]
    fn rejects_unknown_devices_and_unsupported_pl011_accesses() {
        let mut dispatcher = MmioDispatcher::default();
        let mut registers = [0_u64; 31];
        let access =
            MmioAccess::decode_data_abort(data_abort_iss(false, 2, 0), 0x0800_0000).unwrap();
        assert_eq!(
            dispatcher.emulate(access, &mut registers),
            Err(MmioEmulationError::UnmappedIpa(0x0800_0000))
        );
        let access =
            MmioAccess::decode_data_abort(data_abort_iss(false, 2, 0), VIRTUAL_PL011_IPA).unwrap();
        assert_eq!(
            dispatcher.emulate(access, &mut registers),
            Err(MmioEmulationError::Pl011(Pl011Error::UnsupportedRead(0)))
        );
    }
}
