//! vCPU state, pCPU-local world-switch state, and exit handling.

use core::arch::global_asm;
use tvisor_util::gicv2::VirtualGicV2State;
use tvisor_util::mmio::{MmioAccess, MmioDecodeError, MmioDispatcher, MmioEmulationError};
use tvisor_util::virtual_timer::VirtualTimerState;

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
    /// Linux current-task pointer, preserved across EL2 exits.
    pub tpidr_el1: u64,
    /// EL0 read-only thread pointer, preserved for later userspace support.
    pub tpidrro_el0: u64,
    /// Guest FP/Advanced-SIMD registers q0 through q31.
    pub q: [u128; 32],
    /// Guest FP control and status registers.
    pub fpcr: u64,
    pub fpsr: u64,
}

const _: () = assert!(core::mem::size_of::<VcpuContext>() == 896);
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
            tpidr_el1: 0,
            tpidrro_el0: 0,
            q: [0; 32],
            fpcr: 0,
            fpsr: 0,
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
        // In case of instruction trap, elr_el2 stores the address where the trap exactly happened.
        // advance it to avoid re-entering;
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
    timer: VirtualTimerState,
    gic: VirtualGicV2State,
}

const _: () = assert!(core::mem::offset_of!(Vcpu, context) == 0);
const _: () = assert!(core::mem::offset_of!(Vcpu, exit) == 896);
const _: () = assert!(core::mem::offset_of!(Vcpu, timer) == 928);
const _: () = assert!(core::mem::offset_of!(Vcpu, gic) == 960);
const _: () = assert!(core::mem::size_of::<Vcpu>() == 976);

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
            timer: VirtualTimerState {
                cntvoff_el2: 0,
                cntv_cval_el0: 0,
                cntv_ctl_el0: 0,
                pending_irq: 0,
            },
            gic: VirtualGicV2State {
                vmcr: 0,
                timer_lr: 0,
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

    pub fn timer_mut(&mut self) -> &mut VirtualTimerState {
        &mut self.timer
    }

    pub fn gic(&self) -> &VirtualGicV2State {
        &self.gic
    }

    pub fn gic_mut(&mut self) -> &mut VirtualGicV2State {
        &mut self.gic
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

    // Preserve the EL2 FP/Advanced-SIMD state before loading the vCPU's
    // independent vector context. 528 bytes holds q0-q31 plus FPCR/FPSR.
    sub  sp, sp, #528
    stp  q0,  q1,  [sp, #0]
    stp  q2,  q3,  [sp, #32]
    stp  q4,  q5,  [sp, #64]
    stp  q6,  q7,  [sp, #96]
    stp  q8,  q9,  [sp, #128]
    stp  q10, q11, [sp, #160]
    stp  q12, q13, [sp, #192]
    stp  q14, q15, [sp, #224]
    stp  q16, q17, [sp, #256]
    stp  q18, q19, [sp, #288]
    stp  q20, q21, [sp, #320]
    stp  q22, q23, [sp, #352]
    stp  q24, q25, [sp, #384]
    stp  q26, q27, [sp, #416]
    stp  q28, q29, [sp, #448]
    stp  q30, q31, [sp, #480]
    mrs  x9, fpcr
    str  x9, [sp, #512]
    mrs  x9, fpsr
    str  x9, [sp, #520]

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
    ldr  x9, [x0, #344]
    msr  tpidr_el1, x9
    ldr  x9, [x0, #352]
    msr  tpidrro_el0, x9

    // Restore guest FP/Advanced-SIMD context.
    ldp  q0,  q1,  [x0, #368]
    ldp  q2,  q3,  [x0, #400]
    ldp  q4,  q5,  [x0, #432]
    ldp  q6,  q7,  [x0, #464]
    ldp  q8,  q9,  [x0, #496]
    ldp  q10, q11, [x0, #528]
    ldp  q12, q13, [x0, #560]
    ldp  q14, q15, [x0, #592]
    ldp  q16, q17, [x0, #624]
    ldp  q18, q19, [x0, #656]
    ldp  q20, q21, [x0, #688]
    ldp  q22, q23, [x0, #720]
    ldp  q24, q25, [x0, #752]
    ldp  q26, q27, [x0, #784]
    ldp  q28, q29, [x0, #816]
    ldp  q30, q31, [x0, #848]
    ldr  x9, [x0, #880]
    msr  fpcr, x9
    ldr  x9, [x0, #888]
    msr  fpsr, x9

    // Restore per-vCPU architectural virtual-timer state.
    ldr  x9, [x0, #928]
    msr  cntvoff_el2, x9
    ldr  x9, [x0, #936]
    msr  cntv_cval_el0, x9
    ldr  x9, [x0, #944]
    msr  cntv_ctl_el0, x9

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
    mov  x0, #8
    str  x0, [sp, #16]
    b    __vcpu_exit_common

    .global __vcpu_irq_handler
    .type __vcpu_irq_handler, %function
__vcpu_irq_handler:
    // Preserve the same guest state as a synchronous vCPU exit.
    sub  sp, sp, #32
    stp  x0, x1, [sp, #0]
    mov  x0, #9
    str  x0, [sp, #16]

__vcpu_exit_common:

    // Load the active VM-owned vCPU. Its VcpuContext is at offset zero.
    adrp x0, __pcpu_state
    add  x0, x0, :lo12:__pcpu_state
    ldr  x0, [x0, #8]
    cbz  x0, .Lfatal_no_context

    // Save guest FP/Advanced-SIMD state before EL2 code can use it.
    stp  q0,  q1,  [x0, #368]
    stp  q2,  q3,  [x0, #400]
    stp  q4,  q5,  [x0, #432]
    stp  q6,  q7,  [x0, #464]
    stp  q8,  q9,  [x0, #496]
    stp  q10, q11, [x0, #528]
    stp  q12, q13, [x0, #560]
    stp  q14, q15, [x0, #592]
    stp  q16, q17, [x0, #624]
    stp  q18, q19, [x0, #656]
    stp  q20, q21, [x0, #688]
    stp  q22, q23, [x0, #720]
    stp  q24, q25, [x0, #752]
    stp  q26, q27, [x0, #784]
    stp  q28, q29, [x0, #816]
    stp  q30, q31, [x0, #848]
    mrs  x1, fpcr
    str  x1, [x0, #880]
    mrs  x1, fpsr
    str  x1, [x0, #888]

    // Save guest x2..x30 into context
    stp  x2,  x3,  [x0, #16]
    stp  x4,  x5,  [x0, #32]
    stp  x6,  x7,  [x0, #48]
    stp  x8,  x9,  [x0, #64]
    ldp  x1,  x2,  [sp, #0]     // Retrieve guest x0, x1 from temporary stack
    stp  x1,  x2,  [x0, #0]      // Save guest x0, x1 into context
    ldr  x3, [sp, #16]           // Vector selected by the EL2 vector table.
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
    mrs  x1, tpidr_el1
    str  x1, [x0, #344]
    mrs  x1, tpidrro_el0
    str  x1, [x0, #352]

    // Save architectural virtual-timer state before returning to EL2 Rust.
    mrs  x1, cntvoff_el2
    str  x1, [x0, #928]
    mrs  x1, cntv_cval_el0
    str  x1, [x0, #936]
    mrs  x1, cntv_ctl_el0
    str  x1, [x0, #944]

    // Populate the active VcpuExit, which follows VcpuContext.
    add  x1, x0, #896

    str  x3, [x1, #0]
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

    // Restore the EL2 FP/Advanced-SIMD state saved at guest entry.
    ldp  q0,  q1,  [sp, #0]
    ldp  q2,  q3,  [sp, #32]
    ldp  q4,  q5,  [sp, #64]
    ldp  q6,  q7,  [sp, #96]
    ldp  q8,  q9,  [sp, #128]
    ldp  q10, q11, [sp, #160]
    ldp  q12, q13, [sp, #192]
    ldp  q14, q15, [sp, #224]
    ldp  q16, q17, [sp, #256]
    ldp  q18, q19, [sp, #288]
    ldp  q20, q21, [sp, #320]
    ldp  q22, q23, [sp, #352]
    ldp  q24, q25, [sp, #384]
    ldp  q26, q27, [sp, #416]
    ldp  q28, q29, [sp, #448]
    ldp  q30, q31, [sp, #480]
    ldr  x9, [sp, #512]
    msr  fpcr, x9
    ldr  x9, [sp, #520]
    msr  fpsr, x9
    add  sp, sp, #528

    // Restore host callee-saved registers
    ldp  x29, x30, [sp], #16
    ldp  x27, x28, [sp], #16
    ldp  x25, x26, [sp], #16
    ldp  x23, x24, [sp], #16
    ldp  x21, x22, [sp], #16
    ldp  x19, x20, [sp], #16

    mov  x0, x3                  // Return Lower-EL Sync (8) or IRQ (9).
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
