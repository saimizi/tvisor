#[inline]
pub fn bit_check(value: u64, bit: u64) -> bool {
    value & (0x1 << bit) == (0x1 << bit)
}

#[derive(Default)]
pub struct SctlrEl2 {
    pub sctlr_el2: u64,
}

impl SctlrEl2 {
    // EL2 stage-1 MMU enable
    pub fn bit_m(&self) -> bool {
        bit_check(self.sctlr_el2, 0)
    }

    // Alignment checking enable
    pub fn bit_a(&self) -> bool {
        bit_check(self.sctlr_el2, 1)
    }

    // Data/unified cache enable
    pub fn bit_c(&self) -> bool {
        bit_check(self.sctlr_el2, 2)
    }

    //EL2 stack-alignment checking enable
    pub fn bit_sa(&self) -> bool {
        bit_check(self.sctlr_el2, 3)
    }

    // Instruction-cache enable
    pub fn bit_i(&self) -> bool {
        bit_check(self.sctlr_el2, 12)
    }

    // Writable mappings execute-never
    pub fn bit_wxn(&self) -> bool {
        bit_check(self.sctlr_el2, 19)
    }

    pub fn bit_ee(&self) -> bool {
        bit_check(self.sctlr_el2, 25)
    }
}

#[derive(Default)]
pub struct HcrEl2 {
    pub hcr_el2: u64,
}

impl HcrEl2 {
    pub fn bit_vm(&self) -> bool {
        bit_check(self.hcr_el2, 0)
    }
    pub fn bit_ptw(&self) -> bool {
        bit_check(self.hcr_el2, 2)
    }
    pub fn bit_fmo(&self) -> bool {
        bit_check(self.hcr_el2, 3)
    }
    pub fn bit_imo(&self) -> bool {
        bit_check(self.hcr_el2, 4)
    }
    pub fn bit_amo(&self) -> bool {
        bit_check(self.hcr_el2, 5)
    }
    pub fn bit_vf(&self) -> bool {
        bit_check(self.hcr_el2, 6)
    }
    pub fn bit_vi(&self) -> bool {
        bit_check(self.hcr_el2, 7)
    }
    pub fn bit_vse(&self) -> bool {
        bit_check(self.hcr_el2, 8)
    }
    pub fn bit_twi(&self) -> bool {
        bit_check(self.hcr_el2, 13)
    }
    pub fn bit_twe(&self) -> bool {
        bit_check(self.hcr_el2, 14)
    }
    pub fn bit_ttlb(&self) -> bool {
        bit_check(self.hcr_el2, 25)
    }
    pub fn bit_tvm(&self) -> bool {
        bit_check(self.hcr_el2, 26)
    }
    pub fn bit_tge(&self) -> bool {
        bit_check(self.hcr_el2, 27)
    }
    pub fn bit_hcd(&self) -> bool {
        bit_check(self.hcr_el2, 29)
    }
    pub fn bit_trvm(&self) -> bool {
        bit_check(self.hcr_el2, 30)
    }
    pub fn bit_rw(&self) -> bool {
        bit_check(self.hcr_el2, 31)
    }
}

#[derive(Default)]
pub struct TcrEl2 {
    pub tcr_el2: u64,
}

impl TcrEl2 {
    pub fn t0sz(&self) -> u8 {
        (self.tcr_el2 & 0x3f) as u8
    }

    pub fn input_address_bits(&self) -> u8 {
        64 - self.t0sz()
    }

    pub fn irgn0(&self) -> u8 {
        ((self.tcr_el2 >> 8) & 0b11) as u8
    }

    pub fn orgn0(&self) -> u8 {
        ((self.tcr_el2 >> 10) & 0b11) as u8
    }

    pub fn sh0(&self) -> u8 {
        ((self.tcr_el2 >> 12) & 0b11) as u8
    }

    pub fn tg0(&self) -> u8 {
        ((self.tcr_el2 >> 14) & 0b11) as u8
    }

    pub fn ps(&self) -> u8 {
        ((self.tcr_el2 >> 16) & 0b111) as u8
    }

    pub fn bit_tbi(&self) -> bool {
        bit_check(self.tcr_el2, 20)
    }
}

#[derive(Default)]
pub struct Ttbr0El2 {
    pub ttbr0_el2: u64,
}

impl Ttbr0El2 {
    pub fn baddr(&self) -> u64 {
        self.ttbr0_el2 & 0x0000_ffff_ffff_fffe
    }

    pub fn bit_cnp(&self) -> bool {
        bit_check(self.ttbr0_el2, 0)
    }
}

#[derive(Default)]
pub struct MairEl2 {
    pub mair_el2: u64,
}

impl MairEl2 {
    pub fn attributes(&self) -> [u8; 8] {
        core::array::from_fn(|index| ((self.mair_el2 >> (index * 8)) & 0xff) as u8)
    }
}

#[derive(Default)]
pub struct MpidrEl1 {
    pub mpidr_el1: u64,
}

impl MpidrEl1 {
    pub fn affinity(&self) -> (u8, u8, u8, u8) {
        let mpidr = self.mpidr_el1;
        let aff0 = (mpidr & 0xff) as u8;
        let aff1 = ((mpidr >> 8) & 0xff) as u8;
        let aff2 = ((mpidr >> 16) & 0xff) as u8;
        let aff3 = ((mpidr >> 32) & 0xff) as u8;
        (aff3, aff2, aff1, aff0)
    }

    // Uniprocessor-system flag
    pub fn bit_u(&self) -> bool {
        bit_check(self.mpidr_el1, 30)
    }

    // Performance-interdependence indicator for PEs at affinity level 0
    pub fn bit_mt(&self) -> bool {
        bit_check(self.mpidr_el1, 24)
    }

    pub fn current_core(&self) -> u8 {
        self.affinity().3
    }
}

enum DiagStateReg {
    CurrentEl,
    MpidrEl1,
    SpSel,
    Sp,
    SctlrEl2,
    HcrEl2,
    TcrEl2,
    Ttbr0El2,
    MairEl2,
    VtcrEl2,
    VttbrEl2,
}

#[derive(Default)]
pub struct DiagState {
    pub current_el: u64,
    pub mpidr_el1: Option<MpidrEl1>,
    pub spsel: Option<u64>,
    pub sp: Option<u64>,

    pub sctlr_el2: Option<SctlrEl2>,
    pub hcr_el2: Option<HcrEl2>,
    pub tcr_el2: Option<TcrEl2>,
    pub ttbr0_el2: Option<Ttbr0El2>,
    pub mair_el2: Option<MairEl2>,

    pub vtcr_el2: Option<u64>,
    pub vttbr_el2: Option<u64>,

    pub vbar_el2: u64,
    pub daif: u64,
    pub cptr_el2: u64,

    pub cnthctl_el2: u64,
    pub cntvoff_el2: u64,

    pub id_aa64pfr0_el1: u64,
    pub id_aa64mmfr0_el1: u64,
}

impl DiagState {
    fn dump_register(register: DiagStateReg, el: Option<u64>) -> Option<u64> {
        let result;
        unsafe {
            let value: u64;
            match register {
                DiagStateReg::CurrentEl => {
                    core::arch::asm!(
                        "mrs {value}, CurrentEL",
                        value = out(reg) value,
                        options(nomem, nostack, preserves_flags),
                    );

                    result = Some((value >> 2) & 0b11);
                }

                DiagStateReg::MpidrEl1 => {
                    if el == Some(3) || el == Some(2) || el == Some(1) {
                        core::arch::asm!(
                            "mrs {value}, MPIDR_EL1",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None
                    }
                }

                DiagStateReg::SpSel => {
                    if el == Some(3) || el == Some(2) || el == Some(1) {
                        core::arch::asm!(
                            "mrs {value}, SPSel",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value & 0x1);
                    } else {
                        result = None
                    }
                }

                DiagStateReg::Sp => {
                    core::arch::asm!(
                        "mov {value}, sp",
                        value = out(reg) value,
                        options(nomem, nostack, preserves_flags),
                    );
                    result = Some(value);
                }

                DiagStateReg::SctlrEl2 => {
                    if el == Some(2) || el == Some(3) {
                        core::arch::asm!(
                            "mrs {value}, SCTLR_EL2",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None;
                    }
                }

                DiagStateReg::HcrEl2 => {
                    if el == Some(2) || el == Some(3) {
                        core::arch::asm!(
                            "mrs {value}, HCR_EL2",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None;
                    }
                }

                DiagStateReg::TcrEl2 => {
                    if el == Some(2) || el == Some(3) {
                        core::arch::asm!(
                            "mrs {value}, TCR_EL2",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None;
                    }
                }

                DiagStateReg::Ttbr0El2 => {
                    if el == Some(2) || el == Some(3) {
                        core::arch::asm!(
                            "mrs {value}, TTBR0_EL2",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None;
                    }
                }

                DiagStateReg::MairEl2 => {
                    if el == Some(2) || el == Some(3) {
                        core::arch::asm!(
                            "mrs {value}, MAIR_EL2",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None;
                    }
                }

                DiagStateReg::VtcrEl2 => {
                    if el == Some(2) || el == Some(3) {
                        core::arch::asm!(
                            "mrs {value}, VTCR_EL2",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None;
                    }
                }

                DiagStateReg::VttbrEl2 => {
                    if el == Some(2) || el == Some(3) {
                        core::arch::asm!(
                            "mrs {value}, VTTBR_EL2",
                            value = out(reg) value,
                            options(nomem, nostack, preserves_flags),
                        );
                        result = Some(value);
                    } else {
                        result = None;
                    }
                }
            }
        }

        result
    }

    pub fn dump() -> Self {
        // CurrentEL is readable from all ELs
        let current_el = DiagState::dump_register(DiagStateReg::CurrentEl, None).unwrap();

        Self {
            current_el,
            mpidr_el1: DiagState::dump_register(DiagStateReg::MpidrEl1, Some(current_el))
                .map(|v| MpidrEl1 { mpidr_el1: v }),
            spsel: DiagState::dump_register(DiagStateReg::SpSel, Some(current_el)),
            sp: DiagState::dump_register(DiagStateReg::Sp, Some(current_el)),
            sctlr_el2: DiagState::dump_register(DiagStateReg::SctlrEl2, Some(current_el))
                .map(|v| SctlrEl2 { sctlr_el2: v }),
            hcr_el2: DiagState::dump_register(DiagStateReg::HcrEl2, Some(current_el))
                .map(|v| HcrEl2 { hcr_el2: v }),
            tcr_el2: DiagState::dump_register(DiagStateReg::TcrEl2, Some(current_el))
                .map(|v| TcrEl2 { tcr_el2: v }),
            ttbr0_el2: DiagState::dump_register(DiagStateReg::Ttbr0El2, Some(current_el))
                .map(|v| Ttbr0El2 { ttbr0_el2: v }),
            mair_el2: DiagState::dump_register(DiagStateReg::MairEl2, Some(current_el))
                .map(|v| MairEl2 { mair_el2: v }),
            vtcr_el2: DiagState::dump_register(DiagStateReg::VtcrEl2, Some(current_el)),
            vttbr_el2: DiagState::dump_register(DiagStateReg::VttbrEl2, Some(current_el)),
            ..Default::default()
        }
    }
}

impl core::fmt::Display for DiagState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "CurrentEL: {:#018x}", self.current_el)?;
        if let Some(v) = self.mpidr_el1.as_ref() {
            let (aff3, aff2, aff1, aff0) = v.affinity();
            writeln!(
                f,
                "MPIDR_EL1: {:#018x} Aff={:x}.{:x}.{:x}.{:x} U={} MT={} Core={}",
                v.mpidr_el1,
                aff3,
                aff2,
                aff1,
                aff0,
                v.bit_u(),
                v.bit_mt(),
                v.current_core()
            )?;
        }
        if let Some(v) = self.spsel {
            let el_str = if v & 0b1 == 1 {
                match self.current_el {
                    1 => "SP_EL1",
                    2 => "SP_EL2",
                    3 => "SP_EL3",
                    _ => "INVALID",
                }
            } else {
                "SP_EL0"
            };
            writeln!(f, "    SPSel: {:#018x} {}", v, el_str)?;
        }

        if let Some(v) = self.sp {
            writeln!(f, "       SP: {:#018x} align={:#x}", v, v & 0xf)?;
        }

        if let Some(v) = self.sctlr_el2.as_ref() {
            writeln!(
                f,
                "SCTLR_EL2: {:#018x} M={} A={} C={} SA={} I={} WXN={} EE={}",
                v.sctlr_el2,
                v.bit_m(),
                v.bit_a(),
                v.bit_c(),
                v.bit_sa(),
                v.bit_i(),
                v.bit_wxn(),
                v.bit_ee(),
            )?;
        }

        if let Some(v) = self.hcr_el2.as_ref() {
            writeln!(
                f,
                "  HCR_EL2: {:#018x} VM={} PTW={} FMO={} IMO={} AMO={} VF={} VI={} VSE={} TWI={} TWE={} TTLB={} TVM={} TGE={} HCD={} TRVM={} RW={}",
                v.hcr_el2,
                v.bit_vm(),
                v.bit_ptw(),
                v.bit_fmo(),
                v.bit_imo(),
                v.bit_amo(),
                v.bit_vf(),
                v.bit_vi(),
                v.bit_vse(),
                v.bit_twi(),
                v.bit_twe(),
                v.bit_ttlb(),
                v.bit_tvm(),
                v.bit_tge(),
                v.bit_hcd(),
                v.bit_trvm(),
                v.bit_rw(),
            )?;
        }

        if let Some(v) = self.tcr_el2.as_ref() {
            let active = self.sctlr_el2.as_ref().is_some_and(|sctlr| sctlr.bit_m());
            writeln!(
                f,
                "  TCR_EL2: {:#018x} active={} T0SZ={} VA_BITS={} IRGN0={:#x} ORGN0={:#x} SH0={:#x} TG0={:#x} PS={:#x} TBI={}",
                v.tcr_el2,
                active,
                v.t0sz(),
                v.input_address_bits(),
                v.irgn0(),
                v.orgn0(),
                v.sh0(),
                v.tg0(),
                v.ps(),
                v.bit_tbi(),
            )?;
        }

        if let Some(v) = self.ttbr0_el2.as_ref() {
            let active = self.sctlr_el2.as_ref().is_some_and(|sctlr| sctlr.bit_m());
            writeln!(
                f,
                "TTBR0_EL2: {:#018x} active={} BADDR={:#014x} CnP={}",
                v.ttbr0_el2,
                active,
                v.baddr(),
                v.bit_cnp(),
            )?;
        }

        if let Some(v) = self.mair_el2.as_ref() {
            let active = self.sctlr_el2.as_ref().is_some_and(|sctlr| sctlr.bit_m());
            let attr = v.attributes();
            writeln!(
                f,
                " MAIR_EL2: {:#018x} active={} Attr0={:#04x} Attr1={:#04x} Attr2={:#04x} Attr3={:#04x} Attr4={:#04x} Attr5={:#04x} Attr6={:#04x} Attr7={:#04x}",
                v.mair_el2,
                active,
                attr[0],
                attr[1],
                attr[2],
                attr[3],
                attr[4],
                attr[5],
                attr[6],
                attr[7],
            )?;
        }

        if let Some(v) = self.vtcr_el2 {
            let active = self.hcr_el2.as_ref().is_some_and(|hcr| hcr.bit_vm());
            writeln!(f, " VTCR_EL2: {:#018x} active={}", v, active)?;
        }

        if let Some(v) = self.vttbr_el2 {
            let active = self.hcr_el2.as_ref().is_some_and(|hcr| hcr.bit_vm());
            writeln!(f, "VTTBR_EL2: {:#018x} active={}", v, active)?;
        }
        Ok(())
    }
}
