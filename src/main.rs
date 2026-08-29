#![no_std]
#![no_main]

use core::arch::global_asm;
use dtoolkit::standard::NodeStandard;
use tvisor_util::aarch64_reg::CurrentEL;
use tvisor_util::boot_mode::takeover_request_from_args;
use tvisor_util::debug_util::{DebugMemError, debug_fini, debug_init, debug_mem_error};
use tvisor_util::diag::{DiagState, should_collect_full_diagnostics};
use tvisor_util::fdt::{
    discover_console, fdt_address_from_uboot_args, fdt_init, uboot_boot_allocations_from_args,
    uboot_lmb_reservations_from_args,
};
use tvisor_util::memory_map::MemoryMap;
use tvisor_util::platform::discover_system_info;
use tvisor_util::println;
use tvisor_util::system_info::{
    ConsoleKind, PhysAddr, PhysRegion, ReservationAttributes, ReservationOrigin, ReservationOwner,
    ReservedRegion,
};

mod boot;
mod exception;
mod mm;

unsafe extern "C" {
    static __image_start: u8;
    static __image_end: u8;
}

global_asm!(
    r#"
    .section .text.main, "ax"
    .global main
    .type main, %function
main:
    sub  sp, sp, #112
    stp  x18, x19, [sp, #0]
    stp  x20, x21, [sp, #16]
    stp  x22, x23, [sp, #32]
    stp  x24, x25, [sp, #48]
    stp  x26, x27, [sp, #64]
    stp  x28, x29, [sp, #80]
    str  x30, [sp, #96]

    // U-Boot's `go` command jumps into the bytes loaded by TFTP and does not
    // process the ELF NOBITS segment. Initialize Rust's zero-initialized
    // statics before entering any Rust code.
    adrp x9, __bss_start
    add  x9, x9, :lo12:__bss_start
    adrp x10, __bss_end
    add  x10, x10, :lo12:__bss_end
1:
    cmp  x9, x10
    b.hs 2f
    str  xzr, [x9], #8
    b    1b
2:

    bl   rust_main
    str  x0, [sp, #104]

    ldp  x18, x19, [sp, #0]
    ldp  x20, x21, [sp, #16]
    ldp  x22, x23, [sp, #32]
    ldp  x24, x25, [sp, #48]
    ldp  x26, x27, [sp, #64]
    ldp  x28, x29, [sp, #80]
    ldr  x30, [sp, #96]
    ldr  x0, [sp, #104]
    add  sp, sp, #112
    ret
    .size main, . - main
"#,
);

#[repr(isize)]
enum EarlyBootError {
    InvalidUbootArguments = 0x10,
    InvalidDtb = 0x11,
    ConsoleDiscovery = 0x12,
}

#[unsafe(no_mangle)]
extern "C" fn rust_main(argc: isize, argv: *const *const u8) -> isize {
    // Before debug_init, startup must not access UART MMIO or the error stack.
    // U-Boot reports these return values if early DTB discovery fails.
    let dtb_base = match unsafe { fdt_address_from_uboot_args(argc, argv) } {
        Ok(address) => address,
        Err(_) => return EarlyBootError::InvalidUbootArguments as isize,
    };

    // SAFETY: The U-Boot handoff contract requires fdt= to identify a complete,
    // readable DTB that remains unchanged while tvisor uses it.
    let fdt = match unsafe { fdt_init(dtb_base) } {
        Ok(fdt) => fdt,
        Err(_) => return EarlyBootError::InvalidDtb as isize,
    };

    let console = match discover_console(*fdt) {
        Ok(console) => console,
        Err(_) => return EarlyBootError::ConsoleDiscovery as isize,
    };

    let console_register_base = match usize::try_from(console.registers.start().value()) {
        Ok(address) => address,
        Err(_) => return EarlyBootError::ConsoleDiscovery as isize,
    };
    match console.kind {
        ConsoleKind::MiniUart => debug_init(console_register_base),
    }

    println!(
        "DTB base={:#x}, version={}, size={:#x}",
        dtb_base as usize,
        fdt.version(),
        fdt.data().len()
    );
    match fdt.root().model() {
        Ok(Some(model)) => println!("DTB model={}", model),
        Ok(None) => println!("DTB model=<missing>"),
        Err(error) => println!("DTB model is invalid: {}", error),
    }
    println!(
        "Console: {:?}, register_base={:#x}, register_size={:#x}",
        console.kind,
        console.registers.start().value(),
        console.registers.size()
    );

    let takeover = match unsafe { takeover_request_from_args(argc, argv) } {
        Ok(request) => request,
        Err(error) => {
            println!("Invalid boot mode: {}", error);
            debug_fini();
            return 1;
        }
    };

    let mut ret = 0_isize;
    let mut phase7_tables = None;
    'diagnostic: {
        // Validate the execution level before reading any trap-sensitive
        // registers. In particular, ID-group register reads performed at EL1
        // can be redirected to EL2 by HCR_EL2.TID3.
        let current_el = CurrentEL::dump();
        if !should_collect_full_diagnostics(current_el.current_el()) {
            debug_mem_error(DebugMemError::InvalidEL2State);
            println!("CurrentEL: {:#018x}", current_el.value);
            ret = 1;
            break 'diagnostic;
        }

        let diag_state = DiagState::dump();

        if diag_state.sctlr_el2.as_ref().is_some_and(|s| s.bit_ee()) {
            debug_mem_error(DebugMemError::UnexpectedEL2Endianness);
            ret = 1;
            break 'diagnostic;
        }

        if diag_state
            .vbar_el2
            .as_ref()
            .is_some_and(|v| !v.is_aligned())
        {
            debug_mem_error(DebugMemError::InvalidVectorBaseAlignment);
            ret = 1;
            break 'diagnostic;
        }

        if diag_state
            .id_aa64pfr0_el1
            .as_ref()
            .is_some_and(|r| r.el2() == 0)
        {
            debug_mem_error(DebugMemError::UnsupportedEL2Feature);
            ret = 1;
            break 'diagnostic;
        }

        let Some(mpidr_el1) = diag_state.mpidr_el1 else {
            debug_mem_error(DebugMemError::PlatformDiscovery);
            println!("Platform discovery failed: MPIDR_EL1 is unavailable");
            ret = 1;
            break 'diagnostic;
        };
        let image_start = core::ptr::addr_of!(__image_start) as u64;
        let image_end = core::ptr::addr_of!(__image_end) as u64;
        let tvisor_image =
            match PhysRegion::from_bounds(PhysAddr::new(image_start), PhysAddr::new(image_end)) {
                Ok(region) => region,
                Err(error) => {
                    debug_mem_error(DebugMemError::PlatformDiscovery);
                    println!("Platform discovery failed: invalid tvisor image: {}", error);
                    ret = 1;
                    break 'diagnostic;
                }
            };
        let mut system_info = match discover_system_info(
            *fdt,
            PhysAddr::new(dtb_base as usize as u64),
            tvisor_image,
            console,
            mpidr_el1.value,
        ) {
            Ok(info) => info,
            Err(error) => {
                debug_mem_error(DebugMemError::PlatformDiscovery);
                println!("Platform discovery failed: {}", error);
                ret = 1;
                break 'diagnostic;
            }
        };

        let uboot_lmb = match unsafe { uboot_lmb_reservations_from_args(argc, argv) } {
            Ok(reservations) => reservations,
            Err(error) => {
                debug_mem_error(DebugMemError::PlatformDiscovery);
                println!("Memory-map discovery failed: {}", error);
                ret = 1;
                break 'diagnostic;
            }
        };
        let boot_allocations = match unsafe { uboot_boot_allocations_from_args(argc, argv) } {
            Ok(allocations) => allocations,
            Err(error) => {
                debug_mem_error(DebugMemError::PlatformDiscovery);
                println!("Memory-map discovery failed: {}", error);
                ret = 1;
                break 'diagnostic;
            }
        };
        for region in uboot_lmb.iter().chain(boot_allocations.iter()) {
            if let Err(error) = system_info.add_reserved(ReservedRegion {
                region: *region,
                origin: ReservationOrigin::Bootloader,
                owner: ReservationOwner::Bootloader,
                attributes: ReservationAttributes::default(),
            }) {
                debug_mem_error(DebugMemError::PlatformDiscovery);
                println!("Memory-map discovery failed: {}", error);
                ret = 1;
                break 'diagnostic;
            }
        }

        let memory_map = match MemoryMap::from_system_info(&system_info) {
            Ok(map) => map,
            Err(error) => {
                debug_mem_error(DebugMemError::PlatformDiscovery);
                println!("Memory-map normalization failed: {}", error);
                ret = 1;
                break 'diagnostic;
            }
        };

        println!("{}", system_info);
        println!("{}", memory_map);
        println!("{}", diag_state);

        if takeover.is_some() {
            // Check whether support 4KiB page table
            let Some(mmfr0) = diag_state.id_aa64mmfr0_el1 else {
                println!("Phase 7 table preparation failed: PARange is unavailable");
                ret = 1;
                break 'diagnostic;
            };
            if mmfr0.tgran4() != 0 {
                println!("Phase 7 requires 4 KiB translation-granule support");
                ret = 1;
                break 'diagnostic;
            }
            if takeover.is_some_and(|request| request.switch_page_tables) {
                let Some(hcr) = diag_state.hcr_el2 else {
                    println!("Phase 7 cannot validate HCR_EL2");
                    ret = 1;
                    break 'diagnostic;
                };
                // Make sure
                // * stage-2 translation is disabled (HCR_EL2.VM=0).
                // * VHE is disabled (HCR_EL2.E2H = 0)
                if hcr.bit_vm() || hcr.bit_e2h() {
                    println!(
                        "Phase 7 requires HCR_EL2.VM=0 and E2H=0 (VM={} E2H={})",
                        hcr.bit_vm(),
                        hcr.bit_e2h()
                    );
                    ret = 1;
                    break 'diagnostic;
                }
            }
            match mm::prepare(
                &memory_map,
                console.registers.start().value(),
                mmfr0.parange(),
            ) {
                Ok(prepared) => {
                    println!(
                        "Phase 7 tables prepared: arena=[{:#018x}, {:#018x}) pages={}",
                        prepared.arena_start, prepared.arena_end, prepared.used_pages
                    );
                    println!(
                        "  MAIR_EL2={:#018x} TCR_EL2={:#018x}",
                        prepared.registers.mair_el2, prepared.registers.tcr_el2
                    );
                    println!(
                        " TTBR0_EL2={:#018x} SCTLR_EL2={:#018x}",
                        prepared.registers.ttbr0_el2, prepared.registers.sctlr_el2
                    );
                    phase7_tables = Some(prepared);
                }
                Err(error) => {
                    println!("Phase 7 table preparation failed: {}", error);
                    ret = 1;
                    break 'diagnostic;
                }
            }
        }
    }

    if ret == 0
        && let Some(request) = takeover
    {
        let prepared = phase7_tables.expect("takeover tables were prepared");
        println!("Entering private EL2 no-return path...");
        // SAFETY: The explicit takeover argument authorizes abandoning the
        // U-Boot stack after all diagnostic validation has succeeded.
        unsafe {
            boot::enter_private_el2(
                request.fault_test,
                request.switch_page_tables,
                prepared.registers,
            )
        }
    }

    debug_fini();
    ret
}

#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {}", info);
    loop {}
}
