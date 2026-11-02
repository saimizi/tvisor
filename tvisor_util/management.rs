//! One bounded, allocation-free request channel for a (sender, receiver) pair.
//! SGIs are doorbells and may coalesce. Publication and acknowledgement use
//! sequence numbers; a timeout never cancels a request or releases its storage.
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Operation {
    Ping = 1,
    WorkerTest = 2,
    Park = 3,
    FatalStop = 4,
    RunTest = 5,
    TlbQuiesce = 6,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Busy,
    Exhausted,
    Completed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub value: u64,
    pub failed: bool,
}

#[repr(C, align(64))]
pub struct Channel {
    writer: AtomicBool,
    sequence: AtomicU64,
    retired: AtomicU64,
    taken: AtomicU64,
    acknowledged: AtomicU64,
    operation: AtomicU64,
    argument: AtomicU64,
    result: AtomicU64,
    failed: AtomicBool,
}
impl Channel {
    pub const fn new() -> Self {
        Self {
            writer: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            retired: AtomicU64::new(0),
            taken: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
            operation: AtomicU64::new(0),
            argument: AtomicU64::new(0),
            result: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        }
    }
    /// One nonblocking attempt. The caller must ring the target SGI only after
    /// this returns. A dropped Pending deliberately prevents channel reuse.
    pub fn submit(&self, operation: Operation, argument: u64) -> Result<Pending<'_>, Error> {
        self.writer
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| Error::Busy)?;
        let old = self.sequence.load(Ordering::Relaxed);
        let result = if self.retired.load(Ordering::Acquire) != old {
            Err(Error::Busy)
        } else if let Some(sequence) = old.checked_add(1) {
            self.operation.store(operation as u64, Ordering::Relaxed);
            self.argument.store(argument, Ordering::Relaxed);
            self.sequence.store(sequence, Ordering::Release);
            Ok(Pending {
                channel: self,
                sequence,
                completed: false,
            })
        } else {
            Err(Error::Exhausted)
        };
        self.writer.store(false, Ordering::Release);
        result
    }
    pub fn completed_count(&self) -> u64 {
        self.acknowledged.load(Ordering::Acquire)
    }
    /// Called by the target's non-reentrant IRQ dispatcher. CAS also makes
    /// duplicate/spurious doorbells harmless and prevents duplicate execution.
    pub fn take(&self) -> Option<Request<'_>> {
        let sequence = self.sequence.load(Ordering::Acquire);
        let taken = self.taken.load(Ordering::Relaxed);
        if sequence == taken
            || self
                .taken
                .compare_exchange(taken, sequence, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return None;
        }
        let operation = match self.operation.load(Ordering::Relaxed) {
            1 => Operation::Ping,
            2 => Operation::WorkerTest,
            3 => Operation::Park,
            4 => Operation::FatalStop,
            5 => Operation::RunTest,
            6 => Operation::TlbQuiesce,
            _ => unreachable!(),
        };
        Some(Request {
            channel: self,
            sequence,
            operation,
            argument: self.argument.load(Ordering::Relaxed),
        })
    }
}
impl Default for Channel {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Pending<'a> {
    channel: &'a Channel,
    sequence: u64,
    completed: bool,
}
impl Pending<'_> {
    /// A failed or timed-out observation has no effect. Only receiving the
    /// matching completed reply retires the storage for the next request.
    pub fn try_result(&mut self) -> Result<Option<Reply>, Error> {
        if self.completed {
            return Err(Error::Completed);
        }
        if self.channel.acknowledged.load(Ordering::Acquire) != self.sequence {
            return Ok(None);
        }
        let reply = Reply {
            value: self.channel.result.load(Ordering::Relaxed),
            failed: self.channel.failed.load(Ordering::Relaxed),
        };
        self.completed = true;
        self.channel.retired.store(self.sequence, Ordering::Release);
        Ok(Some(reply))
    }
}
pub struct Request<'a> {
    channel: &'a Channel,
    sequence: u64,
    pub operation: Operation,
    pub argument: u64,
}
impl Request<'_> {
    /// The physical interrupt must be fully completed before acknowledging the
    /// logical request. Dropping this request leaves it unacknowledged forever.
    pub fn acknowledge(self, reply: Reply) {
        self.channel.result.store(reply.value, Ordering::Relaxed);
        self.channel.failed.store(reply.failed, Ordering::Relaxed);
        self.channel
            .acknowledged
            .store(self.sequence, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_doorbells_and_reply_ownership() {
        let c = Channel::new();
        assert!(c.take().is_none());
        let mut p = c.submit(Operation::Ping, u64::MAX).unwrap();
        assert_eq!(p.try_result(), Ok(None));
        assert!(matches!(c.submit(Operation::Park, 0), Err(Error::Busy)));
        let r = c.take().unwrap();
        assert_eq!(r.operation, Operation::Ping);
        assert_eq!(r.argument, u64::MAX);
        assert!(c.take().is_none());
        assert_eq!(p.try_result(), Ok(None));
        r.acknowledge(Reply {
            value: 99,
            failed: false,
        });
        assert!(matches!(c.submit(Operation::Park, 0), Err(Error::Busy)));
        assert_eq!(
            p.try_result(),
            Ok(Some(Reply {
                value: 99,
                failed: false
            }))
        );
        assert_eq!(p.try_result(), Err(Error::Completed));
        let mut p = c.submit(Operation::WorkerTest, 4).unwrap();
        c.take().unwrap().acknowledge(Reply {
            value: 7,
            failed: true,
        });
        assert_eq!(
            p.try_result(),
            Ok(Some(Reply {
                value: 7,
                failed: true
            }))
        );
    }
    #[test]
    fn timeout_or_dropped_request_never_cancels_or_reuses_storage() {
        let c = Channel::new();
        let mut p = c.submit(Operation::Park, 1).unwrap();
        for _ in 0..100 {
            assert_eq!(p.try_result(), Ok(None));
        }
        let r = c.take().unwrap();
        r.acknowledge(Reply {
            value: 1,
            failed: false,
        });
        assert!(p.try_result().unwrap().is_some());
        let _p = c.submit(Operation::FatalStop, 2).unwrap();
        drop(c.take());
        assert!(c.take().is_none());
        assert!(matches!(c.submit(Operation::Ping, 3), Err(Error::Busy)));
    }
    #[test]
    fn channels_are_independent_and_do_not_wrap() {
        let a = Channel::new();
        let b = Channel::new();
        let mut p = a.submit(Operation::Ping, 1).unwrap();
        assert!(b.take().is_none());
        let _q = b.submit(Operation::Park, 2).unwrap();
        a.take().unwrap().acknowledge(Reply {
            value: 1,
            failed: false,
        });
        assert!(p.try_result().unwrap().is_some());
        assert_eq!(b.take().unwrap().operation, Operation::Park);
        let c = Channel::new();
        c.sequence.store(u64::MAX, Ordering::Relaxed);
        c.retired.store(u64::MAX, Ordering::Relaxed);
        assert!(matches!(
            c.submit(Operation::Ping, 0),
            Err(Error::Exhausted)
        ));
    }
    #[test]
    fn cross_thread_publication_and_acknowledgement() {
        let c = Channel::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                for i in 1..=1000 {
                    let r = loop {
                        if let Some(r) = c.take() {
                            break r;
                        }
                        core::hint::spin_loop();
                    };
                    assert_eq!(r.argument, i);
                    r.acknowledge(Reply {
                        value: i ^ 0x1234,
                        failed: false,
                    });
                }
            });
            for i in 1..=1000 {
                let mut p = c.submit(Operation::Ping, i).unwrap();
                loop {
                    if let Some(r) = p.try_result().unwrap() {
                        assert_eq!(r.value, i ^ 0x1234);
                        break;
                    }
                    core::hint::spin_loop();
                }
            }
        });
    }
}
