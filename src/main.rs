#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use dtoolkit::fdt::Fdt;
use tvisor_util::PAGE_SIZE;
use tvisor_util::aarch64_reg::*;
use tvisor_util::debug_util::{debug_init, stop};
use tvisor_util::fdt::{discover_console, discover_gic_v2, fdt_address_from_uboot_args, fdt_init};
use tvisor_util::gicv2;
use tvisor_util::system_info::{ConsoleInfo, ConsoleKind, PhysRegion};
use tvisor_util::{halt, println};

mod boot;
mod exception;
mod guest;
mod heap;
mod mm;
mod vcpu;
mod vmctl;

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

fn console_init(
    argc: isize,
    argv: *const *const u8,
) -> Option<(&'static Fdt<'static>, ConsoleInfo)> {
    // Before debug_init, startup must not access UART MMIO.
    let dtb_base = match unsafe { fdt_address_from_uboot_args(argc, argv) } {
        Ok(address) => address,
        Err(_) => return None,
    };

    // SAFETY: The U-Boot handoff contract requires fdt= to identify a complete,
    // readable DTB that remains unchanged while tvisor uses it.
    let fdt = match unsafe { fdt_init(dtb_base) } {
        Ok(fdt) => fdt,
        Err(_) => return None,
    };

    let console = match discover_console(*fdt) {
        Ok(console) => console,
        Err(_) => return None,
    };

    let console_register_base = match usize::try_from(console.registers.start().value()) {
        Ok(address) => address,
        Err(_) => return None,
    };
    match console.kind {
        ConsoleKind::MiniUart => debug_init(console_register_base),
    }

    Some((fdt, console))
}

fn status_check() -> Result<(), ()> {
    // Validate the execution level before reading any trap-sensitive
    // registers. In particular, ID-group register reads performed at EL1
    // can be redirected to EL2 by HCR_EL2.TID3.
    let current_el = CurrentEL::dump();
    if current_el.current_el() != ExceptionLevel::EL2 {
        println!("CurrentEL: {:#018x}", current_el.value);
        return Err(());
    }

    if SctlrEl2::dump().is_some_and(|s| s.bit_ee()) {
        println!("Handoff validation failed: SCTLR_EL2.EE selects big-endian data accesses");
        return Err(());
    }

    if VbarEl2::dump().is_some_and(|v| !v.is_aligned()) {
        println!("Handoff validation failed: VBAR_EL2 is not 2 KiB aligned");
        return Err(());
    }

    if IdAa64Pfr0El1::dump().is_some_and(|v| v.el2() == 0) {
        println!("Handoff validation failed: EL2 is not implemented");
        return Err(());
    }

    if MpidrEl1::dump().is_none() {
        println!("Platform discovery failed: MPIDR_EL1 is unavailable");
        return Err(());
    };

    Ok(())
}

#[unsafe(no_mangle)]
extern "C" fn rust_main(argc: isize, argv: *const *const u8) -> ! {
    let Some((fdt, console)) = console_init(argc, argv) else {
        halt();
    };

    if status_check().is_err() {
        halt();
    }

    let live_dtb: PhysRegion = (*fdt).into();
    let live_dtb_pages =
        match PhysRegion::new_aligned(live_dtb.start(), live_dtb.size(), PAGE_SIZE as u64) {
            Ok(region) => region,
            Err(_) => halt(),
        };
    let uart_region = match PhysRegion::new_aligned(
        console.registers.start(),
        console.registers.size(),
        PAGE_SIZE as u64,
    ) {
        Ok(region) => region,
        Err(_) => halt(),
    };
    let gic = match discover_gic_v2(*fdt) {
        Ok(gic) => gic,
        Err(error) => {
            println!("GICv2 discovery failed: {}", error);
            halt();
        }
    };
    println!(
        "GICv2 DTB discovery: GICD={} GICC={} GICH={} GICV={}",
        gic.distributor, gic.cpu_interface, gic.hypervisor_interface, gic.virtual_cpu_interface,
    );
    gicv2::initialize(gic);

    // Set up bootstrap page table
    let bootstrap = match mm::setup_bootstrap_page_table(live_dtb_pages, uart_region, gic) {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            println!("Failed to setup bootstrap page tables: {}", error);
            stop();
        }
    };

    // SAFETY: Handoff validation has completed and tvisor never returns to
    // U-Boot after replacing the inherited stack and translation regime.
    unsafe { boot::enter_private_el2(bootstrap) }
}

#[panic_handler]
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {}", info);
    halt()
}
