//! vCPU state, pCPU-local world-switch state, and exit handling.

use core::arch::global_asm;
use tvisor_util::mmio::{MmioAccess, MmioDecodeError, MmioDispatcher, MmioEmulationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcpuMmioError {
    Decode(MmioDecodeError),
    Emulation(MmioEmulationError),
    ProgramCounterOverflow,
}

impl core::fmt::Display for VcpuMmioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Decode(error) => write!(f, "cannot decode trapped MMIO: {error}"),
            Self::Emulation(error) => write!(f, "cannot emulate trapped MMIO: {error}"),
            Self::ProgramCounterOverflow => {
                f.write_str("cannot advance guest PC after MMIO emulation")
            }
        }
    }
}

#[repr(C, align(16))]
#[derive(Debug, Clone)]
pub struct VcpuContext {
    /// General-purpose registers x0 through x30
    pub x: [u64; 31],
    /// Stack pointers for EL0 and EL1
    pub sp_el0: u64,
    pub sp_el1: u64,
    /// Exception link register (guest PC)
    pub elr_el2: u64,
    /// Saved program status register (guest PSTATE)
    pub spsr_el2: u64,
    /// EL1 System control registers
    pub sctlr_el1: u64,
    pub cpacr_el1: u64,
    pub ttbr0_el1: u64,
    pub ttbr1_el1: u64,
    pub tcr_el1: u64,
    pub mair_el1: u64,
    pub vbar_el1: u64,
    pub contextidr_el1: u64,
    _pad: [u64; 1],
}

const _: () = assert!(core::mem::size_of::<VcpuContext>() == 352);
const _: () = assert!(core::mem::align_of::<VcpuContext>() == 16);

impl VcpuContext {
    pub const fn new(entry_pc: u64, sp_el1: u64) -> Self {
        let mut ctx = Self {
            x: [0; 31],
            sp_el0: 0,
            sp_el1,
            elr_el2: entry_pc,
            // SPSR_EL2: 0x3c5 = EL1h (mode 0b0101) with D, A, I, F masked (bits 9..6 = 0b1111)
            spsr_el2: 0x3c5,
            sctlr_el1: 0x00c5_0838, // Typical architectural default
            cpacr_el1: 0,
            ttbr0_el1: 0,
            ttbr1_el1: 0,
            tcr_el1: 0,
            mair_el1: 0,
            vbar_el1: 0,
            contextidr_el1: 0,
            _pad: [0; 1],
        };
        ctx.x[0] = 0; // x0 argument (e.g. DTB IPA when booting real guest)
        ctx
    }

    /// Emulates a successfully decoded stage-2 MMIO abort. The guest PC is
    /// advanced only after the device has completed the access and any read
    /// result is safely present in the target guest register.
    pub fn emulate_stage2_mmio(
        &mut self,
        exit: &VcpuExit,
        dispatcher: &mut MmioDispatcher,
    ) -> Result<Option<u8>, VcpuMmioError> {
        let access = MmioAccess::decode_data_abort(exit.esr_el2, exit.fault_ipa())
            .map_err(VcpuMmioError::Decode)?;
        let transmit = dispatcher
            .emulate(access, &mut self.x)
            .map_err(VcpuMmioError::Emulation)?;
        self.elr_el2 = self
            .elr_el2
            .checked_add(4)
            .ok_or(VcpuMmioError::ProgramCounterOverflow)?;
        Ok(transmit)
    }
}

#[repr(C, align(16))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VcpuExit {
    pub vector: u64,
    pub esr_el2: u64,
    pub far_el2: u64,
    pub hpfar_el2: u64,
}

const _: () = assert!(core::mem::size_of::<VcpuExit>() == 32);
const _: () = assert!(core::mem::align_of::<VcpuExit>() == 16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcpuExitReason {
    Hvc { imm: u16, arg0: u64 },
    Stage2DataAbort { ipa: u64, is_write: bool, dfsc: u8 },
    Stage2InstructionAbort { ipa: u64, ifsc: u8 },
    SmcTrap,
    SysRegTrap,
    FpSimdTrap,
    Unknown { ec: u8, iss: u32 },
}

impl VcpuExit {
    pub fn decode_reason(&self, context: &VcpuContext) -> VcpuExitReason {
        let ec = ((self.esr_el2 >> 26) & 0x3f) as u8;
        let iss = (self.esr_el2 & 0x01ff_ffff) as u32;

        match ec {
            // HVC64 instruction execution in AArch64 state
            0x16 => {
                let imm = (iss & 0xffff) as u16;
                let arg0 = context.x[0];
                VcpuExitReason::Hvc { imm, arg0 }
            }
            // SMC64 instruction execution in AArch64 state
            0x17 => VcpuExitReason::SmcTrap,
            // Trapped MSR/MRS/System instruction
            0x18 => VcpuExitReason::SysRegTrap,
            // Access to FP/Advanced SIMD
            0x07 => VcpuExitReason::FpSimdTrap,
            // Instruction Abort from lower EL
            0x20 => {
                let ifsc = (iss & 0x3f) as u8;
                let ipa = self.fault_ipa();
                VcpuExitReason::Stage2InstructionAbort { ipa, ifsc }
            }
            // Data Abort from lower EL
            0x24 => {
                let dfsc = (iss & 0x3f) as u8;
                let is_write = (iss & (1 << 6)) != 0;
                let ipa = self.fault_ipa();
                VcpuExitReason::Stage2DataAbort {
                    ipa,
                    is_write,
                    dfsc,
                }
            }
            _ => VcpuExitReason::Unknown { ec, iss },
        }
    }

    pub fn fault_ipa(&self) -> u64 {
        // HPFAR_EL2[47:4] holds IPA[47:12].
        let fipa_page = ((self.hpfar_el2 >> 4) & 0x0000_0fff_ffff_ffff) << 12;
        let page_offset = self.far_el2 & 0xfff;
        fipa_page | page_offset
    }
}

/// Persistent architectural state of one virtual CPU.
///
/// A vCPU is owned by its VM.  The `repr(C)` layout lets the world-switch
/// assembly use the context at offset zero and the exit record after it.
#[repr(C, align(16))]
#[derive(Debug, Clone)]
pub struct Vcpu {
    context: VcpuContext,
    exit: VcpuExit,
}

const _: () = assert!(core::mem::offset_of!(Vcpu, context) == 0);
const _: () = assert!(core::mem::offset_of!(Vcpu, exit) == 352);

impl Vcpu {
    pub const fn new(entry_pc: u64, sp_el1: u64) -> Self {
        Self {
            context: VcpuContext::new(entry_pc, sp_el1),
            exit: VcpuExit {
                vector: 0,
                esr_el2: 0,
                far_el2: 0,
                hpfar_el2: 0,
            },
        }
    }

    /// Constructs the initial EL1 state required by the standard arm64 Linux
    /// boot ABI. Linux establishes its own stack and translation regime after
    /// entry, so no Phase-9 test stack or HVC protocol is carried into it.
    #[allow(dead_code)] // Used by the Phase 10 Linux image-loader path.
    pub const fn new_linux(entry_pc: u64, dtb_ipa: u64) -> Self {
        let mut vcpu = Self::new(entry_pc, 0);
        vcpu.context.x[0] = dtb_ipa;
        vcpu.context.x[1] = 0;
        vcpu.context.x[2] = 0;
        vcpu.context.x[3] = 0;
        vcpu
    }

    pub fn context(&self) -> &VcpuContext {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut VcpuContext {
        &mut self.context
    }

    pub fn exit(&self) -> &VcpuExit {
        &self.exit
    }
}

/// EL2 world-switch state owned by one physical CPU.
///
/// `active_vcpu` is non-null only while this pCPU is executing a guest.  The
/// vCPU itself remains owned by its `VmCtl`; this is merely the pCPU's active
/// association.  It is represented as a raw pointer because exception-entry
/// assembly must access it without Rust references.
#[repr(C, align(16))]
pub struct PcpuState {
    host_sp: u64,
    active_vcpu: *mut Vcpu,
}

const _: () = assert!(core::mem::size_of::<PcpuState>() == 16);
const _: () = assert!(core::mem::offset_of!(PcpuState, host_sp) == 0);
const _: () = assert!(core::mem::offset_of!(PcpuState, active_vcpu) == 8);

impl PcpuState {
    pub const fn new() -> Self {
        Self {
            host_sp: 0,
            active_vcpu: core::ptr::null_mut(),
        }
    }
}

/// The sole pCPU state until SMP support is introduced.
///
/// Assembly accesses this symbol directly.  Only the world-switch path
/// mutates it, while interrupts are disabled for guest execution.
#[unsafe(no_mangle)]
static mut __pcpu_state: PcpuState = PcpuState::new();

global_asm!(
    r#"
    .section .text.vcpu, "ax"
    .global __vcpu_run
    .type __vcpu_run, %function
__vcpu_run:
    // x0 = *mut Vcpu
    // Save host callee-saved registers on host EL2 stack
    stp  x19, x20, [sp, #-16]!
    stp  x21, x22, [sp, #-16]!
    stp  x23, x24, [sp, #-16]!
    stp  x25, x26, [sp, #-16]!
    stp  x27, x28, [sp, #-16]!
    stp  x29, x30, [sp, #-16]!

    // Save host stack pointer
    adrp x9, __pcpu_state
    add  x9, x9, :lo12:__pcpu_state
    mov  x10, sp
    str  x10, [x9]

    // Associate this pCPU with the VM-owned vCPU.
    adrp x9, __pcpu_state
    add  x9, x9, :lo12:__pcpu_state
    str  x0, [x9, #8]

    // Load guest EL1 system registers
    ldr  x9, [x0, #280]
    msr  sctlr_el1, x9
    ldr  x9, [x0, #288]
    msr  cpacr_el1, x9
    ldr  x9, [x0, #296]
    msr  ttbr0_el1, x9
    ldr  x9, [x0, #304]
    msr  ttbr1_el1, x9
    ldr  x9, [x0, #312]
    msr  tcr_el1, x9
    ldr  x9, [x0, #320]
    msr  mair_el1, x9
    ldr  x9, [x0, #328]
    msr  vbar_el1, x9
    ldr  x9, [x0, #336]
    msr  contextidr_el1, x9

    // Load SP_EL0 and SP_EL1
    ldr  x9, [x0, #248]
    msr  sp_el0, x9
    ldr  x9, [x0, #256]
    msr  sp_el1, x9

    // Load ELR_EL2 and SPSR_EL2
    ldr  x9, [x0, #264]
    msr  elr_el2, x9
    ldr  x9, [x0, #272]
    msr  spsr_el2, x9

    // Trap guest FP/Advanced-SIMD use only for the guest execution window.
    // CPTR_EL2.TFP also affects EL2, so tvisor must not leave it set while
    // running Rust code that may contain compiler-generated SIMD instructions.
    mrs  x9, cptr_el2
    orr  x9, x9, #0x400
    msr  cptr_el2, x9
    isb

    // Restore guest GPRs x1..x30
    ldp  x2,  x3,  [x0, #16]
    ldp  x4,  x5,  [x0, #32]
    ldp  x6,  x7,  [x0, #48]
    ldp  x8,  x9,  [x0, #64]
    ldp  x10, x11, [x0, #80]
    ldp  x12, x13, [x0, #96]
    ldp  x14, x15, [x0, #112]
    ldp  x16, x17, [x0, #128]
    ldp  x18, x19, [x0, #144]
    ldp  x20, x21, [x0, #160]
    ldp  x22, x23, [x0, #176]
    ldp  x24, x25, [x0, #192]
    ldp  x26, x27, [x0, #208]
    ldp  x28, x29, [x0, #224]
    ldr  x30, [x0, #240]
    ldp  x0,  x1,  [x0, #0]

    isb
    eret
    .size __vcpu_run, . - __vcpu_run

    .global __vcpu_exit_handler
    .type __vcpu_exit_handler, %function
__vcpu_exit_handler:
    // Scratch save x0, x1 on stack
    sub  sp, sp, #32
    stp  x0, x1, [sp, #0]

    // Re-enable FP/Advanced SIMD for the EL2 host before any Rust code can
    // execute. Guest x0/x1 are already safe on the stack, so x0 is scratch.
    mrs  x0, cptr_el2
    bic  x0, x0, #0x400
    msr  cptr_el2, x0
    isb

    // Load the active VM-owned vCPU. Its VcpuContext is at offset zero.
    adrp x0, __pcpu_state
    add  x0, x0, :lo12:__pcpu_state
    ldr  x0, [x0, #8]
    cbz  x0, .Lfatal_no_context

    // Save guest x2..x30 into context
    stp  x2,  x3,  [x0, #16]
    stp  x4,  x5,  [x0, #32]
    stp  x6,  x7,  [x0, #48]
    stp  x8,  x9,  [x0, #64]
    ldp  x1,  x2,  [sp, #0]     // Retrieve guest x0, x1 from temporary stack
    stp  x1,  x2,  [x0, #0]      // Save guest x0, x1 into context
    add  sp,  sp,  #32           // Restore temporary stack

    stp  x10, x11, [x0, #80]
    stp  x12, x13, [x0, #96]
    stp  x14, x15, [x0, #112]
    stp  x16, x17, [x0, #128]
    stp  x18, x19, [x0, #144]
    stp  x20, x21, [x0, #160]
    stp  x22, x23, [x0, #176]
    stp  x24, x25, [x0, #192]
    stp  x26, x27, [x0, #208]
    stp  x28, x29, [x0, #224]
    str  x30, [x0, #240]

    // Save guest stack pointers & exception return state
    mrs  x1, sp_el0
    str  x1, [x0, #248]
    mrs  x1, sp_el1
    str  x1, [x0, #256]
    mrs  x1, elr_el2
    str  x1, [x0, #264]
    mrs  x1, spsr_el2
    str  x1, [x0, #272]

    // Save guest EL1 system registers
    mrs  x1, sctlr_el1
    str  x1, [x0, #280]
    mrs  x1, cpacr_el1
    str  x1, [x0, #288]
    mrs  x1, ttbr0_el1
    str  x1, [x0, #296]
    mrs  x1, ttbr1_el1
    str  x1, [x0, #304]
    mrs  x1, tcr_el1
    str  x1, [x0, #312]
    mrs  x1, mair_el1
    str  x1, [x0, #320]
    mrs  x1, vbar_el1
    str  x1, [x0, #328]
    mrs  x1, contextidr_el1
    str  x1, [x0, #336]

    // Populate the active VcpuExit, which follows VcpuContext.
    add  x1, x0, #352

    mov  x2, #8                  // Vector 8: Lower EL AArch64 Sync
    str  x2, [x1, #0]
    mrs  x2, esr_el2
    str  x2, [x1, #8]
    mrs  x2, far_el2
    str  x2, [x1, #16]
    mrs  x2, hpfar_el2
    str  x2, [x1, #24]

    // Restore host stack pointer
    adrp x9, __pcpu_state
    add  x9, x9, :lo12:__pcpu_state
    ldr  x10, [x9]
    str  xzr, [x9, #8]          // no vCPU is active after this exit
    mov  sp, x10

    // Restore host callee-saved registers
    ldp  x29, x30, [sp], #16
    ldp  x27, x28, [sp], #16
    ldp  x25, x26, [sp], #16
    ldp  x23, x24, [sp], #16
    ldp  x21, x22, [sp], #16
    ldp  x19, x20, [sp], #16

    mov  x0, #8                  // Return exit vector 8
    ret

.Lfatal_no_context:
    wfe
    b .Lfatal_no_context
    .size __vcpu_exit_handler, . - __vcpu_exit_handler
"#
);

unsafe extern "C" {
    pub fn __vcpu_run(vcpu: *mut Vcpu) -> u64;
}
