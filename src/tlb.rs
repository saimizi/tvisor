//! Shared lifecycle/update gate and responsive worker parking.
use tvisor_util::{
    management::{Operation, Reply},
    sync::{InterruptControl, LocalIrq},
    tlb_update::UpdateState,
};
pub static UPDATES: UpdateState = UpdateState::new();

#[cfg(any(feature = "tlb-update-test", feature = "tlb-update-timeout-test"))]
#[path = "tlb_test.rs"]
mod acceptance;
#[cfg(feature = "tlb-update-test")]
pub use acceptance::before_guest;
#[cfg(feature = "tlb-update-timeout-test")]
pub use acceptance::timeout_acceptance;
#[cfg(any(feature = "tlb-update-test", feature = "tlb-update-timeout-test"))]
pub use acceptance::{owns, prepare, ready_to_write, worker};

struct RestoreIrq(u64);
impl Drop for RestoreIrq {
    fn drop(&mut self) {
        LocalIrq::restore(self.0);
    }
}

pub fn park_workers() -> Result<(), &'static str> {
    let required = crate::secondary::required_mask();
    let _restore = RestoreIrq(LocalIrq::save_and_mask());
    LocalIrq::restore(0);
    for id in 1..tvisor_util::cpu_topology::MAX_CPUS {
        if required & (1 << id) == 0 {
            continue;
        }
        let mut request = crate::management::request(id, Operation::Park, 0)?;
        if crate::management::wait(&mut request)?.failed {
            return Err("worker park rejected");
        }
        if crate::pcpu::PCPU_SLOTS[id].startup.mailbox.lifecycle()
            != tvisor_util::secondary_mailbox::CpuState::Parked
        {
            return Err("worker not Parked");
        }
        let mut ping = crate::management::request(id, Operation::Ping, 0x1160 + id as u64)?;
        if crate::management::wait(&mut ping)?
            != (Reply {
                value: 0x1160 + id as u64,
                failed: false,
            })
        {
            return Err("parked worker unresponsive");
        }
        tvisor_util::diagnostic_println!(
            "Phase 11.6 CPU{} Parked responsive; retained in mapping membership",
            id
        );
    }
    if !UPDATES.try_membership_change() {
        return Err("SMP gate lifecycle snapshot busy");
    }
    let (online, parked) = crate::management::physical_smp_masks();
    UPDATES.finish_membership_change();
    if online != required | 1 || parked != required || crate::pcpu::has_fatal() {
        return Err("SMP gate observed incomplete membership");
    }
    tvisor_util::diagnostic_println!(
        "Phase 11.6 physical SMP gate online_mask={:#x} parked_mask={:#x} mapping_epoch={}",
        online,
        parked,
        UPDATES.epoch()
    );
    Ok(())
}
