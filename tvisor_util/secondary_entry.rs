//! Preflight validation for the first one-CPU spin-table startup attempt.
//! Validation describes prerequisites; it does not itself release a CPU.
use crate::cpu_topology::{CpuDescriptor, StartupMethod};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryContract {
    /// A backend-specific contract establishes that the entry will be at EL2
    /// with translation disabled, in AArch64, before release is attempted.
    pub el2_mmu_off: bool,
    /// The platform contract establishes membership in the coherency domain.
    pub coherent: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryError {
    MissingTarget,
    InvalidTarget,
    UnsupportedMethod,
    SharedReleaseCell,
    EntryContractUnverified,
    CoherencyUnverified,
}

/// Select the lowest enabled non-boot logical CPU. Backend and release-cell
/// validation are separate: a failing target must not silently fall back to a
/// different CPU or startup mechanism.
pub fn select_secondary(cpus: &[CpuDescriptor]) -> Result<usize, EntryError> {
    cpus.iter()
        .filter(|cpu| cpu.enabled)
        .filter_map(|cpu| cpu.logical_id)
        .filter(|&id| id != 0)
        .min()
        .ok_or(EntryError::MissingTarget)
}

/// Validate the backend/target portion of release preflight. Resource readiness
/// is enforced separately by the consuming FrozenStartup/PublishedStartup path;
/// this helper alone never authorizes a firmware write.
pub fn validate_spin_table_release(
    cpus: &[CpuDescriptor],
    target: usize,
    contract: EntryContract,
) -> Result<u64, EntryError> {
    let release_address = validate_spin_table_target(cpus, target)?;
    if !contract.el2_mmu_off {
        return Err(EntryError::EntryContractUnverified);
    }
    if !contract.coherent {
        return Err(EntryError::CoherencyUnverified);
    }
    Ok(release_address)
}

/// Validate target ownership independently of publication state.
pub fn validate_spin_table_target(
    cpus: &[CpuDescriptor],
    target: usize,
) -> Result<u64, EntryError> {
    let cpu = cpus
        .iter()
        .find(|cpu| cpu.logical_id == Some(target))
        .ok_or(EntryError::MissingTarget)?;
    if target == 0 || !cpu.enabled {
        return Err(EntryError::InvalidTarget);
    }
    let StartupMethod::SpinTable { release_address } = cpu.startup else {
        return Err(EntryError::UnsupportedMethod);
    };
    if release_address & 7 != 0 {
        return Err(EntryError::InvalidTarget);
    }
    if cpus.iter().any(|other| {
        !core::ptr::eq(other, cpu)
            && other.startup == (StartupMethod::SpinTable { release_address })
    }) {
        return Err(EntryError::SharedReleaseCell);
    }
    Ok(release_address)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cpu(id: usize, startup: StartupMethod) -> CpuDescriptor {
        CpuDescriptor {
            affinity: id as u64,
            enabled: true,
            logical_id: Some(id),
            startup,
        }
    }
    fn contract() -> EntryContract {
        EntryContract {
            el2_mmu_off: true,
            coherent: true,
        }
    }
    #[test]
    fn selects_lowest_enabled_non_boot_logical_id() {
        let method = StartupMethod::Psci;
        let disabled = CpuDescriptor {
            enabled: false,
            ..cpu(1, method)
        };
        assert_eq!(
            select_secondary(&[cpu(3, method), disabled, cpu(2, method), cpu(0, method)]),
            Ok(2)
        );
        assert_eq!(
            select_secondary(&[cpu(0, method), disabled]),
            Err(EntryError::MissingTarget)
        );
    }
    #[test]
    fn rejects_each_missing_entry_prerequisite() {
        let cpus = [
            cpu(
                0,
                StartupMethod::SpinTable {
                    release_address: 0xd8,
                },
            ),
            cpu(
                1,
                StartupMethod::SpinTable {
                    release_address: 0xe0,
                },
            ),
        ];
        let valid = contract();
        assert_eq!(validate_spin_table_release(&cpus, 1, valid), Ok(0xe0));
        assert_eq!(
            validate_spin_table_release(&cpus, 0, valid),
            Err(EntryError::InvalidTarget)
        );
        assert_eq!(
            validate_spin_table_release(&cpus, 2, valid),
            Err(EntryError::MissingTarget)
        );
        assert_eq!(
            validate_spin_table_release(
                &cpus,
                1,
                EntryContract {
                    el2_mmu_off: false,
                    ..valid
                }
            ),
            Err(EntryError::EntryContractUnverified)
        );
        assert_eq!(
            validate_spin_table_release(
                &cpus,
                1,
                EntryContract {
                    coherent: false,
                    ..valid
                }
            ),
            Err(EntryError::CoherencyUnverified)
        );
    }
    #[test]
    fn rejects_shared_cell_and_unavailable_backend() {
        let same = StartupMethod::SpinTable {
            release_address: 0xe0,
        };
        let cpus = [cpu(1, same), cpu(2, same)];
        assert_eq!(
            validate_spin_table_release(&cpus, 1, contract()),
            Err(EntryError::SharedReleaseCell)
        );
        let disabled = CpuDescriptor {
            enabled: false,
            logical_id: None,
            ..cpu(2, same)
        };
        assert_eq!(
            validate_spin_table_release(&[cpu(1, same), disabled], 1, contract()),
            Err(EntryError::SharedReleaseCell)
        );
        assert_eq!(
            validate_spin_table_release(&[cpu(1, StartupMethod::Psci)], 1, contract()),
            Err(EntryError::UnsupportedMethod)
        );
        assert_eq!(
            validate_spin_table_release(
                &[cpu(1, StartupMethod::SpinTable { release_address: 3 })],
                1,
                contract()
            ),
            Err(EntryError::InvalidTarget)
        );
    }
}
