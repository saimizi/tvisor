#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use dtoolkit::standard::NodeStandard;
use tvisor_util::aarch64_reg::CurrentEL;
use tvisor_util::boot_mode::fault_test_from_args;
use tvisor_util::debug_util::{debug_fini, debug_init};
use tvisor_util::diag::{DiagState, should_collect_full_diagnostics};
use tvisor_util::fdt::{
    discover_console, fdt_address_from_uboot_args, fdt_init, uboot_boot_allocations_from_args,
    uboot_lmb_reservations_from_args,
};
use tvisor_util::platform::discover_system_info_builder;
use tvisor_util::println;
use tvisor_util::system_info::{
    ConsoleKind, PhysAddr, PhysRegion, ReservationAttributes, ReservationOrigin, ReservationOwner,
    ReservedRegion,
};

mod boot;
mod exception;
mod guest;
mod mm;
mod vcpu;

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

    // Takeover is unconditional. Branch without creating a return address;
    // rust_main either enters tvisor's private EL2 environment or halts.
    b    rust_main
    .size main, . - main
"#,
);

fn halt() -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

fn stop() -> ! {
    debug_fini();
    halt()
}

#[unsafe(no_mangle)]
extern "C" fn rust_main(argc: isize, argv: *const *const u8) -> ! {
    // Before debug_init, startup must not access UART MMIO.
    let dtb_base = match unsafe { fdt_address_from_uboot_args(argc, argv) } {
        Ok(address) => address,
        Err(_) => halt(),
    };

    // SAFETY: The U-Boot handoff contract requires fdt= to identify a complete,
    // readable DTB that remains unchanged while tvisor uses it.
    let fdt = match unsafe { fdt_init(dtb_base) } {
        Ok(fdt) => fdt,
        Err(_) => halt(),
    };

    let console = match discover_console(*fdt) {
        Ok(console) => console,
        Err(_) => halt(),
    };

    let console_register_base = match usize::try_from(console.registers.start().value()) {
        Ok(address) => address,
        Err(_) => halt(),
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

    let fault_test = match unsafe { fault_test_from_args(argc, argv) } {
        Ok(fault_test) => fault_test,
        Err(error) => {
            println!("Invalid fault test: {}", error);
            debug_fini();
            halt();
        }
    };

    // Validate the execution level before reading any trap-sensitive
    // registers. In particular, ID-group register reads performed at EL1
    // can be redirected to EL2 by HCR_EL2.TID3.
    let current_el = CurrentEL::dump();
    if !should_collect_full_diagnostics(current_el.current_el()) {
        println!("CurrentEL: {:#018x}", current_el.value);
        stop();
    }

    let diag_state = DiagState::dump();

    if diag_state.sctlr_el2.as_ref().is_some_and(|s| s.bit_ee()) {
        println!("Handoff validation failed: SCTLR_EL2.EE selects big-endian data accesses");
        stop();
    }

    if diag_state
        .vbar_el2
        .as_ref()
        .is_some_and(|v| !v.is_aligned())
    {
        println!("Handoff validation failed: VBAR_EL2 is not 2 KiB aligned");
        stop();
    }

    if diag_state
        .id_aa64pfr0_el1
        .as_ref()
        .is_some_and(|r| r.el2() == 0)
    {
        println!("Handoff validation failed: EL2 is not implemented");
        stop();
    }

    let Some(mpidr_el1) = diag_state.mpidr_el1 else {
        println!("Platform discovery failed: MPIDR_EL1 is unavailable");
        stop();
    };
    let image_start = core::ptr::addr_of!(__image_start) as u64;
    let image_end = core::ptr::addr_of!(__image_end) as u64;
    let tvisor_image =
        match PhysRegion::from_bounds(PhysAddr::new(image_start), PhysAddr::new(image_end)) {
            Ok(region) => region,
            Err(error) => {
                println!("Platform discovery failed: invalid tvisor image: {}", error);
                stop();
            }
        };
    let mut system_info_builder = match discover_system_info_builder(
        *fdt,
        PhysAddr::new(dtb_base as usize as u64),
        tvisor_image,
        console,
        mpidr_el1.value,
    ) {
        Ok(info) => info,
        Err(error) => {
            println!("Platform discovery failed: {}", error);
            stop();
        }
    };

    let uboot_lmb = match unsafe { uboot_lmb_reservations_from_args(argc, argv) } {
        Ok(reservations) => reservations,
        Err(error) => {
            println!("Memory-map discovery failed: {}", error);
            stop();
        }
    };
    let boot_allocations = match unsafe { uboot_boot_allocations_from_args(argc, argv) } {
        Ok(allocations) => allocations,
        Err(error) => {
            println!("Memory-map discovery failed: {}", error);
            stop();
        }
    };
    for region in uboot_lmb.iter().chain(boot_allocations.iter()) {
        if let Err(error) = system_info_builder.add_reserved(ReservedRegion {
            region: *region,
            origin: ReservationOrigin::Bootloader,
            owner: ReservationOwner::Bootloader,
            attributes: ReservationAttributes::default(),
        }) {
            println!("Memory-map discovery failed: {}", error);
            stop();
        }
    }

    let system_info = match system_info_builder.finalize() {
        Ok(info) => info,
        Err(error) => {
            println!("Memory-map normalization failed: {}", error);
            stop();
        }
    };

    println!("{}", system_info);
    println!("{}", diag_state);

    let live_dtb = match PhysRegion::new(
        PhysAddr::new(dtb_base as usize as u64),
        fdt.data().len() as u64,
    ) {
        Ok(region) => region,
        Err(error) => {
            println!("Phase 8 allocator preparation failed: invalid DTB region: {error}");
            stop();
        }
    };

    // Check whether 4 KiB translation granules are supported.
    let Some(mmfr0) = diag_state.id_aa64mmfr0_el1 else {
        println!("Phase 7 table preparation failed: PARange is unavailable");
        stop();
    };
    if mmfr0.tgran4() != 0 {
        println!("Phase 7 requires 4 KiB translation-granule support");
        stop();
    }

    let Some(hcr) = diag_state.hcr_el2 else {
        println!("Phase 7 cannot validate HCR_EL2");
        stop();
    };
    // The initial translation regime requires stage 2 and VHE disabled.
    if hcr.bit_vm() || hcr.bit_e2h() {
        println!(
            "Phase 7 requires HCR_EL2.VM=0 and E2H=0 (VM={} E2H={})",
            hcr.bit_vm(),
            hcr.bit_e2h()
        );
        stop();
    }

    let prepared = match mm::prepare(
        system_info.memory(),
        console.registers.start().value(),
        mmfr0.parange(),
        live_dtb,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            println!("Phase 7 table preparation failed: {}", error);
            stop();
        }
    };

    drop(system_info);

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
    println!(
        "Phase 8 allocator prepared: RAM={} reserved={} in-use={} unused={} DTB={}",
        prepared.allocator_stats.ram_pages,
        prepared.allocator_stats.reserved_pages,
        prepared.allocator_stats.in_use_pages,
        prepared.allocator_stats.unused_pages,
        prepared.live_dtb_pages,
    );
    println!("Entering private EL2 no-return path...");
    // SAFETY: Handoff validation has completed and tvisor never returns to
    // U-Boot after replacing the inherited stack and translation regime.
    unsafe { boot::enter_private_el2(fault_test, prepared.registers) }
}

#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {}", info);
    halt()
}
