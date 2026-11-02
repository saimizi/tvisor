//! DT startup discovery and the validated spin-table hardware publication backend.
use core::arch::asm;
use dtoolkit::fdt::Fdt;
use spin::Once;
use tvisor_util::{
    aarch64_reg::MpidrEl1,
    cpu_startup::{StartupContractError, StartupMemoryContract, discover_startup_memory},
    cpu_topology::{CpuDiscoveryError, CpuTopology, discover_cpu_topology},
    diagnostic_println,
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
    diagnostic_println!("Physical CPU discovery: boot MPIDR={:#x}", mpidr);
    #[cfg(feature = "diagnostics")]
    for cpu in topology.cpus() {
        diagnostic_println!("    {:?}", cpu);
    }
    diagnostic_println!("    PSCI DT description: {:?}", topology.psci);
    if topology.psci.is_some() {
        diagnostic_println!(
            "    PSCI EL2 eligibility/version/CPU_ON/entry contract: unverified; no firmware calls"
        );
    } else {
        diagnostic_println!("    PSCI: not advertised; conduit/version/CPU_ON queries skipped");
    }

    let contract = discover_startup_memory(fdt, &topology).map_err(DiscoveryError::Contract)?;
    // Validate all descriptions and reservation ownership before touching any
    // cell. Read only DT-advertised cells in reserved RAM, never firmware code.
    contract
        .verify_release_cells(read_release_cell)
        .map_err(DiscoveryError::Contract)?;
    #[cfg(feature = "diagnostics")]
    for cell in contract.release_cells() {
        diagnostic_println!("    Spin-table release cell={} initial value=0", cell);
    }
    diagnostic_println!(
        "    DT startup interface validated; firmware instructions are not inspected"
    );
    diagnostic_println!(
        "    Secondary EL2/MMU/cache/coherency state must be validated at 11.3 entry"
    );
    diagnostic_println!("    Secondary release deferred until Phase 11.3 final boot preflight");
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
    // SAFETY: discovery validated owned aligned RAM and AT confirmed identity
    // mapping. The assembly helper supports PA zero without a Rust reference.
    unsafe { tvisor_util::secondary_hardware::read_release_cell(address.value()) }
        .map_err(|_| StartupContractError::ReleaseCellNotIdentityMapped)
}

/// Read-only preflight; the irreversible helper rechecks immediately before store.
pub fn verify_unreleased_cell(address: PhysAddr) -> Result<(), StartupContractError> {
    if !startup_contract().is_some_and(|contract| {
        contract
            .release_cells()
            .iter()
            .any(|cell| cell.start() == address)
    }) {
        return Err(StartupContractError::MissingHoldingReservation);
    }
    if read_release_cell(address)? != 0 {
        return Err(StartupContractError::NonzeroReleaseCell);
    }
    Ok(())
}

/// CPU0 validates the cumulative released set; untouched cells must remain zero.
pub fn verify_release_set(released: u64, entry: u64) -> Result<(), &'static str> {
    let contract = startup_contract().ok_or("startup contract unavailable")?;
    let topology = topology().ok_or("topology unavailable")?;
    for cell in contract.release_cells() {
        let value = read_release_cell(cell.start()).map_err(|_| "release-cell readback failed")?;
        diagnostic_println!(
            "Phase 11.5 release readback cell={:#x} value={:#x}",
            cell.start().value(),
            value
        );
        if tvisor_util::startup_publication::verify_release_value(
            topology.cpus(),
            released,
            cell.start().value(),
            entry,
            value,
        )
        .is_err()
        {
            return Err("unexpected release-cell value");
        }
    }
    Ok(())
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
