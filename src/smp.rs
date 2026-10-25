//! Phase 11.0 boot-CPU discovery only. No CPU release operations live here.
use core::arch::asm;
use dtoolkit::{fdt::Fdt, standard::NodeStandard};
use spin::Once;
use tvisor_util::{
    aarch64_reg::MpidrEl1,
    cpu_topology::{CpuDiscoveryError, discover_cpu_topology},
    println,
    rpi4_startup::{self, ARMSTUB_SIZE, SpinTableContract, StartupContractError},
};

static STARTUP_CONTRACT: Once<SpinTableContract> = Once::new();

#[derive(Debug)]
pub enum DiscoveryError {
    UnsupportedPlatform,
    MissingMpidr,
    Topology(CpuDiscoveryError),
    Contract(StartupContractError),
    LowPageNotIdentityMapped,
    BootCpuSmpenDisabled,
}

pub fn startup_contract() -> Option<&'static SpinTableContract> {
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

    // This reviewed profile is specific to BCM2711 Cortex-A72. Generic PSCI
    // discovery remains available, but does not authorize SMC/HVC or reclaiming
    // a different firmware's live secondary memory.
    if !fdt.root().is_compatible("brcm,bcm2711") {
        return Err(DiscoveryError::UnsupportedPlatform);
    }
    rpi4_startup::verify_dt_reservation(fdt).map_err(DiscoveryError::Contract)?;
    let (midr, cpuectlr): (u64, u64);
    unsafe {
        asm!("mrs {value}, MIDR_EL1", value = out(reg) midr, options(nomem, nostack, preserves_flags));
    }
    if (midr >> 24) != 0x41 || ((midr >> 4) & 0xfff) != 0xd08 {
        return Err(DiscoveryError::UnsupportedPlatform);
    }
    unsafe {
        asm!("mrs {value}, S3_1_C15_C2_1", value = out(reg) cpuectlr, options(nomem, nostack, preserves_flags));
    }
    if cpuectlr & (1 << 6) == 0 {
        return Err(DiscoveryError::BootCpuSmpenDisabled);
    }

    // The low page is not mapped by tvisor's later stage-1 root. Capture it
    // before takeover, but first verify U-Boot's current mapping via AT. Rust
    // must never form/dereference a null pointer even for physical RAM at zero.
    let (saved_par, par): (u64, u64);
    unsafe {
        asm!(
            "mrs {saved}, PAR_EL1", "at s1e2r, {address}", "isb",
            "mrs {result}, PAR_EL1", "msr PAR_EL1, {saved}",
            saved = out(reg) saved_par, result = out(reg) par,
            address = in(reg) 0u64,
            options(nostack, preserves_flags),
        );
    }
    let _ = saved_par;
    if par & 1 != 0 || par & 0x000f_ffff_ffff_f000 != 0 {
        return Err(DiscoveryError::LowPageNotIdentityMapped);
    }
    let mut snapshot = [0u8; ARMSTUB_SIZE];
    for (address, byte) in snapshot.iter_mut().enumerate() {
        let value: u32;
        // SAFETY: this is the verified mapped RAM page in the supported board
        // profile. The assembly loads bytes without Rust null-pointer access.
        unsafe {
            asm!("ldrb {value:w}, [{address}]", value = out(reg) value,
                 address = in(reg) address, options(nostack, readonly, preserves_flags));
        }
        *byte = value as u8;
    }
    let contract = rpi4_startup::verify_spin_table_contract(&topology, &snapshot)
        .map_err(DiscoveryError::Contract)?;
    println!(
        "    Verified BCM2711 armstub8-gic profile: immutable bytes match, release cells zero"
    );
    println!(
        "    Secondary contract: NS AArch64 EL2h, DAIF masked, MMU/D-cache/I-cache off, x0-x3=0"
    );
    println!(
        "    Firmware sets CPUECTLR_EL1.SMPEN before park; boot CPU CPUECTLR_EL1={:#x}",
        cpuectlr
    );
    println!(
        "    Holding memory={} (permanent DT reservation)",
        contract.holding_memory()
    );
    println!("    Secondary activation disabled (Phase 11.0 discovery only)");
    STARTUP_CONTRACT.call_once(|| contract);
    Ok(())
}

impl core::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Topology(error) => write!(f, "invalid CPU description: {error:?}"),
            Self::Contract(error) => write!(f, "startup contract rejected: {error:?}"),
            error => write!(f, "{error:?}"),
        }
    }
}
