//! Phase 6 EL2 exception entry and reporting.
//!
//! AArch64 defines an exception vector table as sixteen fixed 128-byte slots.
//! The CPU selects one slot from the exception type (synchronous, IRQ, FIQ, or
//! SError), the source exception level, and whether the current EL uses SP0 or
//! SPx, then branches to `VBAR_EL2 + slot_offset`.
//!
//! Each slot here performs only the work that must be unique at entry: it
//! allocates an [`ExceptionFrame`], preserves x16/x17 before using x16 as a
//! temporary, records the slot number, and branches to one shared assembly
//! handler.  The shared handler saves the remaining general-purpose and EL2
//! exception registers and passes the completed frame to the Rust reporter.
//! Phase 6 resumes only from its deliberate `BRK #0x600` test.  The reporter
//! advances `ELR_EL2` past that instruction, after which assembly restores the
//! saved state and executes `eret`; every unexpected exception remains fatal.

use core::arch::{asm, global_asm};

use tvisor_util::println;

#[repr(C, align(16))]
/// Complete processor state captured by the Phase 6 EL2 exception entry.
///
/// The field order and offsets are an ABI shared with the assembly handler
/// below, so changes must be reflected in its load/store offsets.
pub struct ExceptionFrame {
    /// General-purpose registers x0 through x30 as they were at exception
    /// entry.  x29 is the frame pointer and x30 is the link register.
    pub x: [u64; 31],
    /// Vector-slot number selected by the CPU, in the range 0 through 15.
    /// It identifies the exception type, source EL, and SP0/SPx group.
    pub vector: u64,
    /// Exception Link Register: the address associated with the interrupted
    /// or faulting instruction, used as the return address by `eret`.
    pub elr_el2: u64,
    /// Saved Program Status Register: the PSTATE and execution mode captured
    /// when the exception was taken to EL2.
    pub spsr_el2: u64,
    /// Exception Syndrome Register: the exception class and, when defined,
    /// additional syndrome information describing the exception cause.
    pub esr_el2: u64,
    /// Fault Address Register: the faulting virtual address for exception
    /// classes that define it; its value is not meaningful for every class.
    pub far_el2: u64,
}

const _: () = assert!(core::mem::size_of::<ExceptionFrame>() == 288);
const _: () = assert!(core::mem::align_of::<ExceptionFrame>() == 16);

global_asm!(
    r#"
    .macro VECTOR_SLOT id
        sub sp, sp, #288
        stp x16, x17, [sp, #128]
        mov x16, #\id
        str x16, [sp, #248]
        b __el2_exception_common
        .balign 128
    .endm

    .section .vectors, "ax"
    .balign 2048
    .global __el2_vectors
__el2_vectors:
    // Current EL using SP0: synchronous exception.
    VECTOR_SLOT 0
    // Current EL using SP0: IRQ.
    VECTOR_SLOT 1
    // Current EL using SP0: FIQ.
    VECTOR_SLOT 2
    // Current EL using SP0: SError.
    VECTOR_SLOT 3
    // Current EL using SPx (SP_EL2 at EL2): synchronous exception.
    VECTOR_SLOT 4
    // Current EL using SPx (SP_EL2 at EL2): IRQ.
    VECTOR_SLOT 5
    // Current EL using SPx (SP_EL2 at EL2): FIQ.
    VECTOR_SLOT 6
    // Current EL using SPx (SP_EL2 at EL2): SError.
    VECTOR_SLOT 7
    // Lower EL executing AArch64: synchronous exception.
    b __vcpu_exit_handler
    .balign 128
    // Lower EL executing AArch64: IRQ.
    VECTOR_SLOT 9
    // Lower EL executing AArch64: FIQ.
    VECTOR_SLOT 10
    // Lower EL executing AArch64: SError.
    VECTOR_SLOT 11
    // Lower EL executing AArch32: synchronous exception.
    VECTOR_SLOT 12
    // Lower EL executing AArch32: IRQ.
    VECTOR_SLOT 13
    // Lower EL executing AArch32: FIQ.
    VECTOR_SLOT 14
    // Lower EL executing AArch32: SError.
    VECTOR_SLOT 15

    .section .text.exception, "ax"
__el2_exception_common:
    stp x0,  x1,  [sp, #0]
    stp x2,  x3,  [sp, #16]
    stp x4,  x5,  [sp, #32]
    stp x6,  x7,  [sp, #48]
    stp x8,  x9,  [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]
    mrs x9, elr_el2
    str x9, [sp, #256]
    mrs x9, spsr_el2
    str x9, [sp, #264]
    mrs x9, esr_el2
    str x9, [sp, #272]
    mrs x9, far_el2
    str x9, [sp, #280]
    mov x0, sp
    bl phase6_exception_handler
    cbz x0, 1f

    // Restore the possibly updated exception-return state before restoring
    // the temporary register used for these loads.
    ldr x16, [sp, #256]
    msr elr_el2, x16
    ldr x16, [sp, #264]
    msr spsr_el2, x16

    ldp x0,  x1,  [sp, #0]
    ldp x2,  x3,  [sp, #16]
    ldp x4,  x5,  [sp, #32]
    ldp x6,  x7,  [sp, #48]
    ldp x8,  x9,  [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldp x28, x29, [sp, #224]
    ldr x30, [sp, #240]
    ldp x16, x17, [sp, #128]
    add sp, sp, #288
    eret
1:
    wfe
    b 1b
"#,
);

const CURRENT_EL_SPX_SYNC_VECTOR: u64 = 4;
const ESR_EC_SHIFT: u32 = 26;
const ESR_EC_MASK: u64 = 0x3f;
const ESR_EC_BRK_AARCH64: u64 = 0x3c;
const ESR_BRK_COMMENT_MASK: u64 = 0xffff;
const PHASE6_BRK_COMMENT: u64 = 0x600;

#[unsafe(no_mangle)]
extern "C" fn phase6_exception_handler(frame: &mut ExceptionFrame) -> u64 {
    println!("Phase 6 EL2 exception");
    println!(" vector={}", frame.vector);
    println!(" ESR_EL2={:#018x}", frame.esr_el2);
    println!(" ELR_EL2={:#018x}", frame.elr_el2);
    println!("SPSR_EL2={:#018x}", frame.spsr_el2);
    println!(" FAR_EL2={:#018x}", frame.far_el2);

    let exception_class = (frame.esr_el2 >> ESR_EC_SHIFT) & ESR_EC_MASK;
    let brk_comment = frame.esr_el2 & ESR_BRK_COMMENT_MASK;
    if frame.vector == CURRENT_EL_SPX_SYNC_VECTOR
        && exception_class == ESR_EC_BRK_AARCH64
        && brk_comment == PHASE6_BRK_COMMENT
    {
        frame.elr_el2 = frame.elr_el2.wrapping_add(4);
        println!("Returning after deliberate synchronous exception");
        return 1;
    }

    println!("Unexpected exception; halting");
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
