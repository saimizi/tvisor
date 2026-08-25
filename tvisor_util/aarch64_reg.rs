#[inline]
pub fn bit_check(value: u64, bit: u64) -> bool {
    value & (0x1 << bit) == (0x1 << bit)
}

#[repr(i8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExceptionLevel {
    Invalid = -1,
    EL0 = 0,
    EL1 = 1,
    EL2 = 2,
    EL3 = 3,
}

impl From<u64> for ExceptionLevel {
    fn from(value: u64) -> Self {
        match value {
            0 => ExceptionLevel::EL0,
            1 => ExceptionLevel::EL1,
            2 => ExceptionLevel::EL2,
            3 => ExceptionLevel::EL3,
            _ => ExceptionLevel::Invalid,
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct CurrentEL {
    pub value: u64,
}

impl CurrentEL {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Self {
        let value: u64;
        unsafe {
            core::arch::asm!(
                "mrs {value}, CurrentEL",
                value = out(reg) value,
                options(nomem, nostack, preserves_flags),
            );
        }
        Self { value }
    }

    pub fn current_el(&self) -> ExceptionLevel {
        ((self.value >> 2) & 0b11).into()
    }
}

impl From<CurrentEL> for ExceptionLevel {
    fn from(value: CurrentEL) -> Self {
        value.current_el()
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct MpidrEl1 {
    pub value: u64,
}

impl MpidrEl1 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el = CurrentEL::dump().current_el();
        if el >= ExceptionLevel::EL1 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, MPIDR_EL1",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    pub fn affinity(&self) -> (u8, u8, u8, u8) {
        let mpidr = self.value;
        let aff0 = (mpidr & 0xff) as u8;
        let aff1 = ((mpidr >> 8) & 0xff) as u8;
        let aff2 = ((mpidr >> 16) & 0xff) as u8;
        let aff3 = ((mpidr >> 32) & 0xff) as u8;
        (aff3, aff2, aff1, aff0)
    }

    // Uniprocessor-system flag
    pub fn bit_u(&self) -> bool {
        bit_check(self.value, 30)
    }

    // Performance-interdependence indicator for PEs at affinity level 0
    pub fn bit_mt(&self) -> bool {
        bit_check(self.value, 24)
    }

    pub fn current_core(&self) -> u8 {
        self.affinity().3
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct SpSel {
    pub value: u64,
}

pub enum SpEl {
    SpEL0,
    SpEL1,
    SpEL2,
    SpEL3,
    Invalid,
}

impl core::fmt::Display for SpEl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::SpEL0 => "SP_EL0",
            Self::SpEL1 => "SP_EL1",
            Self::SpEL2 => "SP_EL2",
            Self::SpEL3 => "SP_EL3",
            Self::Invalid => "Invalid",
        };
        write!(f, "{}", s)
    }
}

impl SpSel {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL1 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, SPSel",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    pub fn sp_el(&self, current_el: ExceptionLevel) -> SpEl {
        if self.value & 0x1 == 1 {
            match current_el {
                ExceptionLevel::EL1 => SpEl::SpEL1,
                ExceptionLevel::EL2 => SpEl::SpEL2,
                ExceptionLevel::EL3 => SpEl::SpEL3,
                _ => SpEl::Invalid,
            }
        } else {
            SpEl::SpEL0
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct Sp {
    pub value: usize,
}

impl Sp {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Self {
        let value: usize;
        unsafe {
            core::arch::asm!(
                "mov {value}, sp",
                value = out(reg) value,
                options(nomem, nostack, preserves_flags),
            );
        }

        Self { value }
    }

    pub fn sp(&self) -> usize {
        self.value
    }

    pub fn align(&self) -> usize {
        self.value & 0xf
    }
}

impl Into<usize> for Sp {
    fn into(self) -> usize {
        self.value
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct SctlrEl2 {
    pub value: u64,
}

impl SctlrEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el = CurrentEL::dump().current_el();
        if el >= ExceptionLevel::EL2 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, SCTLR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }

            Some(Self { value })
        } else {
            None
        }
    }

    // EL2 stage-1 MMU enable
    pub fn bit_m(&self) -> bool {
        bit_check(self.value, 0)
    }

    // Alignment checking enable
    pub fn bit_a(&self) -> bool {
        bit_check(self.value, 1)
    }

    // Data/unified cache enable
    pub fn bit_c(&self) -> bool {
        bit_check(self.value, 2)
    }

    //EL2 stack-alignment checking enable
    pub fn bit_sa(&self) -> bool {
        bit_check(self.value, 3)
    }

    // Instruction-cache enable
    pub fn bit_i(&self) -> bool {
        bit_check(self.value, 12)
    }

    // Writable mappings execute-never
    pub fn bit_wxn(&self) -> bool {
        bit_check(self.value, 19)
    }

    pub fn bit_ee(&self) -> bool {
        bit_check(self.value, 25)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct HcrEl2 {
    pub value: u64,
}

impl HcrEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, HCR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    pub fn bit_vm(&self) -> bool {
        bit_check(self.value, 0)
    }
    pub fn bit_ptw(&self) -> bool {
        bit_check(self.value, 2)
    }
    pub fn bit_fmo(&self) -> bool {
        bit_check(self.value, 3)
    }
    pub fn bit_imo(&self) -> bool {
        bit_check(self.value, 4)
    }
    pub fn bit_amo(&self) -> bool {
        bit_check(self.value, 5)
    }
    pub fn bit_vf(&self) -> bool {
        bit_check(self.value, 6)
    }
    pub fn bit_vi(&self) -> bool {
        bit_check(self.value, 7)
    }
    pub fn bit_vse(&self) -> bool {
        bit_check(self.value, 8)
    }
    pub fn bit_twi(&self) -> bool {
        bit_check(self.value, 13)
    }
    pub fn bit_twe(&self) -> bool {
        bit_check(self.value, 14)
    }
    pub fn bit_ttlb(&self) -> bool {
        bit_check(self.value, 25)
    }
    pub fn bit_tvm(&self) -> bool {
        bit_check(self.value, 26)
    }
    pub fn bit_tge(&self) -> bool {
        bit_check(self.value, 27)
    }
    pub fn bit_hcd(&self) -> bool {
        bit_check(self.value, 29)
    }
    pub fn bit_trvm(&self) -> bool {
        bit_check(self.value, 30)
    }
    pub fn bit_rw(&self) -> bool {
        bit_check(self.value, 31)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct TcrEl2 {
    pub value: u64,
}

impl TcrEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, TCR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }
    pub fn t0sz(&self) -> u8 {
        (self.value & 0x3f) as u8
    }

    pub fn input_address_bits(&self) -> u8 {
        64 - self.t0sz()
    }

    pub fn irgn0(&self) -> u8 {
        ((self.value >> 8) & 0b11) as u8
    }

    pub fn orgn0(&self) -> u8 {
        ((self.value >> 10) & 0b11) as u8
    }

    pub fn sh0(&self) -> u8 {
        ((self.value >> 12) & 0b11) as u8
    }

    pub fn tg0(&self) -> u8 {
        ((self.value >> 14) & 0b11) as u8
    }

    pub fn ps(&self) -> u8 {
        ((self.value >> 16) & 0b111) as u8
    }

    pub fn bit_tbi(&self) -> bool {
        bit_check(self.value, 20)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct Ttbr0El2 {
    pub value: u64,
}

impl Ttbr0El2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, TTBR0_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }
    pub fn baddr(&self) -> u64 {
        self.value & 0x0000_ffff_ffff_fffe
    }

    pub fn bit_cnp(&self) -> bool {
        bit_check(self.value, 0)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct MairEl2 {
    pub value: u64,
}

impl MairEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, MAIR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }
    pub fn attributes(&self) -> [u8; 8] {
        core::array::from_fn(|index| ((self.value >> (index * 8)) & 0xff) as u8)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct VtcrEl2 {
    pub value: u64,
}

impl VtcrEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, VTCR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    // Size offset of the stage-2 input address space
    pub fn t0sz(&self) -> u8 {
        (self.value & 0x3f) as u8
    }

    // Nominal intermediate-physical-address width
    pub fn ipa_bits(&self) -> u8 {
        64 - self.t0sz()
    }

    // Starting level of the stage-2 lookup
    pub fn sl0(&self) -> u8 {
        ((self.value >> 6) & 0b11) as u8
    }

    // Inner cacheability used for stage-2 walks
    pub fn irgn0(&self) -> u8 {
        ((self.value >> 8) & 0b11) as u8
    }

    // Outer cacheability used for stage-2 walks
    pub fn orgn0(&self) -> u8 {
        ((self.value >> 10) & 0b11) as u8
    }

    // Shareability used for stage-2 walks
    pub fn sh0(&self) -> u8 {
        ((self.value >> 12) & 0b11) as u8
    }

    // Stage-2 translation granule
    pub fn tg0(&self) -> u8 {
        ((self.value >> 14) & 0b11) as u8
    }

    // Maximum physical output-address size
    pub fn ps(&self) -> u8 {
        ((self.value >> 16) & 0b111) as u8
    }

    // VMID Size (FEAT_VMID16): 0 = 8-bit VMID, 1 = 16-bit VMID
    pub fn bit_vs(&self) -> bool {
        bit_check(self.value, 19)
    }

    // 52-bit IPA and output-address support (FEAT_LPA2)
    pub fn bit_ds(&self) -> bool {
        bit_check(self.value, 32)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct VttbrEl2 {
    pub value: u64,
}

impl VttbrEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value: u64;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, VTTBR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    // Virtual Machine Identifier, bits [63:48].
    // With an 8-bit VMID (VTCR_EL2.VS == 0) only the low 8 bits hold the VMID and
    // the upper 8 bits are RES0. With FEAT_VMID16 and VTCR_EL2.VS == 1 all 16 bits
    // are used.
    pub fn vmid(&self) -> u16 {
        ((self.value >> 48) & 0xffff) as u16
    }

    // Physical base address of the starting-level stage-2 table, bits [47:1].
    // Bit 0 is CnP (FEAT_TTCNP) and is not part of the base address.
    pub fn baddr(&self) -> u64 {
        self.value & 0x0000_ffff_ffff_fffe
    }

    // Common-not-Private (FEAT_TTCNP); RES0 otherwise.
    pub fn bit_cnp(&self) -> bool {
        bit_check(self.value, 0)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct VbarEl2 {
    pub value: u64,
}

impl VbarEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();

        if el >= ExceptionLevel::EL2 {
            let value;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, VBAR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }
    // Low 11 bits. VBAR_EL2 must be 2048-byte aligned, so these are zero.
    pub fn alignment(&self) -> u64 {
        self.value & 0x7ff
    }

    pub fn is_aligned(&self) -> bool {
        self.alignment() == 0
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct Daif {
    pub value: u64,
}

impl Daif {
    // DAIF is not unconditionally readable at EL0: access
    // depends on SCTLR_EL1.UMA and traps when UMA == 0.
    // But we can't read SCTLR_EL1 if current EL is EL0,
    // so depends on `uma` from caller
    #[cfg(target_arch = "aarch64")]
    pub fn dump(uma: Option<bool>) -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL1 || (el == ExceptionLevel::EL0 && uma.is_some_and(|a| a)) {
            let value;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, DAIF",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    // Debug exceptions masked
    pub fn bit_d(&self) -> bool {
        bit_check(self.value, 9)
    }

    // SError exceptions masked
    pub fn bit_a(&self) -> bool {
        bit_check(self.value, 8)
    }

    // IRQ exceptions masked
    pub fn bit_i(&self) -> bool {
        bit_check(self.value, 7)
    }

    // FIQ exceptions masked
    pub fn bit_f(&self) -> bool {
        bit_check(self.value, 6)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct CptrEl2 {
    pub value: u64,
}

impl CptrEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, CPTR_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    // Traps FP/Advanced SIMD instructions at EL0, EL1, and EL2
    pub fn bit_tfp(&self) -> bool {
        bit_check(self.value, 10)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct CnthctlEl2 {
    pub value: u64,
}

impl CnthctlEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL2 {
            let value;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, CNTHCTL_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }

    // EL1 physical counter access enable
    pub fn bit_el1pcten(&self) -> bool {
        bit_check(self.value, 0)
    }

    // EL1 physical timer access enable
    pub fn bit_el1pcen(&self) -> bool {
        bit_check(self.value, 1)
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct CntvoffEl2 {
    pub value: u64,
}

impl CntvoffEl2 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();

        if el >= ExceptionLevel::EL2 {
            let value;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, CNTVOFF_EL2",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct IdAa64Pfr0El1 {
    pub value: u64,
}

impl IdAa64Pfr0El1 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL1 {
            let value;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, ID_AA64PFR0_EL1",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }
    // EL2 implementation support (0 = not implemented)
    pub fn el2(&self) -> u8 {
        ((self.value >> 8) & 0xf) as u8
    }

    // Floating-point support (0 = implemented, 0xf = not implemented)
    pub fn fp(&self) -> u8 {
        ((self.value >> 16) & 0xf) as u8
    }

    // Advanced SIMD support (0 = implemented, 0xf = not implemented)
    pub fn advsimd(&self) -> u8 {
        ((self.value >> 20) & 0xf) as u8
    }

    // System-register GIC CPU interface (0 = not implemented)
    pub fn gic(&self) -> u8 {
        ((self.value >> 24) & 0xf) as u8
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct IdAa64Mmfr0El1 {
    pub value: u64,
}

impl IdAa64Mmfr0El1 {
    #[cfg(target_arch = "aarch64")]
    pub fn dump() -> Option<Self> {
        let el: ExceptionLevel = CurrentEL::dump().into();
        if el >= ExceptionLevel::EL1 {
            let value;
            unsafe {
                core::arch::asm!(
                    "mrs {value}, ID_AA64MMFR0_EL1",
                    value = out(reg) value,
                    options(nomem, nostack, preserves_flags),
                );
            }
            Some(Self { value })
        } else {
            None
        }
    }
    // Implemented physical-address range
    pub fn parange(&self) -> u8 {
        (self.value & 0xf) as u8
    }

    // Supported ASID width (0 = 8-bit, 2 = 16-bit)
    pub fn asidbits(&self) -> u8 {
        ((self.value >> 4) & 0xf) as u8
    }

    // 16 KiB translation-granule support (0 = not supported)
    pub fn tgran16(&self) -> u8 {
        ((self.value >> 20) & 0xf) as u8
    }

    // 64 KiB translation-granule support (0 = supported, 0xf = not supported)
    pub fn tgran64(&self) -> u8 {
        ((self.value >> 24) & 0xf) as u8
    }

    // 4 KiB translation-granule support (0 = supported, 0xf = not supported)
    pub fn tgran4(&self) -> u8 {
        ((self.value >> 28) & 0xf) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::{CnthctlEl2, CptrEl2, Daif, IdAa64Mmfr0El1, IdAa64Pfr0El1, VtcrEl2, VttbrEl2};

    #[test]
    fn vtcr_el2_decodes_every_field() {
        // Set each field to a distinct, non-trivial value at its architectural position.
        let raw = 0b100001_u64 // T0SZ [5:0] = 33
            | (0b01_u64 << 6) // SL0 [7:6] = 1
            | (0b10_u64 << 8) // IRGN0 [9:8] = 2
            | (0b11_u64 << 10) // ORGN0 [11:10] = 3
            | (0b01_u64 << 12) // SH0 [13:12] = 1
            | (0b10_u64 << 14) // TG0 [15:14] = 2
            | (0b101_u64 << 16) // PS [18:16] = 5
            | (1_u64 << 19) // VS [19] = 1
            | (1_u64 << 32); // DS [32] = 1

        let r = VtcrEl2 { value: raw };
        assert_eq!(r.t0sz(), 33);
        assert_eq!(r.ipa_bits(), 31);
        assert_eq!(r.sl0(), 1);
        assert_eq!(r.irgn0(), 2);
        assert_eq!(r.orgn0(), 3);
        assert_eq!(r.sh0(), 1);
        assert_eq!(r.tg0(), 2);
        assert_eq!(r.ps(), 5);
        assert!(r.bit_vs());
        assert!(r.bit_ds());
    }

    #[test]
    fn vtcr_el2_fields_do_not_overlap() {
        // A value with every bit set must decode each field to its full mask width,
        // catching any accessor that reads too many (or too few) bits.
        let r = VtcrEl2 { value: u64::MAX };
        assert_eq!(r.t0sz(), 0x3f);
        assert_eq!(r.sl0(), 0b11);
        assert_eq!(r.irgn0(), 0b11);
        assert_eq!(r.orgn0(), 0b11);
        assert_eq!(r.sh0(), 0b11);
        assert_eq!(r.tg0(), 0b11);
        assert_eq!(r.ps(), 0b111);
        assert!(r.bit_vs());
        assert!(r.bit_ds());
    }

    #[test]
    fn vttbr_el2_decodes_vmid_baddr_and_cnp() {
        let vmid = 0x1234_u64;
        let baddr = 0x0000_0a5a_5a5a_u64 & 0x0000_ffff_ffff_fffe;
        // Bit 0 is CnP, not part of BADDR.
        let raw = (vmid << 48) | baddr | 1_u64;

        let r = VttbrEl2 { value: raw };
        assert_eq!(r.vmid(), 0x1234);
        assert_eq!(r.baddr(), baddr);
        assert!(r.bit_cnp());
    }

    #[test]
    fn vttbr_el2_baddr_excludes_cnp_bit() {
        // Bit 0 must never leak into BADDR, and the VMID field must never leak
        // into BADDR either.
        let r = VttbrEl2 {
            value: 0xffff_ffff_ffff_ffff,
        };
        assert_eq!(r.baddr(), 0x0000_ffff_ffff_fffe);
        assert_eq!(r.vmid(), 0xffff);
        assert!(r.bit_cnp());
    }

    #[test]
    fn daif_decodes_each_mask_bit() {
        // D/A/I/F live at bits 9/8/7/6 and must not bleed into one another.
        assert!(Daif { value: 1 << 9 }.bit_d());
        assert!(!Daif { value: 1 << 9 }.bit_a());
        assert!(Daif { value: 1 << 8 }.bit_a());
        assert!(!Daif { value: 1 << 8 }.bit_i());
        assert!(Daif { value: 1 << 7 }.bit_i());
        assert!(!Daif { value: 1 << 7 }.bit_f());
        assert!(Daif { value: 1 << 6 }.bit_f());
        assert!(!Daif { value: 1 << 6 }.bit_d());
    }

    #[test]
    fn daif_reserved_bits_are_ignored() {
        // Bits above 9 are RES0 and must not decode as any mask.
        let r = Daif { value: 1 << 63 };
        assert!(!r.bit_d());
        assert!(!r.bit_a());
        assert!(!r.bit_i());
        assert!(!r.bit_f());
    }

    #[test]
    fn cptr_el2_decodes_tfp() {
        assert!(CptrEl2 { value: 1 << 10 }.bit_tfp());
        assert!(!CptrEl2 { value: 0 }.bit_tfp());
    }

    #[test]
    fn cptr_el2_tfp_does_not_leak_from_other_bits() {
        // RES1 bits [13:12] and [9:0] are set but TFP must stay clear.
        let r = CptrEl2 {
            value: (0b11 << 12) | 0x3ff,
        };
        assert!(!r.bit_tfp());
    }
    #[test]
    fn cnthctl_el2_decodes_access_bits() {
        assert!(CnthctlEl2 { value: 1 << 0 }.bit_el1pcten());
        assert!(!CnthctlEl2 { value: 1 << 0 }.bit_el1pcen());
        assert!(CnthctlEl2 { value: 1 << 1 }.bit_el1pcen());
        assert!(!CnthctlEl2 { value: 1 << 1 }.bit_el1pcten());
    }

    #[test]
    fn cnthctl_el2_reserved_bits_do_not_leak() {
        // A high reserved bit must not decode as EL1PCTEN/EL1PCEN.
        let r = CnthctlEl2 { value: 1 << 63 };
        assert!(!r.bit_el1pcten());
        assert!(!r.bit_el1pcen());
    }
    #[test]
    fn id_aa64pfr0_el1_decodes_fields() {
        let raw = (0b0001_u64 << 8) // EL2 [11:8] = 1 (AArch64 only)
            | (0b0000_u64 << 16) // FP [19:16] = 0 (implemented)
            | (0b1111_u64 << 20) // AdvSIMD [23:20] = 0xf (not implemented)
            | (0b0011_u64 << 24); // GIC [27:24] = 3 (v4.1)

        let r = IdAa64Pfr0El1 { value: raw };
        assert_eq!(r.el2(), 1);
        assert_eq!(r.fp(), 0);
        assert_eq!(r.advsimd(), 0xf);
        assert_eq!(r.gic(), 3);
    }

    #[test]
    fn id_aa64pfr0_el1_fields_do_not_overlap() {
        let r = IdAa64Pfr0El1 { value: u64::MAX };
        assert_eq!(r.el2(), 0xf);
        assert_eq!(r.fp(), 0xf);
        assert_eq!(r.advsimd(), 0xf);
        assert_eq!(r.gic(), 0xf);
    }

    #[test]
    fn id_aa64mmfr0_el1_decodes_fields() {
        let raw = (0b0110_u64 << 0) // PARange [3:0] = 6 (52-bit)
            | (0b0010_u64 << 4) // ASIDBits [7:4] = 2 (16-bit)
            | (0b0001_u64 << 20) // TGran16 [23:20] = 1 (supported)
            | (0b0000_u64 << 24) // TGran64 [27:24] = 0 (supported)
            | (0b0000_u64 << 28); // TGran4 [31:28] = 0 (supported)

        let r = IdAa64Mmfr0El1 { value: raw };
        assert_eq!(r.parange(), 6);
        assert_eq!(r.asidbits(), 2);
        assert_eq!(r.tgran16(), 1);
        assert_eq!(r.tgran64(), 0);
        assert_eq!(r.tgran4(), 0);
    }

    #[test]
    fn id_aa64mmfr0_el1_fields_do_not_overlap() {
        let r = IdAa64Mmfr0El1 { value: u64::MAX };
        assert_eq!(r.parange(), 0xf);
        assert_eq!(r.asidbits(), 0xf);
        assert_eq!(r.tgran16(), 0xf);
        assert_eq!(r.tgran64(), 0xf);
        assert_eq!(r.tgran4(), 0xf);
    }
}
