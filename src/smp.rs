//! Phase 11.0 DT startup discovery only. No firmware matching or CPU release.
use core::arch::asm;
use dtoolkit::fdt::Fdt;
use spin::Once;
use tvisor_util::{
    aarch64_reg::MpidrEl1,
    cpu_startup::{StartupContractError, StartupMemoryContract, discover_startup_memory},
    cpu_topology::{CpuDiscoveryError, CpuTopology, discover_cpu_topology},
    println,
    system_info::PhysAddr,
};

static TOPOLOGY: Once<CpuTopology> = Once::new();
pub fn topology() -> Option<&'static CpuTopology> {
    TOPOLOGY.get()
}

static STARTUP_CONTRACT: Once<StartupMemoryContract> = Once::new();

#[derive(Debug)]
pub enum DiscoveryError {
    MissingMpidr,
    Pcpu(&'static str),
    Topology(CpuDiscoveryError),
    Contract(StartupContractError),
}

pub fn startup_contract() -> Option<&'static StartupMemoryContract> {
    STARTUP_CONTRACT.get()
}

pub fn discover(fdt: Fdt<'_>) -> Result<(), DiscoveryError> {
    let mpidr = MpidrEl1::dump().ok_or(DiscoveryError::MissingMpidr)?.value;
    let topology = discover_cpu_topology(fdt, mpidr).map_err(DiscoveryError::Topology)?;
    println!("Physical CPU discovery: boot MPIDR={:#x}", mpidr);
    for cpu in topology.cpus() {
        println!("    {:?}", cpu);
    }
    println!("    PSCI DT description: {:?}", topology.psci);
    if topology.psci.is_some() {
        println!(
            "    PSCI EL2 eligibility/version/CPU_ON/entry contract: unverified; no firmware calls"
        );
    } else {
        println!("    PSCI: not advertised; conduit/version/CPU_ON queries skipped");
    }

    let contract = discover_startup_memory(fdt, &topology).map_err(DiscoveryError::Contract)?;
    // Validate all descriptions and reservation ownership before touching any
    // cell. Read only DT-advertised cells in reserved RAM, never firmware code.
    contract
        .verify_release_cells(read_release_cell)
        .map_err(DiscoveryError::Contract)?;
    for cell in contract.release_cells() {
        println!("    Spin-table release cell={} initial value=0", cell);
    }
    println!("    DT startup interface validated; firmware instructions are not inspected");
    println!("    Secondary EL2/MMU/cache/coherency state must be validated at 11.3 entry");
    println!("    Secondary activation disabled (Phase 11.0 discovery only)");
    crate::pcpu::prepare_boot_cpu(&topology).map_err(DiscoveryError::Pcpu)?;
    TOPOLOGY.call_once(|| topology);
    STARTUP_CONTRACT.call_once(|| contract);
    Ok(())
}

fn read_release_cell(address: PhysAddr) -> Result<u64, StartupContractError> {
    // Each validated cell is aligned to 8 bytes and cannot cross the smallest
    // AArch64 translation granule. Check the inherited read mapping first.
    let par: u64;
    unsafe {
        asm!(
            "mrs {saved}, PAR_EL1", "at s1e2r, {address}", "isb",
            "mrs {result}, PAR_EL1", "msr PAR_EL1, {saved}",
            saved = out(reg) _, result = out(reg) par,
            address = in(reg) address.value(),
            options(nostack, preserves_flags),
        );
    }
    const PA_PAGE_MASK: u64 = 0x000f_ffff_ffff_f000;
    if par & 1 != 0 || par & PA_PAGE_MASK != address.value() & PA_PAGE_MASK {
        return Err(StartupContractError::ReleaseCellNotIdentityMapped);
    }
    let value: u64;
    // SAFETY: discovery validated the complete cell in reserved physical RAM;
    // AT confirms an identity read mapping. The boot handoff check requires
    // little-endian accesses. Assembly also handles a cell at physical zero
    // without constructing/dereferencing a null Rust pointer.
    unsafe {
        asm!("ldr {value}, [{address}]", value = out(reg) value,
             address = in(reg) address.value(), options(nostack, readonly, preserves_flags));
    }
    Ok(value)
}

impl core::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Pcpu(error) => write!(f, "per-CPU preparation failed: {error}"),
            Self::Topology(error) => write!(f, "invalid CPU description: {error:?}"),
            Self::Contract(error) => write!(f, "startup contract rejected: {error:?}"),
            error => write!(f, "{error:?}"),
        }
    }
}
