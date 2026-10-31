//! Supported Armv8-A/A72 host EL2 policy. Guest execution has its own controls.
use crate::aarch64_reg::*;

pub const HCR_RW: u64 = 1 << 31;
pub const HOST_HCR: u64 = HCR_RW; // RW=1; VM, routing and trap controls clear; E2H=0.
pub const CPTR_RES1: u64 = (3 << 12) | 0x3ff;
pub const HOST_CPTR: u64 = CPTR_RES1; // TFP/TTA/TCPAC clear; allow host FP/SIMD.
pub const HOST_HSTR: u64 = 0;
pub const HOST_VTTBR: u64 = 0;
pub const HOST_CNTVOFF: u64 = 0;
pub const HOST_CNTHCTL: u64 = 3; // EL1 physical counter/timer access permitted.
pub const DISABLED_TIMER_CTL: u64 = 0;
pub const SCTLR_RES1: u64 = (3 << 28) | (3 << 22) | (1 << 18) | (1 << 16) | (1 << 11) | (3 << 4);
pub const HOST_SCTLR: u64 = SCTLR_RES1 | (1 << 0) | (1 << 2) | (1 << 3) | (1 << 12) | (1 << 19);
pub const HOST_MAIR: u64 = MAIR_EL2_VALUE;
pub const MASKED_DAIF: u64 = 0x3c0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    UnsupportedPaRange,
    InvalidRoot,
}
/// Constructed once from owned table metadata, never copied from live registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostTranslation {
    root: u64,
    tcr: u64,
}
impl HostTranslation {
    pub fn new(root: u64, pa_range: u8) -> Result<Self, PolicyError> {
        // This profile uses 4 KiB tables and at most 48-bit physical addresses.
        if pa_range > 5 {
            return Err(PolicyError::UnsupportedPaRange);
        }
        let bits = [32, 36, 40, 42, 44, 48][pa_range as usize];
        if root == 0 || root & 4095 != 0 || root >> bits != 0 {
            return Err(PolicyError::InvalidRoot);
        }
        Ok(Self {
            root,
            tcr: TCR_EL2_RES1
                | TCR_EL2_T0SZ_FOR_VA_39_BIT
                | TCR_EL2_IRGN0_MEM_WB_RA_WA
                | TCR_EL2_ORGN0_MEM_WB_RA_WA
                | TCR_EL2_SH0_INNER_SHAREABLE
                | ((pa_range as u64) << 16),
        })
    }
    pub const fn root(self) -> u64 {
        self.root
    }
    pub const fn tcr(self) -> u64 {
        self.tcr
    }
    pub const fn mair(self) -> u64 {
        HOST_MAIR
    }
    pub const fn sctlr(self) -> u64 {
        HOST_SCTLR
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_existing_board_translation_without_reserved_or_host_extension_bits() {
        let p = HostTranslation::new(0x4100000, 4).unwrap();
        assert_eq!(p.tcr(), 0x80843519);
        assert_eq!(p.mair(), 0x4ff);
        assert_eq!(p.sctlr(), 0x30cd183d);
        assert_eq!(HOST_HCR & ((1 << 34) | 1), 0); // no VHE or stage 2
        assert_eq!(HOST_CPTR & (1 << 10), 0);
        assert_eq!(HOST_CPTR & CPTR_RES1, CPTR_RES1);
        assert_eq!(HOST_SCTLR & (1 << 25), 0); // little endian
    }
    #[test]
    fn validates_root_against_each_supported_physical_address_size() {
        for (range, bits) in [32, 36, 40, 42, 44, 48].into_iter().enumerate() {
            assert!(HostTranslation::new((1 << bits) - 4096, range as u8).is_ok());
            assert_eq!(
                HostTranslation::new(1 << bits, range as u8),
                Err(PolicyError::InvalidRoot)
            );
        }
        for root in [0, 1, 0x4100001] {
            assert_eq!(HostTranslation::new(root, 2), Err(PolicyError::InvalidRoot));
        }
        for range in [6, 7, 255] {
            assert_eq!(
                HostTranslation::new(0x4100000, range),
                Err(PolicyError::UnsupportedPaRange)
            );
        }
    }
}
