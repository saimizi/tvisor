//! Direct validated publication and release; persistent state lives in slots
//! and the aggregate boot policy. No intermediate release capabilities.
use crate::{
    PAGE_SIZE,
    cpu_topology::CpuDescriptor,
    secondary_entry::{EntryContract, validate_spin_table_release},
    secondary_hardware::*,
    secondary_mailbox::CpuState,
    secondary_startup::StartupView,
    startup_resources::StartupResources,
    system_info::{PhysAddr, PhysRegion},
};
use core::sync::atomic::{AtomicU64, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationError {
    InvalidLineSize,
    Overflow,
    Preconditions,
    Io,
    NonzeroCell,
}
/// Check every rounded address before issuing any maintenance. End is exclusive.
pub fn cache_range(start: u64, end: u64, line: u64) -> Result<(u64, u64), PublicationError> {
    if !line.is_power_of_two() || line < 8 || line > PAGE_SIZE as u64 {
        return Err(PublicationError::InvalidLineSize);
    }
    if start >= end {
        return Err(PublicationError::Preconditions);
    }
    let end = end
        .checked_add(line - 1)
        .ok_or(PublicationError::Overflow)?
        & !(line - 1);
    Ok((start & !(line - 1), end))
}
/// # Safety
/// The complete rounded interval must be mapped as validated Normal memory.
pub unsafe fn clean_range(start: u64, end: u64, line: u64) -> Result<(), PublicationError> {
    let (mut address, end) = cache_range(start, end, line)?;
    while address < end {
        unsafe { clean_cache_line(address)? };
        address += line;
    }
    Ok(())
}
fn regions(r: &StartupResources) -> Result<[PhysRegion; 6], PublicationError> {
    let stack = PhysRegion::from_bounds(PhysAddr::new(r.stack.bottom), PhysAddr::new(r.stack.top))
        .map_err(|_| PublicationError::Preconditions)?;
    Ok([r.code, r.vectors, r.rodata, r.writable, r.tables, stack])
}
pub fn validate_publication(
    cpus: &[CpuDescriptor],
    id: usize,
    contract: EntryContract,
    record: StartupView<'_>,
    r: &StartupResources,
    line: u64,
) -> Result<(), PublicationError> {
    let cell = validate_spin_table_release(cpus, id, contract)
        .map_err(|_| PublicationError::Preconditions)?;
    let cpu = cpus
        .iter()
        .find(|c| c.logical_id == Some(id))
        .ok_or(PublicationError::Preconditions)?;
    if record.mailbox.state(record.generation) != Ok(CpuState::Prepared)
        || record.config.mair != crate::el2_policy::HOST_MAIR
        || record.config.sctlr != crate::el2_policy::HOST_SCTLR
        || !crate::el2_policy::HostTranslation::new(
            record.config.root,
            ((record.config.tcr >> 16) & 7) as u8,
        )
        .is_ok_and(|p| p.tcr() == record.config.tcr)
        || record.slot_address() != r.context.start().value()
        || record.slot_address()
            != r.slots.start().value()
                + id as u64 * core::mem::size_of::<crate::pcpu::PcpuSlot<()>>() as u64
        || record.config_address() != r.config.start().value()
        || record.identity.logical_id != id as u64
        || record.identity.affinity != cpu.affinity
        || !record.mailbox.observe_ready(record.generation)
        || record.identity.context != record.slot_address()
        || record.config.root != r.root
        || record.identity.stack_bottom != r.stack.bottom
        || record.identity.stack_top != r.stack.top
        || r.entry & 3 != 0
        || !r.code.contains_address(PhysAddr::new(r.entry))
        || !r.release_page.contains_region(
            PhysRegion::new(PhysAddr::new(cell), 8).map_err(|_| PublicationError::Overflow)?,
        )
    {
        return Err(PublicationError::Preconditions);
    }
    for region in regions(r)?
        .into_iter()
        .chain(core::iter::once(r.release_page))
    {
        cache_range(region.start().value(), region.end().value(), line)?;
    }
    Ok(())
}
/// # Safety
/// CPU0 must have validated and frozen all startup mappings. The caller must
/// finish publication for every required CPU before beginning any release.
pub unsafe fn publish_startup(
    cpus: &[CpuDescriptor],
    id: usize,
    contract: EntryContract,
    record: StartupView<'_>,
    r: &StartupResources,
    line: u64,
) -> Result<(), PublicationError> {
    validate_publication(cpus, id, contract, record, r, line)?;
    for region in regions(r)? {
        unsafe { clean_range(region.start().value(), region.end().value(), line)? };
    }
    unsafe { publication_barrier() }
}
/// # Safety
/// Only CPU0 in the aggregate Releasing phase, after *all* publication barriers.
/// Frozen mappings, validated executable entry and resource ownership must be
/// retained until reset. The attempted mask is conservative, never an arrival.
pub unsafe fn release_secondary(
    cpus: &[CpuDescriptor],
    contract: EntryContract,
    record: StartupView<'_>,
    entry: u64,
    line: u64,
    attempted: &AtomicU64,
) -> Result<u64, PublicationError> {
    let id =
        usize::try_from(record.identity.logical_id).map_err(|_| PublicationError::Preconditions)?;
    let cell = validate_spin_table_release(cpus, id, contract)
        .map_err(|_| PublicationError::Preconditions)?;
    let cpu = cpus
        .iter()
        .find(|c| c.logical_id == Some(id))
        .ok_or(PublicationError::Preconditions)?;
    cache_range(
        cell,
        cell.checked_add(8).ok_or(PublicationError::Overflow)?,
        line,
    )?;
    if entry == 0
        || entry & 3 != 0
        || record.identity.affinity != cpu.affinity
        || record.identity.context != record.slot_address()
        || record.mailbox.state(record.generation) != Ok(CpuState::Prepared)
        || attempted.load(Ordering::Acquire) & (1 << id) != 0
    {
        return Err(PublicationError::Preconditions);
    }
    if unsafe { read_release_cell(cell)? } != 0 {
        return Err(PublicationError::NonzeroCell);
    }
    record
        .mailbox
        .begin_start(record.generation)
        .map_err(|_| PublicationError::Preconditions)?;
    unsafe { publication_barrier()? };
    // Preserve the last-moment read after publishing Starting.
    if unsafe { read_release_cell(cell)? } != 0 {
        return Err(PublicationError::NonzeroCell);
    }
    attempted
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |mask| {
            if mask & (1 << id) == 0 {
                Some(mask | (1 << id))
            } else {
                None
            }
        })
        .map_err(|_| PublicationError::Preconditions)?;
    let timestamp = unsafe { write_release_cell_le64(cell, entry)? };
    unsafe {
        clean_cache_line(cell & !(line - 1))?;
        publication_barrier()?;
        send_event()?;
    }
    Ok(timestamp)
}

/// Check cumulative firmware-cell readback after each successful arrival.
pub fn verify_release_value(
    cpus: &[CpuDescriptor],
    attempted: u64,
    cell: u64,
    entry: u64,
    value: u64,
) -> Result<(), PublicationError> {
    let required = crate::secondary_entry::secondary_mask(cpus)
        .map_err(|_| PublicationError::Preconditions)?;
    if attempted & !required != 0 {
        return Err(PublicationError::Preconditions);
    }
    let released = cpus.iter().any(|cpu| {
        cpu.logical_id.is_some_and(|id| attempted & (1 << id) != 0)
            && cpu.startup
                == crate::cpu_topology::StartupMethod::SpinTable {
                    release_address: cell,
                }
    });
    if value == if released { entry } else { 0 } {
        Ok(())
    } else {
        Err(PublicationError::Preconditions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cpu_topology::StartupMethod,
        secondary_boot::{self, BootError, BootStatus},
        secondary_hardware::test_io::{self, Event, IO},
        secondary_startup::test_fixture::Fixture,
    };
    fn region(a: u64, n: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(a), n).unwrap()
    }
    fn contract() -> EntryContract {
        EntryContract {
            el2_mmu_off: true,
            coherent: true,
        }
    }
    fn cpus(cell: u64) -> [CpuDescriptor; 4] {
        core::array::from_fn(|id| CpuDescriptor {
            affinity: id as u64,
            enabled: true,
            logical_id: Some(id),
            startup: if id == 0 {
                StartupMethod::Missing
            } else {
                StartupMethod::SpinTable {
                    release_address: cell + (id as u64 - 1) * 8,
                }
            },
        })
    }
    fn resources(f: &Fixture, id: usize, cell: u64) -> StartupResources {
        let r = f.get(7).unwrap();
        StartupResources {
            code: region(0x100000, 4096),
            vectors: region(0x101000, 4096),
            rodata: region(0x102000, 4096),
            writable: region(0x103000, 4096),
            tables: region(0x800000, 16 * 4096),
            slots: region(r.slot_address() - id as u64 * 384, 4 * 384),
            context: region(r.slot_address(), 64),
            config: region(r.config_address(), 128),
            release_page: region(cell & !4095, 4096),
            stack: crate::pcpu::stack_layout(r.identity.stack_bottom - 4096).unwrap(),
            entry: 0x100000,
            root: 0x800000,
        }
    }
    fn fixture(id: usize) -> std::boxed::Box<Fixture> {
        Fixture::new(id, id as u64, 0x201000 + id as u64 * 0x20000, 0x800000, 7)
    }
    #[test]
    fn checked_clean_range_rounds_shared_lines_including_zero_and_rejects_overflow_before_io() {
        test_io::reset();
        unsafe { clean_range(0x103001, 0x103041, 64) }.unwrap();
        assert_eq!(
            IO.with(|io| io.borrow().events.clone()),
            [Event::Clean(0x103000), Event::Clean(0x103040)]
        );
        test_io::reset();
        unsafe { clean_range(0, 8, 64) }.unwrap();
        assert_eq!(IO.with(|io| io.borrow().events.clone()), [Event::Clean(0)]);
        for line in [0, 4, 7, 12, 8192] {
            assert_eq!(
                cache_range(0, 4096, line),
                Err(PublicationError::InvalidLineSize)
            );
        }
        test_io::reset();
        assert_eq!(
            unsafe { clean_range(u64::MAX - 8, u64::MAX, 64) },
            Err(PublicationError::Overflow)
        );
        assert!(IO.with(|io| io.borrow().events.is_empty()));
        assert!(cache_range(64, 64, 64).is_err());
    }
    #[test]
    fn all_publications_complete_before_any_release_and_exact_cell_sequence_is_preserved() {
        for cell in [0, 0xe0] {
            test_io::reset();
            let cpus = cpus(cell);
            let fixtures = [fixture(1), fixture(2), fixture(3)];
            let attempted = AtomicU64::new(0);
            let mut expected = std::vec::Vec::new();
            for (i, f) in fixtures.iter().enumerate() {
                let r = resources(f, i + 1, cell + (i as u64) * 8);
                unsafe { publish_startup(&cpus, i + 1, contract(), f.get(7).unwrap(), &r, 64) }
                    .unwrap();
                for range in regions(&r).unwrap() {
                    let (start, end) =
                        cache_range(range.start().value(), range.end().value(), 64).unwrap();
                    expected.extend((start..end).step_by(64).map(Event::Clean));
                }
                expected.push(Event::Barrier);
            }
            assert_eq!(IO.with(|io| io.borrow().events.clone()), expected);
            for (i, f) in fixtures.iter().enumerate() {
                let address = cell + i as u64 * 8;
                assert_eq!(
                    unsafe {
                        release_secondary(
                            &cpus,
                            contract(),
                            f.get(7).unwrap(),
                            0x100000,
                            64,
                            &attempted,
                        )
                    },
                    Ok(123)
                );
                expected.extend([
                    Event::Read(address),
                    Event::Barrier,
                    Event::Read(address),
                    Event::Store(address, 0x100000u64.to_le()),
                    Event::Clean(address & !63),
                    Event::Barrier,
                    Event::Sev,
                ]);
            }
            assert_eq!(attempted.load(Ordering::Acquire), 14);
            assert_eq!(IO.with(|io| io.borrow().events.clone()), expected);
            for f in &fixtures {
                assert!(!expected.contains(&Event::Clean(
                    f.get(7).unwrap().identity.stack_bottom - 4096
                )));
            }
        }
    }
    #[test]
    fn every_publication_failure_prevents_release_and_is_terminal() {
        for position in [1, 64, 1024, 2305] {
            test_io::reset();
            let f = fixture(1);
            let r = resources(&f, 1, 0xe0);
            let state = AtomicU64::new(0);
            secondary_boot::begin_boot_once(&state).unwrap();
            IO.with(|io| io.borrow_mut().fail_at = Some(position));
            let result =
                unsafe { publish_startup(&cpus(0xe0), 1, contract(), f.get(7).unwrap(), &r, 64) };
            assert!(result.is_err(), "failure at {position}");
            secondary_boot::finalize_boot(&state, Err(BootError::Preparation("publication")))
                .unwrap_err();
            assert!(secondary_boot::begin_boot_once(&state).is_err());
            assert!(IO.with(|io| {
                !io.borrow()
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::Store(..) | Event::Sev))
            }));
        }
    }
    #[test]
    fn invalid_identity_generation_backend_entry_and_ranges_issue_no_maintenance() {
        for case in 0..12 {
            test_io::reset();
            let f = fixture(1);
            let mut r = resources(&f, 1, 0xe0);
            let mut cpus = cpus(0xe0);
            let mut c = contract();
            let mut v = f.get(7).unwrap();
            let mut line = 64;
            match case {
                0 => c.coherent = false,
                1 => c.el2_mmu_off = false,
                2 => cpus[1].affinity = 8,
                3 => cpus[1].startup = StartupMethod::Psci,
                4 => r.context = region(r.context.start().value() + 64, 64),
                5 => r.config = region(r.config.start().value() + 64, 128),
                6 => v.generation = 8,
                7 => r.entry = r.code.end().value(),
                8 => r.release_page = region(4096, 4096),
                9 => line = 7,
                10 => cpus[2].logical_id = Some(1),
                _ => cpus[2].affinity = 1,
            }
            assert!(unsafe { publish_startup(&cpus, 1, c, v, &r, line) }.is_err());
            assert!(IO.with(|io| io.borrow().events.is_empty()));
        }
    }
    #[test]
    fn nonzero_cell_and_last_moment_recheck_prevent_the_store() {
        for late in [false, true] {
            test_io::reset();
            let f = fixture(1);
            let attempts = AtomicU64::new(0);
            IO.with(|io| {
                let mut h = io.borrow_mut();
                if late {
                    h.nonzero_at_read = Some(3);
                } else {
                    h.cells.insert(0xe0, 1);
                }
            });
            assert_eq!(
                unsafe {
                    release_secondary(
                        &cpus(0xe0),
                        contract(),
                        f.get(7).unwrap(),
                        0x100000,
                        64,
                        &attempts,
                    )
                },
                Err(PublicationError::NonzeroCell)
            );
            assert_eq!(attempts.load(Ordering::Acquire), 0);
            assert!(IO.with(|io| {
                !io.borrow()
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::Store(..)))
            }));
        }
    }
    #[test]
    fn every_release_failure_retains_potential_attempt_after_ambiguous_store_and_never_retries() {
        for position in 1..=7 {
            test_io::reset();
            let f = fixture(1);
            let attempts = AtomicU64::new(0);
            let state = AtomicU64::new(0);
            secondary_boot::begin_boot_once(&state).unwrap();
            secondary_boot::begin_release(&state).unwrap();
            IO.with(|io| io.borrow_mut().fail_at = Some(position));
            let result = unsafe {
                release_secondary(
                    &cpus(0xe0),
                    contract(),
                    f.get(7).unwrap(),
                    0x100000,
                    64,
                    &attempts,
                )
            };
            assert!(result.is_err());
            assert_eq!(
                attempts.load(Ordering::Acquire),
                if position >= 4 { 2 } else { 0 }
            );
            if position >= 4 {
                assert_eq!(
                    IO.with(|io| io.borrow().cells.get(&0xe0).copied()),
                    Some(0x100000)
                );
            }
            secondary_boot::finalize_boot(&state, result.map(|_| ()).map_err(BootError::Release))
                .unwrap_err();
            assert_eq!(
                secondary_boot::boot_status(&state),
                BootStatus::ReleaseFailed
            );
            assert!(secondary_boot::begin_boot_once(&state).is_err());
            assert!(secondary_boot::guest_authorized(&state, false).is_err());
        }
    }
    #[test]
    fn a_successful_release_cannot_be_repeated_even_if_firmware_clears_the_cell() {
        test_io::reset();
        let f = fixture(1);
        let attempts = AtomicU64::new(0);
        unsafe {
            release_secondary(
                &cpus(0xe0),
                contract(),
                f.get(7).unwrap(),
                0x100000,
                64,
                &attempts,
            )
        }
        .unwrap();
        IO.with(|io| {
            let mut h = io.borrow_mut();
            h.cells.clear();
            h.events.clear();
        });
        assert!(
            unsafe {
                release_secondary(
                    &cpus(0xe0),
                    contract(),
                    f.get(7).unwrap(),
                    0x100000,
                    64,
                    &attempts,
                )
            }
            .is_err()
        );
        assert!(IO.with(|io| io.borrow().events.is_empty()));
    }
}

#[cfg(test)]
mod readback_tests {
    use super::*;
    use crate::cpu_topology::StartupMethod;
    #[test]
    fn cumulative_readback_checks_released_and_unreleased_cells() {
        let cpus = core::array::from_fn::<_, 4, _>(|id| CpuDescriptor {
            affinity: id as u64,
            logical_id: Some(id),
            enabled: true,
            startup: StartupMethod::SpinTable {
                release_address: 0xd8 + id as u64 * 8,
            },
        });
        for mask in [2, 6, 14] {
            for id in 0..4 {
                let expected = if mask & (1 << id) != 0 { 0x4001000 } else { 0 };
                let cell = 0xd8 + id * 8;
                assert!(verify_release_value(&cpus, mask, cell, 0x4001000, expected).is_ok());
                assert!(verify_release_value(&cpus, mask, cell, 0x4001000, expected ^ 8).is_err());
            }
        }
        assert!(verify_release_value(&cpus, 15, 0xd8, 0x4001000, 0x4001000).is_err());
    }
}
