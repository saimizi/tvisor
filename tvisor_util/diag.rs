enum DiagStateReg {
    CurrentEl,
    MpidrEl1,
    SpSel,
    Sp,
}

#[derive(Default)]
pub struct DiagState {
    pub current_el: u64,
    pub mpidr_el1: u64,
    pub spsel: u64,
    pub sp: u64,

    pub sctlr_el2: u64,
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
    fn dump_register(register: DiagStateReg) -> u64 {
        let result: u64;
        unsafe {
            let value: u64;
            match register {
                DiagStateReg::CurrentEl => {
                    core::arch::asm!(
                        "mrs {value}, CurrentEL",
                        value = out(reg) value,
                        options(nomem, nostack, preserves_flags),
                    );

                    result = (value >> 2) & 0b11;
                }

                DiagStateReg::MpidrEl1 => {
                    core::arch::asm!(
                        "mrs {value}, MPIDR_EL1",
                        value = out(reg) value,
                        options(nomem, nostack, preserves_flags),
                    );
                    result = value;
                }

                DiagStateReg::SpSel => {
                    core::arch::asm!(
                        "mrs {value}, SPSel",
                        value = out(reg) value,
                        options(nomem, nostack, preserves_flags),
                    );
                    result = value & 0x1;
                }

                DiagStateReg::Sp => {
                    core::arch::asm!(
                        "mov {value}, sp",
                        value = out(reg) value,
                        options(nomem, nostack, preserves_flags),
                    );
                    result = value;
                }
            }
        }

        result
    }

    pub fn dump() -> Self {
        Self {
            current_el: DiagState::dump_register(DiagStateReg::CurrentEl),
            mpidr_el1: DiagState::dump_register(DiagStateReg::MpidrEl1),
            spsel: DiagState::dump_register(DiagStateReg::SpSel),
            sp: DiagState::dump_register(DiagStateReg::Sp),
            ..Default::default()
        }
    }

    pub fn mpidr_el1_affinity(&self) -> (u8, u8, u8, u8) {
        let mpidr = self.mpidr_el1;
        let aff0 = (mpidr & 0xff) as u8;
        let aff1 = ((mpidr >> 8) & 0xff) as u8;
        let aff2 = ((mpidr >> 16) & 0xff) as u8;
        let aff3 = ((mpidr >> 32) & 0xff) as u8;
        (aff3, aff2, aff1, aff0)
    }

    pub fn mpidr_el1_u(&self) -> u8 {
        ((self.mpidr_el1 >> 30) & 0b01) as u8
    }

    pub fn mpidr_el1_mt(&self) -> u8 {
        ((self.mpidr_el1 >> 24) & 0b01) as u8
    }

    // aff0 is the current core.
    pub fn current_core(&self) -> u8 {
        self.mpidr_el1_affinity().3
    }
}

impl core::fmt::Display for DiagState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "CurrentEL: {:#018x}", self.current_el)?;
        writeln!(f, "MPIDR_EL1: {:#018x}", self.mpidr_el1)?;
        let (aff3, aff2, aff1, aff0) = self.mpidr_el1_affinity();
        writeln!(f, "      Aff: {:x}.{:x}.{:x}.{:x}", aff3, aff2, aff1, aff0)?;
        writeln!(f, "        U: {}", self.mpidr_el1_u())?;
        writeln!(f, "       MT: {}", self.mpidr_el1_mt())?;
        let el_str = if self.spsel & 0b1 == 1 {
            match self.current_el {
                1 => "SP_EL1",
                2 => "SP_EL2",
                3 => "SP_EL3",
                _ => "INVALID",
            }
        } else {
            "SP_EL0"
        };
        writeln!(f, "    SPSel: {:#018x} {}", self.spsel, el_str)?;
        writeln!(f, "       SP: {:#018x} align={:#x}", self.sp, self.sp & 0xf)?;
        Ok(())
    }
}
