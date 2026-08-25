use crate::aarch64_reg::*;

#[derive(Default)]
pub struct DiagState {
    pub current_el: CurrentEL,
    pub mpidr_el1: Option<MpidrEl1>,
    pub spsel: Option<SpSel>,
    pub sp: Sp,

    pub sctlr_el2: Option<SctlrEl2>,
    pub hcr_el2: Option<HcrEl2>,
    pub tcr_el2: Option<TcrEl2>,
    pub ttbr0_el2: Option<Ttbr0El2>,
    pub mair_el2: Option<MairEl2>,

    pub vtcr_el2: Option<VtcrEl2>,
    pub vttbr_el2: Option<VttbrEl2>,

    pub vbar_el2: Option<VbarEl2>,
    pub daif: Option<Daif>,
    pub cptr_el2: Option<CptrEl2>,

    pub cnthctl_el2: Option<CnthctlEl2>,
    pub cntvoff_el2: Option<CntvoffEl2>,

    pub id_aa64pfr0_el1: Option<IdAa64Pfr0El1>,
    pub id_aa64mmfr0_el1: Option<IdAa64Mmfr0El1>,
}

#[cfg(target_arch = "aarch64")]
impl DiagState {
    pub fn dump() -> Self {
        Self {
            current_el: CurrentEL::dump(),
            mpidr_el1: MpidrEl1::dump(),
            spsel: SpSel::dump(),
            sp: Sp::dump(),
            sctlr_el2: SctlrEl2::dump(),
            hcr_el2: HcrEl2::dump(),

            tcr_el2: TcrEl2::dump(),
            ttbr0_el2: Ttbr0El2::dump(),
            mair_el2: MairEl2::dump(),
            vtcr_el2: VtcrEl2::dump(),
            vttbr_el2: VttbrEl2::dump(),
            vbar_el2: VbarEl2::dump(),
            daif: Daif::dump(None),
            cptr_el2: CptrEl2::dump(),
            cnthctl_el2: CnthctlEl2::dump(),
            cntvoff_el2: CntvoffEl2::dump(),
            id_aa64pfr0_el1: IdAa64Pfr0El1::dump(),
            id_aa64mmfr0_el1: IdAa64Mmfr0El1::dump(),
        }
    }
}

impl core::fmt::Display for DiagState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "CurrentEL: {:#018x}", self.current_el.value)?;
        if let Some(v) = self.mpidr_el1.as_ref() {
            let (aff3, aff2, aff1, aff0) = v.affinity();
            writeln!(
                f,
                "MPIDR_EL1: {:#018x} Aff={:x}.{:x}.{:x}.{:x} U={} MT={} Core={}",
                v.value,
                aff3,
                aff2,
                aff1,
                aff0,
                v.bit_u(),
                v.bit_mt(),
                v.current_core()
            )?;
        }
        if let Some(v) = self.spsel.as_ref() {
            writeln!(
                f,
                "    SPSel: {:#018x} {}",
                v.value,
                v.sp_el(self.current_el.current_el())
            )?;
        }

        writeln!(
            f,
            "       SP: {:#018x} align={:#x}",
            self.sp.sp(),
            self.sp.align()
        )?;

        if let Some(v) = self.sctlr_el2.as_ref() {
            writeln!(
                f,
                "SCTLR_EL2: {:#018x} M={} A={} C={} SA={} I={} WXN={} EE={}",
                v.value,
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
                v.value,
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
                v.value,
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
                v.value,
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
                v.value,
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

        if let Some(v) = self.vtcr_el2.as_ref() {
            let active = self.hcr_el2.as_ref().is_some_and(|hcr| hcr.bit_vm());
            writeln!(
                f,
                " VTCR_EL2: {:#018x} active={} T0SZ={} IPA_BITS={} SL0={:#x} IRGN0={:#x} ORGN0={:#x} SH0={:#x} TG0={:#x} PS={:#x} VS={} DS={}",
                v.value,
                active,
                v.t0sz(),
                v.ipa_bits(),
                v.sl0(),
                v.irgn0(),
                v.orgn0(),
                v.sh0(),
                v.tg0(),
                v.ps(),
                v.bit_vs(),
                v.bit_ds(),
            )?;
        }

        if let Some(v) = self.vttbr_el2.as_ref() {
            let active = self.hcr_el2.as_ref().is_some_and(|hcr| hcr.bit_vm());
            writeln!(
                f,
                "VTTBR_EL2: {:#018x} active={} VMID={:#06x} BADDR={:#014x} CnP={}",
                v.value,
                active,
                v.vmid(),
                v.baddr(),
                v.bit_cnp(),
            )?;
        }

        if let Some(v) = self.vbar_el2.as_ref() {
            writeln!(f, " VBAR_EL2: {:#018x} align={:#x}", v.value, v.alignment())?;
        }

        if let Some(v) = self.daif.as_ref() {
            writeln!(
                f,
                "     DAIF: {:#018x} D={} A={} I={} F={}",
                v.value,
                v.bit_d(),
                v.bit_a(),
                v.bit_i(),
                v.bit_f(),
            )?;
        }

        if let Some(v) = self.cptr_el2.as_ref() {
            writeln!(f, " CPTR_EL2: {:#018x} TFP={}", v.value, v.bit_tfp(),)?;
        }

        if let Some(v) = self.cnthctl_el2.as_ref() {
            writeln!(
                f,
                "CNTHCTL_EL2: {:#018x} EL1PCTEN={} EL1PCEN={}",
                v.value,
                v.bit_el1pcten(),
                v.bit_el1pcen(),
            )?;
        }

        if let Some(v) = self.cntvoff_el2.as_ref() {
            writeln!(f, "CNTVOFF_EL2: {:#018x}", v.value)?;
        }
        if let Some(v) = self.id_aa64pfr0_el1.as_ref() {
            writeln!(
                f,
                "ID_AA64PFR0_EL1: {:#018x} EL2={:#x} FP={:#x} AdvSIMD={:#x} GIC={:#x}",
                v.value,
                v.el2(),
                v.fp(),
                v.advsimd(),
                v.gic(),
            )?;
        }

        if let Some(v) = self.id_aa64mmfr0_el1.as_ref() {
            writeln!(
                f,
                "ID_AA64MMFR0_EL1: {:#018x} PARange={:#x} ASIDBits={:#x} TGran4={:#x} TGran16={:#x} TGran64={:#x}",
                v.value,
                v.parange(),
                v.asidbits(),
                v.tgran4(),
                v.tgran16(),
                v.tgran64(),
            )?;
        }
        Ok(())
    }
}
