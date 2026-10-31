//! Allocation-free validation of the production secondary checkpoint.
use crate::{cpu_topology::MPIDR_AFFINITY_MASK, el2_policy::*, secondary_record::SecondaryRecord};
use core::sync::atomic::Ordering;
#[derive(Clone, Copy, Debug)]
pub struct LocalState {
    pub current_el: u64,
    pub affinity: u64,
    pub sp: u64,
    pub tpidr: u64,
    pub vbar: u64,
    pub mair: u64,
    pub tcr: u64,
    pub root: u64,
    pub sctlr: u64,
    pub daif: u64,
    pub hcr: u64,
    pub cptr: u64,
    pub hstr: u64,
    pub vttbr: u64,
    pub cntvoff: u64,
    pub cnthctl: u64,
    pub cnthp_ctl: u64,
    pub cntp_ctl: u64,
    pub cntv_ctl: u64,
    pub spsel: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct EntryState {
    pub sctlr: u64,
    pub midr: u64,
    pub cpuectlr: u64,
    pub mpidr: u64,
}
/// Bit positions are stable diagnostic identifiers; multiple defects survive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failures(pub u64);
pub const FAILURE_NAMES: &[&str] = &[
    "Generation",
    "ExceptionLevel",
    "Affinity",
    "Stack",
    "ContextPointer",
    "ContextIdentity",
    "Vectors",
    "Mair",
    "Tcr",
    "Root",
    "Sctlr",
    "InterruptMasks",
    "Hcr",
    "Cptr",
    "Hstr",
    "Vttbr",
    "CounterOffset",
    "CounterAccess",
    "HypTimer",
    "PhysicalTimer",
    "VirtualTimer",
    "StackSelection",
    "MemoryMap",
    "EntryRegime",
    "CpuImplementation",
    "MaintenanceParticipation",
    "EntryAffinity",
    "LogicalId",
];
pub fn validate(
    r: &SecondaryRecord,
    s: LocalState,
    e: EntryState,
    vectors: u64,
    context_valid: bool,
    map_published: bool,
) -> Failures {
    let checks = [
        r.generation != 0 && r.mailbox.observe_ready(r.generation), // Generation
        s.current_el == 8,                                          // ExceptionLevel
        s.affinity & MPIDR_AFFINITY_MASK == r.affinity,             // Affinity
        s.sp >= r.stack_bottom && s.sp < r.stack_top && s.sp & 15 == 0, // Stack
        s.tpidr == r.context,                                       // ContextPointer
        context_valid,                                              // ContextIdentity
        s.vbar == vectors && vectors != 0 && vectors & 2047 == 0,   // Vectors
        s.mair == r.mair && s.mair == HOST_MAIR,                    // Mair
        s.tcr == r.tcr
            && crate::el2_policy::HostTranslation::new(r.root, ((r.tcr >> 16) & 7) as u8)
                .is_ok_and(|p| p.tcr() == r.tcr), // Tcr
        s.root == r.root,                                           // Root
        s.sctlr == r.sctlr && s.sctlr == HOST_SCTLR,                // Sctlr
        s.daif & MASKED_DAIF == MASKED_DAIF,                        // InterruptMasks
        s.hcr == HOST_HCR,                                          // Hcr
        s.cptr == HOST_CPTR,                                        // Cptr
        s.hstr == HOST_HSTR,                                        // Hstr
        s.vttbr == HOST_VTTBR,                                      // Vttbr
        s.cntvoff == HOST_CNTVOFF,                                  // CounterOffset
        s.cnthctl == HOST_CNTHCTL,                                  // CounterAccess
        s.cnthp_ctl & 3 == DISABLED_TIMER_CTL,                      // HypTimer
        s.cntp_ctl & 3 == DISABLED_TIMER_CTL,                       // PhysicalTimer
        s.cntv_ctl & 3 == DISABLED_TIMER_CTL,                       // VirtualTimer
        s.spsel == 1,                                               // StackSelection
        map_published,                                              // MemoryMap
        e.sctlr & ((1 << 25) | (1 << 2) | 1) == 0,                  // EntryRegime
        (e.midr >> 24) & 255 == 0x41 && (e.midr >> 4) & 4095 == 0xd08, // CpuImplementation
        e.cpuectlr & (1 << 6) != 0,                                 // MaintenanceParticipation
        e.mpidr & MPIDR_AFFINITY_MASK == r.affinity,                // EntryAffinity
        r.logical_id > 0 && r.logical_id < crate::cpu_topology::MAX_CPUS as u64, // LogicalId
    ];
    Failures(
        checks
            .iter()
            .enumerate()
            .fold(0, |bits, (i, ok)| bits | if *ok { 0 } else { 1 << i }),
    )
}
/// Called only by the entrant that won mailbox.enter, before arrival/failure.
/// Rejected duplicate entrants must not call this writer.
pub fn publish_checkpoint(r: &SecondaryRecord, s: LocalState, e: EntryState, failures: Failures) {
    r.checkpoint
        .current_el
        .store(s.current_el, Ordering::Relaxed);
    r.checkpoint.affinity.store(s.affinity, Ordering::Relaxed);
    r.checkpoint.sp.store(s.sp, Ordering::Relaxed);
    r.checkpoint.tpidr.store(s.tpidr, Ordering::Relaxed);
    r.checkpoint.vbar.store(s.vbar, Ordering::Relaxed);
    r.checkpoint.mair.store(s.mair, Ordering::Relaxed);
    r.checkpoint.tcr.store(s.tcr, Ordering::Relaxed);
    r.checkpoint.root.store(s.root, Ordering::Relaxed);
    r.checkpoint.sctlr.store(s.sctlr, Ordering::Relaxed);
    r.checkpoint.daif.store(s.daif, Ordering::Relaxed);
    r.checkpoint.hcr.store(s.hcr, Ordering::Relaxed);
    r.checkpoint.cptr.store(s.cptr, Ordering::Relaxed);
    r.checkpoint.hstr.store(s.hstr, Ordering::Relaxed);
    r.checkpoint.vttbr.store(s.vttbr, Ordering::Relaxed);
    r.checkpoint.cntvoff.store(s.cntvoff, Ordering::Relaxed);
    r.checkpoint.cnthctl.store(s.cnthctl, Ordering::Relaxed);
    r.checkpoint.cnthp_ctl.store(s.cnthp_ctl, Ordering::Relaxed);
    r.checkpoint.cntp_ctl.store(s.cntp_ctl, Ordering::Relaxed);
    r.checkpoint.cntv_ctl.store(s.cntv_ctl, Ordering::Relaxed);
    r.checkpoint.spsel.store(s.spsel, Ordering::Relaxed);
    r.entry.sctlr.store(e.sctlr, Ordering::Relaxed);
    r.entry.midr.store(e.midr, Ordering::Relaxed);
    r.entry.cpuectlr.store(e.cpuectlr, Ordering::Relaxed);
    r.entry.mpidr.store(e.mpidr, Ordering::Relaxed);
    r.failures.store(failures.0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        secondary_mailbox::{Outcome, WorkerState},
        secondary_record::{RecordStorage, StartupRecord},
    };
    fn fixture() -> (RecordStorage, LocalState, EntryState) {
        let p = HostTranslation::new(0x430000, 4).unwrap();
        let storage = RecordStorage::new();
        storage
            .publish(StartupRecord {
                logical_id: 1,
                affinity: 0x100,
                context: 0x420000,
                stack_bottom: 0x800000,
                stack_top: 0x810000,
                generation: 7,
                root: p.root(),
                mair: p.mair(),
                tcr: p.tcr(),
                sctlr: p.sctlr(),
            })
            .unwrap();
        let state = LocalState {
            current_el: 8,
            affinity: 0x80000100,
            sp: 0x80ff00,
            tpidr: 0x420000,
            vbar: 0x440000,
            mair: p.mair(),
            tcr: p.tcr(),
            root: p.root(),
            sctlr: p.sctlr(),
            daif: MASKED_DAIF,
            hcr: HOST_HCR,
            cptr: HOST_CPTR,
            hstr: HOST_HSTR,
            vttbr: HOST_VTTBR,
            cntvoff: HOST_CNTVOFF,
            cnthctl: HOST_CNTHCTL,
            cnthp_ctl: 0,
            cntp_ctl: 0,
            cntv_ctl: 0,
            spsel: 1,
        };
        (
            storage,
            state,
            EntryState {
                sctlr: 0x30c50830,
                midr: 0x410fd083,
                cpuectlr: 64,
                mpidr: 0x80000100,
            },
        )
    }
    fn bit(name: &str) -> u64 {
        1 << FAILURE_NAMES.iter().position(|n| *n == name).unwrap()
    }
    #[test]
    fn valid_local_and_entry_state_can_acknowledge_and_park() {
        let (storage, state, entry) = fixture();
        let r = storage.get(7).unwrap();
        r.mailbox.enter(7).unwrap();
        let failures = validate(r, state, entry, state.vbar, true, true);
        assert_eq!(failures, Failures(0));
        publish_checkpoint(r, state, entry, failures);
        r.mailbox.acknowledge(7).unwrap();
        r.mailbox.park(7).unwrap();
        assert_eq!(r.failures.load(Ordering::Relaxed), 0);
        assert_eq!(r.entry.midr.load(Ordering::Relaxed), entry.midr);
    }
    #[test]
    fn each_register_mismatch_produces_its_specific_failure() {
        let (storage, s, e) = fixture();
        let r = storage.get(7).unwrap();
        let invalid = [
            (LocalState { current_el: 4, ..s }, "ExceptionLevel"),
            (
                LocalState {
                    affinity: 0x80000000,
                    ..s
                },
                "Affinity",
            ),
            (LocalState { sp: 0x810000, ..s }, "Stack"),
            (LocalState { tpidr: 0, ..s }, "ContextPointer"),
            (LocalState { vbar: 0, ..s }, "Vectors"),
            (LocalState { mair: 0, ..s }, "Mair"),
            (LocalState { tcr: 0, ..s }, "Tcr"),
            (LocalState { root: 0, ..s }, "Root"),
            (LocalState { sctlr: 0, ..s }, "Sctlr"),
            (
                LocalState {
                    daif: MASKED_DAIF & !(1 << 7),
                    ..s
                },
                "InterruptMasks",
            ),
            (
                LocalState {
                    hcr: HOST_HCR | 1,
                    ..s
                },
                "Hcr",
            ),
            (
                LocalState {
                    cptr: HOST_CPTR | (1 << 10),
                    ..s
                },
                "Cptr",
            ),
            (LocalState { hstr: 1, ..s }, "Hstr"),
            (LocalState { vttbr: 1, ..s }, "Vttbr"),
            (LocalState { cntvoff: 1, ..s }, "CounterOffset"),
            (LocalState { cnthctl: 0, ..s }, "CounterAccess"),
            (LocalState { cnthp_ctl: 1, ..s }, "HypTimer"),
            (LocalState { cntp_ctl: 1, ..s }, "PhysicalTimer"),
            (LocalState { cntv_ctl: 1, ..s }, "VirtualTimer"),
            (LocalState { spsel: 0, ..s }, "StackSelection"),
        ];
        for (state, name) in invalid {
            assert_eq!(
                validate(r, state, e, s.vbar, true, true),
                Failures(bit(name)),
                "{name}"
            );
        }
        for (entry, name) in [
            (
                EntryState {
                    sctlr: e.sctlr | 1,
                    ..e
                },
                "EntryRegime",
            ),
            (
                EntryState {
                    sctlr: e.sctlr | (1 << 2),
                    ..e
                },
                "EntryRegime",
            ),
            (
                EntryState {
                    sctlr: e.sctlr | (1 << 25),
                    ..e
                },
                "EntryRegime",
            ),
            (EntryState { midr: 0, ..e }, "CpuImplementation"),
            (EntryState { cpuectlr: 0, ..e }, "MaintenanceParticipation"),
            (EntryState { mpidr: 0, ..e }, "EntryAffinity"),
        ] {
            assert_eq!(
                validate(r, s, entry, s.vbar, true, true),
                Failures(bit(name)),
                "{name}"
            );
        }
        assert_eq!(
            validate(r, s, e, s.vbar, false, false),
            Failures(bit("ContextIdentity") | bit("MemoryMap"))
        );
    }
    #[test]
    fn stack_bounds_and_timer_status_are_handled_without_false_success() {
        let (storage, s, e) = fixture();
        let r = storage.get(7).unwrap();
        for sp in [r.stack_bottom - 16, r.stack_top, r.stack_bottom + 1] {
            assert_ne!(
                validate(r, LocalState { sp, ..s }, e, s.vbar, true, true).0 & bit("Stack"),
                0
            );
        }
        assert_eq!(
            validate(
                r,
                LocalState {
                    sp: r.stack_bottom,
                    cnthp_ctl: 4,
                    cntp_ctl: 4,
                    cntv_ctl: 4,
                    ..s
                },
                e,
                s.vbar,
                true,
                true
            ),
            Failures(0)
        );
        // A timer's read-only ISTATUS can be unknown while disabled; IMASK must still match policy.
        assert_eq!(
            validate(r, LocalState { cntp_ctl: 2, ..s }, e, s.vbar, true, true),
            Failures(bit("PhysicalTimer"))
        );
    }
    #[test]
    fn failure_snapshot_is_visible_and_late_failure_does_not_replace_timeout() {
        let (storage, state, entry) = fixture();
        let r = storage.get(7).unwrap();
        r.mailbox.finish(7, Outcome::TimedOut).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                r.mailbox.enter(7).unwrap();
                let bad = LocalState {
                    hcr: 0,
                    cnthctl: 0,
                    ..state
                };
                let failures = validate(r, bad, entry, state.vbar, true, true);
                publish_checkpoint(r, bad, entry, failures);
                r.mailbox.fail(7).unwrap();
            });
            while r.mailbox.state(7) != Ok(WorkerState::Failed) {
                std::thread::yield_now();
            }
            assert_eq!(
                r.failures.load(Ordering::Relaxed),
                bit("Hcr") | bit("CounterAccess")
            );
            assert_eq!(r.checkpoint.cnthctl.load(Ordering::Relaxed), 0);
            assert_eq!(r.entry.cpuectlr.load(Ordering::Relaxed), 64);
        });
        assert!(!r.mailbox.has_arrived(7));
        assert_eq!(r.mailbox.outcome(), Outcome::TimedOut);
        assert!(r.mailbox.enter(7).is_err());
    }
    #[test]
    fn matching_a_bad_record_does_not_bypass_named_register_policy() {
        let (_, state, entry) = fixture();
        let storage = RecordStorage::new();
        storage
            .publish(StartupRecord {
                logical_id: 1,
                affinity: 0x100,
                context: 0x420000,
                stack_bottom: 0x800000,
                stack_top: 0x810000,
                generation: 7,
                root: state.root,
                mair: 0,
                tcr: 0,
                sctlr: 0,
            })
            .unwrap();
        let r = storage.get(7).unwrap();
        assert_eq!(
            validate(
                r,
                LocalState {
                    mair: 0,
                    tcr: 0,
                    sctlr: 0,
                    ..state
                },
                entry,
                state.vbar,
                true,
                true
            ),
            Failures(bit("Mair") | bit("Tcr") | bit("Sctlr"))
        );
    }
}
