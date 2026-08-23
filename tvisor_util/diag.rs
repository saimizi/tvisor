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
}

#[derive(Default)]
pub struct DiagState {
    pub current_el: u64,
    pub mpidr_el1: Option<MpidrEl1>,
    pub spsel: Option<u64>,
    pub sp: Option<u64>,

    pub sctlr_el2: Option<SctlrEl2>,
    pub hcr_el2: u64,
    pub tcr_el2: u64,
    pub ttbr0_el2: u64,
    pub mair_el2: u64,

    pub vtcr_el2: u64,
    pub vttbr_el2: u64,

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
        Ok(())
    }
}
