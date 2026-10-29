//! Allocation-free first-failure evidence. Snapshot readers never wait for a
//! faulting writer; an interrupted publication simply has no ready snapshot.
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub kind: u64,
    pub syndrome: u64,
    pub pc: u64,
    pub address: u64,
}
pub struct FailureRecord {
    state: AtomicU8,
    kind: AtomicU64,
    syndrome: AtomicU64,
    pc: AtomicU64,
    address: AtomicU64,
}
impl FailureRecord {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            kind: AtomicU64::new(0),
            syndrome: AtomicU64::new(0),
            pc: AtomicU64::new(0),
            address: AtomicU64::new(0),
        }
    }
    pub fn record(&self, failure: Failure) {
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        self.kind.store(failure.kind, Ordering::Relaxed);
        self.syndrome.store(failure.syndrome, Ordering::Relaxed);
        self.pc.store(failure.pc, Ordering::Relaxed);
        self.address.store(failure.address, Ordering::Relaxed);
        self.state.store(2, Ordering::Release);
    }
    pub fn snapshot(&self) -> Option<Failure> {
        if self.state.load(Ordering::Acquire) != 2 {
            return None;
        }
        Some(Failure {
            kind: self.kind.load(Ordering::Relaxed),
            syndrome: self.syndrome.load(Ordering::Relaxed),
            pc: self.pc.load(Ordering::Relaxed),
            address: self.address.load(Ordering::Relaxed),
        })
    }
}
pub struct FatalState<const N: usize> {
    pub fatal: AtomicBool,
    pub records: [FailureRecord; N],
}
impl<const N: usize> FatalState<N> {
    pub const fn new() -> Self {
        Self {
            fatal: AtomicBool::new(false),
            records: [const { FailureRecord::new() }; N],
        }
    }
    pub fn record(&self, cpu: usize, failure: Failure) {
        self.fatal.store(true, Ordering::Release);
        if let Some(record) = self.records.get(cpu) {
            record.record(failure);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_first_record_per_cpu_without_waiting() {
        let fatal = FatalState::<4>::new();
        assert!(fatal.records[0].snapshot().is_none());
        std::thread::scope(|scope| {
            for cpu in 0..4 {
                let fatal = &fatal;
                scope.spawn(move || {
                    fatal.record(
                        cpu,
                        Failure {
                            kind: 1,
                            syndrome: cpu as u64,
                            pc: 123,
                            address: 456,
                        },
                    );
                    fatal.record(
                        cpu,
                        Failure {
                            kind: 2,
                            syndrome: 0,
                            pc: 0,
                            address: 0,
                        },
                    );
                });
            }
        });
        assert!(fatal.fatal.load(Ordering::Acquire));
        for cpu in 0..4 {
            assert_eq!(
                fatal.records[cpu].snapshot(),
                Some(Failure {
                    kind: 1,
                    syndrome: cpu as u64,
                    pc: 123,
                    address: 456
                })
            );
        }
    }
}
