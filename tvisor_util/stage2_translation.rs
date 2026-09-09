//! Stage-2 translation tables used by tvisor for guest EL1/EL0 execution.
//!
//! Phase 9 uses a 39-bit Intermediate Physical Address (IPA) space, a 4 KiB
//! translation granule, and a Level 1 starting lookup:
//!
//! ```text
//! IPA[38:30]   IPA[29:21]   IPA[20:12]   IPA[11:0]
//!  L1 index     L2 index     L3 index    Page offset
//!   9 bits       9 bits       9 bits       12 bits
//! ```
//!
//! With `T0SZ=25` (39 bits) and `SL0=0b01` (Level 1 start), the root table is a
//! single standard 4 KiB page containing 512 64-bit descriptors.
//!
//! In Phase 9, only 4 KiB leaf page descriptors at Level 3 are constructed.
//! Block descriptors at Level 1 and Level 2 are deferred to future phases.
//!
//! Leaf descriptor bit encoding:
//! ```text
//! Bit(s)   Field      Policy
//! 0        Valid      1 for every populated descriptor
//! 1        Type       1 for L3 4 KiB page (and table at L1/L2)
//! [5:2]    MemAttr    0b1111 = Normal Inner/Outer WB/WA; 0b0001 = Device-nGnRE
//! [7:6]    S2AP       00 = None, 01 = RO, 10 = WO, 11 = RW
//! [9:8]    SH         11 = Inner Shareable for Normal; 00 = Non-shareable for Device
//! 10       AF         1 (Access Flag, set to prevent initial AF fault)
//! [54:53]  XN         00 = Executable; 10 = Execute-Never (XN)
//! ```

use crate::el2_translation::{TranslationError, pa_bits_from_pa_range};
use crate::is_page_aligned;

pub const IPA_BITS: u8 = 39;

pub const L1_SHIFT: u32 = 30;
pub const L2_SHIFT: u32 = 21;
pub const L3_SHIFT: u32 = 12;
pub const ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;

pub const VALID: u64 = 1 << 0;
pub const TABLE_OR_PAGE: u64 = 1 << 1;
pub const MEM_ATTR_NORMAL_WB_WA: u64 = 0b1111 << 2;
pub const MEM_ATTR_DEVICE_NGNRE: u64 = 0b0001 << 2;
pub const S2AP_NONE: u64 = 0b00 << 6;
pub const S2AP_READ_ONLY: u64 = 0b01 << 6;
pub const S2AP_WRITE_ONLY: u64 = 0b10 << 6;
pub const S2AP_READ_WRITE: u64 = 0b11 << 6;
pub const SH_NONE: u64 = 0b00 << 8;
pub const SH_INNER: u64 = 0b11 << 8;
pub const ACCESS_FLAG: u64 = 1 << 10;
pub const XN_EXEC: u64 = 0b00 << 53;
pub const XN_NON_EXEC: u64 = 0b10 << 53;

pub const VTCR_EL2_T0SZ_39_BIT: u64 = 25;
pub const VTCR_EL2_SL0_LEVEL_1: u64 = 0b01 << 6;
pub const VTCR_EL2_IRGN0_NORMAL_WB_WA: u64 = 0b01 << 8;
pub const VTCR_EL2_ORGN0_NORMAL_WB_WA: u64 = 0b01 << 10;
pub const VTCR_EL2_SH0_INNER: u64 = 0b11 << 12;
pub const VTCR_EL2_TG0_4KB: u64 = 0b00 << 14;
pub const VTCR_EL2_RES1: u64 = 1 << 31;

pub const HCR_EL2_VM: u64 = 1 << 0;
pub const HCR_EL2_SWIO: u64 = 1 << 1;
pub const HCR_EL2_TSC: u64 = 1 << 19;
pub const HCR_EL2_RW: u64 = 1 << 31;
pub const HCR_EL2_STAGE2_VALUE: u64 = HCR_EL2_RW | HCR_EL2_TSC | HCR_EL2_SWIO | HCR_EL2_VM;

pub const CPTR_EL2_TFP: u64 = 1 << 10;
pub const CPTR_EL2_RES1: u64 = (0b11 << 12) | 0x3ff;
/// CPTR_EL2 value used while tvisor executes at EL2. TFP remains clear because
/// Rust and compiler-generated routines may use FP/Advanced SIMD instructions.
pub const CPTR_EL2_STAGE2_VALUE: u64 = CPTR_EL2_RES1;

/// Virtual MPIDR_EL1 for vCPU 0 on a virtual uniprocessor system.
/// Bit 31 = RES1 (1)
/// Bit 30 = U (1, uniprocessor)
/// Bits [23:0] = 0 (Affinity 0.0.0)
pub const VMPIDR_EL2_VCPU0: u64 = (1 << 31) | (1 << 30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage2MemoryType {
    NormalWbWa,
    DeviceNgNre,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage2Access {
    None,
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage2Exec {
    Executable,
    ExecuteNever,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage2RegisterValues {
    pub vtcr_el2: u64,
    pub vttbr_el2: u64,
    pub hcr_el2: u64,
    pub cptr_el2: u64,
    pub vmpidr_el2: u64,
}

pub fn encode_l3_page_descriptor(
    pa: u64,
    mem_type: Stage2MemoryType,
    access: Stage2Access,
    exec: Stage2Exec,
) -> u64 {
    let mem_attr = match mem_type {
        Stage2MemoryType::NormalWbWa => MEM_ATTR_NORMAL_WB_WA,
        Stage2MemoryType::DeviceNgNre => MEM_ATTR_DEVICE_NGNRE,
    };
    let s2ap = match access {
        Stage2Access::None => S2AP_NONE,
        Stage2Access::ReadOnly => S2AP_READ_ONLY,
        Stage2Access::WriteOnly => S2AP_WRITE_ONLY,
        Stage2Access::ReadWrite => S2AP_READ_WRITE,
    };
    let sh = match mem_type {
        Stage2MemoryType::NormalWbWa => SH_INNER,
        Stage2MemoryType::DeviceNgNre => SH_NONE,
    };
    let xn = match exec {
        Stage2Exec::Executable => XN_EXEC,
        Stage2Exec::ExecuteNever => XN_NON_EXEC,
    };

    VALID | TABLE_OR_PAGE | mem_attr | s2ap | sh | ACCESS_FLAG | xn | (pa & ADDRESS_MASK)
}

pub fn stage2_register_values(
    vm_id: u8,
    root_table_pa: u64,
    pa_range: u8,
) -> Result<Stage2RegisterValues, TranslationError> {
    let pa_bits = pa_bits_from_pa_range(pa_range)?;
    let max_root_pa = (1_u64 << pa_bits) - 1;
    if !is_page_aligned(root_table_pa as usize) || root_table_pa > max_root_pa {
        return Err(TranslationError::InvalidTableBase);
    }

    let ps_field = ((pa_range & 0x7) as u64) << 16;
    let vtcr_el2 = VTCR_EL2_RES1
        | VTCR_EL2_TG0_4KB
        | VTCR_EL2_SH0_INNER
        | VTCR_EL2_ORGN0_NORMAL_WB_WA
        | VTCR_EL2_IRGN0_NORMAL_WB_WA
        | VTCR_EL2_SL0_LEVEL_1
        | ps_field
        | VTCR_EL2_T0SZ_39_BIT;

    let vttbr_el2 = ((vm_id as u64) << 48) | (root_table_pa & ADDRESS_MASK);

    Ok(Stage2RegisterValues {
        vtcr_el2,
        vttbr_el2,
        hcr_el2: HCR_EL2_STAGE2_VALUE,
        cptr_el2: CPTR_EL2_STAGE2_VALUE,
        vmpidr_el2: VMPIDR_EL2_VCPU0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage2_register_values_encodes_expected_fields() {
        let root_pa = 0x0500_0000;
        let regs = stage2_register_values(1, root_pa, 0b0010).expect("valid regs");

        assert_eq!(regs.vtcr_el2 & 0x3f, 25); // T0SZ = 25 (39 bits)
        assert_eq!((regs.vtcr_el2 >> 6) & 0x3, 0b01); // SL0 = Level 1
        assert_eq!((regs.vtcr_el2 >> 14) & 0x3, 0b00); // TG0 = 4 KiB
        assert_eq!((regs.vtcr_el2 >> 16) & 0x7, 0b0010); // PS = 40 bits
        assert_eq!((regs.vtcr_el2 >> 31) & 0x1, 1); // RES1

        assert_eq!((regs.vttbr_el2 >> 48) & 0xff, 1); // VMID = 1
        assert_eq!(regs.vttbr_el2 & ADDRESS_MASK, root_pa);

        assert_eq!(regs.hcr_el2 & HCR_EL2_VM, HCR_EL2_VM);
        assert_eq!(regs.hcr_el2 & HCR_EL2_RW, HCR_EL2_RW);
        assert_eq!(regs.cptr_el2 & CPTR_EL2_TFP, 0);
        assert_eq!(regs.vmpidr_el2, 0xC000_0000);
    }

    #[test]
    fn stage2_encodes_device_and_readonly_attributes() {
        let desc = encode_l3_page_descriptor(
            0x2000_0000,
            Stage2MemoryType::DeviceNgNre,
            Stage2Access::ReadOnly,
            Stage2Exec::ExecuteNever,
        );
        assert_eq!(desc & VALID, VALID);
        assert_eq!(desc & TABLE_OR_PAGE, TABLE_OR_PAGE);
        assert_eq!(desc & (0b1111 << 2), MEM_ATTR_DEVICE_NGNRE);
        assert_eq!(desc & (0b11 << 6), S2AP_READ_ONLY);
        assert_eq!(desc & (0b11 << 8), SH_NONE);
        assert_eq!(desc & ACCESS_FLAG, ACCESS_FLAG);
        assert_eq!(desc & (0b11 << 53), XN_NON_EXEC);
        assert_eq!(desc & ADDRESS_MASK, 0x2000_0000);
    }
}
