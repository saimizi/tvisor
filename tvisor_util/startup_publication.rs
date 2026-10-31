//! Consuming publication/release pipeline over validated frozen resources.
use crate::{
    PAGE_SIZE,
    cpu_topology::CpuDescriptor,
    secondary_entry::{EntryContract, validate_spin_table_release},
    secondary_mailbox::{Outcome, WorkerState},
    secondary_record::SecondaryRecord,
    startup_resources::FrozenStartup,
    system_info::{PhysAddr, PhysRegion},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    InvalidLineSize,
    Overflow,
    Preconditions,
    Io,
    NonzeroCell,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheLines {
    next: u64,
    end: u64,
    line: u64,
}
impl CacheLines {
    pub fn new(region: PhysRegion, line: u64) -> Result<Self, PublicationError> {
        if !line.is_power_of_two() || line < 8 || line > PAGE_SIZE as u64 {
            return Err(PublicationError::InvalidLineSize);
        }
        let end = region
            .end()
            .value()
            .checked_add(line - 1)
            .ok_or(PublicationError::Overflow)?
            & !(line - 1);
        Ok(Self {
            next: region.start().value() & !(line - 1),
            end,
            line,
        })
    }
}
impl Iterator for CacheLines {
    type Item = u64;
    fn next(&mut self) -> Option<u64> {
        if self.next == self.end {
            return None;
        }
        let value = self.next;
        self.next += self.line;
        Some(value)
    }
}
/// Callbacks are also the host-test seam for the production ordering protocol.
/// clean completes one DC CVAC issuance; complete is DSB SY, not just a compiler fence.
pub trait PublicationIo {
    /// Identity VA=PA in the hardware backend; hosted tests model PA separately.
    fn record_address(&self, record: &SecondaryRecord) -> u64 {
        record as *const _ as u64
    }

    fn clean(&mut self, address: u64) -> Result<(), PublicationError>;
    fn complete(&mut self) -> Result<(), PublicationError>;
    fn read_cell(&mut self, address: u64) -> Result<u64, PublicationError>;
    fn store_le64(&mut self, address: u64, value: u64) -> Result<(), PublicationError>;
    fn send_event(&mut self) -> Result<(), PublicationError>;
}

pub struct PublicationPlan {
    frozen: FrozenStartup,
    cell: u64,
    line: u64,
}
pub struct PublishedStartup {
    cell: u64,
    entry: u64,
    line: u64,
}
impl PublicationPlan {
    pub fn new(frozen: FrozenStartup, cell: u64, line: u64) -> Result<Self, PublicationError> {
        let r = frozen.resources();
        if cell & 7 != 0
            || !r.release_page.contains_region(
                PhysRegion::new(PhysAddr::new(cell), 8).map_err(|_| PublicationError::Overflow)?,
            )
        {
            return Err(PublicationError::Preconditions);
        }
        // Validate every rounded interval before issuing any maintenance.
        for region in Self::regions(&frozen)? {
            CacheLines::new(region, line)?;
        }
        CacheLines::new(r.release_page, line)?;
        Ok(Self { frozen, cell, line })
    }
    fn regions(frozen: &FrozenStartup) -> Result<[PhysRegion; 6], PublicationError> {
        let r = frozen.resources();
        let stack =
            PhysRegion::from_bounds(PhysAddr::new(r.stack.bottom), PhysAddr::new(r.stack.top))
                .map_err(|_| PublicationError::Preconditions)?;
        Ok([r.code, r.vectors, r.rodata, r.writable, r.tables, stack])
    }
    /// Consume actual frozen-resource evidence, the published production record
    /// and the platform contract; no caller-supplied resource-ready booleans.
    pub fn publish(
        self,
        cpus: &[CpuDescriptor],
        target: usize,
        backend: EntryContract,
        record: &SecondaryRecord,
        io: &mut impl PublicationIo,
    ) -> Result<PublishedStartup, PublicationError> {
        let address = validate_spin_table_release(cpus, target, backend)
            .map_err(|_| PublicationError::Preconditions)?;
        let r = self.frozen.resources();
        let cpu = cpus
            .iter()
            .find(|cpu| cpu.logical_id == Some(target))
            .ok_or(PublicationError::Preconditions)?;
        if record.mailbox.state(record.generation) != Ok(WorkerState::Ready)
            || record.mailbox.outcome() != Outcome::Pending
            || record.mair != crate::el2_policy::HOST_MAIR
            || record.sctlr != crate::el2_policy::HOST_SCTLR
            || !crate::el2_policy::HostTranslation::new(record.root, ((record.tcr >> 16) & 7) as u8)
                .is_ok_and(|p| p.tcr() == record.tcr)
            || io.record_address(record) != r.record.start().value()
            || address != self.cell
            || record.logical_id != target as u64
            || record.affinity != cpu.affinity
            || !record.mailbox.observe_ready(record.generation)
            || record.context != r.context.start().value()
            || record.root != r.root
            || record.stack_bottom != r.stack.bottom
            || record.stack_top != r.stack.top
        {
            return Err(PublicationError::Preconditions);
        }
        for region in Self::regions(&self.frozen)? {
            for line in CacheLines::new(region, self.line)? {
                io.clean(line)?;
            }
        }
        io.complete()?;
        Ok(PublishedStartup {
            cell: self.cell,
            entry: r.entry,
            line: self.line,
        })
    }
}
impl PublishedStartup {
    /// The handle is consumed even on failure. No retry or memory reuse is
    /// authorized, including if a backend reports an error after the store.
    pub fn release(self, io: &mut impl PublicationIo) -> Result<(), PublicationError> {
        if io.read_cell(self.cell)? != 0 {
            return Err(PublicationError::NonzeroCell);
        }
        io.store_le64(self.cell, self.entry)?;
        io.clean(self.cell & !(self.line - 1))?;
        io.complete()?;
        io.send_event()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cpu_topology::StartupMethod,
        el2_policy::HostTranslation,
        el2_stage1_translation::{Mapping, MemoryType, TableSet, TableStorage},
        secondary_record::{RecordStorage, StartupRecord},
        startup_resources::BootTables,
    };
    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        Clean(u64),
        Barrier,
        Read(u64),
        Store(u64, u64),
        Sev,
    }
    #[derive(Default)]
    struct Io {
        events: std::vec::Vec<Event>,
        value: u64,
        fail_at: Option<usize>,
        wrong_record: bool,
    }
    impl Io {
        fn event(&mut self, event: Event) -> Result<(), PublicationError> {
            self.events.push(event);
            if self.fail_at == Some(self.events.len()) {
                Err(PublicationError::Io)
            } else {
                Ok(())
            }
        }
    }
    impl PublicationIo for Io {
        fn record_address(&self, _: &SecondaryRecord) -> u64 {
            if self.wrong_record {
                0x103040
            } else {
                0x103000
            }
        }
        fn clean(&mut self, a: u64) -> Result<(), PublicationError> {
            self.event(Event::Clean(a))
        }
        fn complete(&mut self) -> Result<(), PublicationError> {
            self.event(Event::Barrier)
        }
        fn read_cell(&mut self, a: u64) -> Result<u64, PublicationError> {
            self.event(Event::Read(a))?;
            Ok(self.value)
        }
        fn store_le64(&mut self, a: u64, v: u64) -> Result<(), PublicationError> {
            self.event(Event::Store(a, v))
        }
        fn send_event(&mut self) -> Result<(), PublicationError> {
            self.event(Event::Sev)
        }
    }
    fn region(a: u64, n: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(a), n).unwrap()
    }
    fn fixture(cell: u64) -> (FrozenStartup, RecordStorage, [CpuDescriptor; 2]) {
        let r = crate::startup_resources::StartupResources {
            code: region(0x100000, 4096),
            vectors: region(0x101000, 4096),
            rodata: region(0x102000, 4096),
            writable: region(0x103000, 4096),
            tables: region(0x800000, 16 * 4096),
            record: region(0x103000, core::mem::size_of::<RecordStorage>() as u64),
            context: region(0x103800, 64),
            release_page: region(cell & !4095, 4096),
            stack: crate::pcpu::stack_layout(0x200000).unwrap(),
            entry: 0x100000,
            root: 0x800000,
        };
        let mut storage = TableStorage::<16>::zeroed();
        let mut tables = TableSet::new(&mut storage, r.root, 44).unwrap();
        for (p, w, x) in [
            (r.code, false, true),
            (r.vectors, false, true),
            (r.rodata, false, false),
            (r.writable, true, false),
            (r.tables, true, false),
            (r.release_page, true, false),
        ] {
            tables
                .map(Mapping {
                    va: p.start().value(),
                    pa: p.start().value(),
                    size: p.size(),
                    memory_type: MemoryType::Normal,
                    writable: w,
                    executable: x,
                })
                .unwrap();
        }
        crate::pcpu::map_stack(&mut tables, r.stack).unwrap();
        let mut gate = BootTables::new(tables);
        let frozen = gate.freeze_startup(r, core::iter::once(r.stack)).unwrap();
        let record = RecordStorage::new();
        let p = HostTranslation::new(r.root, 4).unwrap();
        record
            .publish(StartupRecord {
                logical_id: 1,
                affinity: 0x100,
                stack_bottom: r.stack.bottom,
                stack_top: r.stack.top,
                context: r.context.start().value(),
                root: p.root(),
                mair: p.mair(),
                tcr: p.tcr(),
                sctlr: p.sctlr(),
                generation: 7,
            })
            .unwrap();
        let cpus = [
            CpuDescriptor {
                affinity: 0,
                enabled: true,
                logical_id: Some(0),
                startup: StartupMethod::Missing,
            },
            CpuDescriptor {
                affinity: 0x100,
                enabled: true,
                logical_id: Some(1),
                startup: StartupMethod::SpinTable {
                    release_address: cell,
                },
            },
        ];
        (frozen, record, cpus)
    }
    fn backend() -> EntryContract {
        EntryContract {
            el2_mmu_off: true,
            coherent: true,
        }
    }
    #[test]
    fn cache_lines_round_partial_shared_lines_and_reject_overflow_or_unsafe_sizes() {
        assert_eq!(
            CacheLines::new(region(0x103001, 64), 64)
                .unwrap()
                .collect::<std::vec::Vec<_>>(),
            [0x103000, 0x103040]
        );
        assert_eq!(
            CacheLines::new(region(0, 8), 64)
                .unwrap()
                .collect::<std::vec::Vec<_>>(),
            [0]
        );
        for line in [0, 4, 7, 12, 8192] {
            assert_eq!(
                CacheLines::new(region(0, 4096), line),
                Err(PublicationError::InvalidLineSize)
            );
        }
        assert_eq!(
            CacheLines::new(region(u64::MAX - 8, 8), 64),
            Err(PublicationError::Overflow)
        );
    }
    #[test]
    fn every_required_range_is_cleaned_and_completed_before_the_only_store() {
        for cell in [0, 0xe0] {
            let (frozen, s, cpus) = fixture(cell);
            let mut io = Io::default();
            let ranges = PublicationPlan::regions(&frozen).unwrap();
            PublicationPlan::new(frozen, cell, 64)
                .unwrap()
                .publish(&cpus, 1, backend(), s.get(7).unwrap(), &mut io)
                .unwrap()
                .release(&mut io)
                .unwrap();
            let expected = ranges
                .into_iter()
                .flat_map(|r| CacheLines::new(r, 64).unwrap())
                .map(Event::Clean)
                .collect::<std::vec::Vec<_>>();
            assert_eq!(&io.events[..expected.len()], &expected);
            assert_eq!(
                &io.events[expected.len()..],
                &[
                    Event::Barrier,
                    Event::Read(cell),
                    Event::Store(cell, 0x100000),
                    Event::Clean(cell & !63),
                    Event::Barrier,
                    Event::Sev
                ]
            );
            assert!(!io.events.contains(&Event::Clean(0x200000))); // guard excluded
        }
    }
    #[test]
    fn unverified_backend_changed_target_and_stale_record_issue_no_maintenance_or_write() {
        for case in 0..8 {
            let (frozen, s, mut cpus) = fixture(0xe0);
            let mut io = Io::default();
            let mut b = backend();
            match case {
                0 => b.coherent = false,
                1 => b.el2_mmu_off = false,
                2 => cpus[1].affinity = 2,
                3 => cpus[1].startup = StartupMethod::Psci,
                4 => s
                    .get(7)
                    .unwrap()
                    .mailbox
                    .finish(7, Outcome::TimedOut)
                    .unwrap(),
                5 => io.wrong_record = true,
                6 => s.get(7).unwrap().mailbox.enter(7).unwrap(),
                _ => {
                    cpus[1].startup = StartupMethod::SpinTable {
                        release_address: 0xe8,
                    }
                }
            }
            assert!(
                PublicationPlan::new(frozen, 0xe0, 64)
                    .unwrap()
                    .publish(&cpus, 1, b, s.get(7).unwrap(), &mut io)
                    .is_err()
            );
            assert!(io.events.is_empty());
        }
    }
    #[test]
    fn maintenance_and_completion_failures_cannot_produce_a_release_handle() {
        let (frozen, s, cpus) = fixture(0xe0);
        let count = PublicationPlan::regions(&frozen)
            .unwrap()
            .into_iter()
            .map(|r| CacheLines::new(r, 64).unwrap().count())
            .sum::<usize>();
        drop(frozen);
        for failure in [1, count, count + 1] {
            let (frozen, s2, cpus2) = fixture(0xe0);
            let mut io = Io {
                fail_at: Some(failure),
                ..Io::default()
            };
            assert!(
                PublicationPlan::new(frozen, 0xe0, 64)
                    .unwrap()
                    .publish(&cpus2, 1, backend(), s2.get(7).unwrap(), &mut io)
                    .is_err()
            );
            assert!(
                !io.events
                    .iter()
                    .any(|e| matches!(e, Event::Store(..) | Event::Sev))
            );
        }
        let _ = (s, cpus);
    }
    #[test]
    fn nonzero_cell_and_each_release_stage_failure_stop_without_retry() {
        for failure in 0..=6 {
            let (frozen, s, cpus) = fixture(0xe0);
            let mut io = Io::default();
            let published = PublicationPlan::new(frozen, 0xe0, 64)
                .unwrap()
                .publish(&cpus, 1, backend(), s.get(7).unwrap(), &mut io)
                .unwrap();
            io.events.clear();
            if failure == 0 {
                io.value = 1;
            } else {
                io.fail_at = Some(failure);
            }
            let result = published.release(&mut io);
            if failure <= 5 {
                assert!(result.is_err());
            }
            assert!(
                io.events
                    .iter()
                    .filter(|e| matches!(e, Event::Store(..)))
                    .count()
                    <= 1
            );
            if failure <= 2 {
                assert!(!io.events.contains(&Event::Sev));
            }
            if failure == 0 {
                assert_eq!(io.events, [Event::Read(0xe0)]);
            }
            if failure > 0 && failure <= 5 {
                assert_eq!(io.events.len(), failure);
            }
        }
    }
}
