//! Physical-CPU layout and startup metadata shared with assembly and host tests.
use crate::{
    PAGE_SIZE,
    cpu_topology::MAX_CPUS,
    el2_stage1_translation::{Mapping, MemoryType, TableSet, TranslationError},
};
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

pub const STACK_SIZE: u64 = 64 * 1024;
pub const GUARD_SIZE: u64 = PAGE_SIZE as u64;
pub const STACK_STRIDE: u64 = GUARD_SIZE + STACK_SIZE;
pub const STACK_ARENA_SIZE: u64 = MAX_CPUS as u64 * STACK_STRIDE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackLayout {
    pub guard_start: u64,
    pub bottom: u64,
    pub top: u64,
}
pub fn stack_layout(base: u64, cpu: usize) -> Result<StackLayout, TranslationError> {
    if base & (PAGE_SIZE as u64 - 1) != 0 || cpu >= MAX_CPUS {
        return Err(TranslationError::InvalidateParameter);
    }
    let guard_start = base
        .checked_add(cpu as u64 * STACK_STRIDE)
        .ok_or(TranslationError::AddressOverflow)?;
    let bottom = guard_start
        .checked_add(GUARD_SIZE)
        .ok_or(TranslationError::AddressOverflow)?;
    let top = bottom
        .checked_add(STACK_SIZE)
        .ok_or(TranslationError::AddressOverflow)?;
    Ok(StackLayout {
        guard_start,
        bottom,
        top,
    })
}
pub fn map_stacks<const N: usize>(
    tables: &mut TableSet<'_, N>,
    base: u64,
) -> Result<(), TranslationError> {
    for cpu in 0..MAX_CPUS {
        let stack = stack_layout(base, cpu)?;
        tables.map(Mapping {
            va: stack.bottom,
            pa: stack.bottom,
            size: STACK_SIZE,
            memory_type: MemoryType::Normal,
            writable: true,
            executable: false,
        })?;
    }
    validate_stacks(tables, base)
}
pub fn validate_stacks<const N: usize>(
    tables: &TableSet<'_, N>,
    base: u64,
) -> Result<(), TranslationError> {
    for cpu in 0..MAX_CPUS {
        let stack = stack_layout(base, cpu)?;
        for address in (stack.guard_start..stack.bottom).step_by(PAGE_SIZE) {
            if tables.walk(address)?.is_some() {
                return Err(TranslationError::Unexpected);
            }
        }
        // Check every mapped stack page, including its last byte.
        for address in (stack.bottom..stack.top)
            .step_by(PAGE_SIZE)
            .chain(core::iter::once(stack.top - 1))
        {
            let mapping = tables.walk(address)?.ok_or(TranslationError::Unexpected)?;
            if mapping.pa != address
                || !mapping.writable
                || mapping.executable
                || mapping.memory_type != MemoryType::Normal
            {
                return Err(TranslationError::Unexpected);
            }
        }
    }
    Ok(())
}

/// Owner-only mutable world-switch context. Not Sync: other CPUs may not touch
/// host_sp/active_vcpu. Identity and bounds are immutable after preparation.
#[repr(C, align(64))]
pub struct PcpuContext<V> {
    pub host_sp: u64,
    pub active_vcpu: *mut V,
    pub logical_id: u64,
    pub affinity: u64,
    pub stack_bottom: u64,
    pub stack_top: u64,
    pub present: u64,
}
impl<V> PcpuContext<V> {
    pub const fn empty() -> Self {
        Self {
            host_sp: 0,
            active_vcpu: core::ptr::null_mut(),
            logical_id: 0,
            affinity: 0,
            stack_bottom: 0,
            stack_top: 0,
            present: 0,
        }
    }
}
const _: () = assert!(core::mem::size_of::<PcpuContext<()>>() == 64);
const _: () = assert!(core::mem::align_of::<PcpuContext<()>>() == 64);
const _: () = assert!(core::mem::offset_of!(PcpuContext<()>, host_sp) == 0);
const _: () = assert!(core::mem::offset_of!(PcpuContext<()>, active_vcpu) == 8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CpuLifecycle {
    Discovered,
    Prepared,
    ReleaseRequested,
    Entered,
    Online,
    Parked,
    Failed,
}

/// Cross-CPU protocol storage, separate from owner-only mutable CPU context.
/// 11.1 publishes Prepared only; requests/releases/acknowledgements are not yet
/// implemented. A later protocol must validate generation before accepting ack.
pub struct CpuStartupState {
    lifecycle: AtomicU8,
    pub generation: AtomicU64,
    pub request: AtomicU64,
    pub acknowledgement: AtomicU64,
}
impl CpuStartupState {
    pub const fn new() -> Self {
        Self {
            lifecycle: AtomicU8::new(CpuLifecycle::Discovered as u8),
            generation: AtomicU64::new(1),
            request: AtomicU64::new(0),
            acknowledgement: AtomicU64::new(0),
        }
    }
    /// The boot CPU must finish initializing immutable metadata before this.
    pub fn publish_prepared(&self) -> Result<(), u8> {
        self.lifecycle
            .compare_exchange(
                CpuLifecycle::Discovered as u8,
                CpuLifecycle::Prepared as u8,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .map(|_| ())
    }
    pub fn is_prepared(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) == CpuLifecycle::Prepared as u8
    }
    pub fn mailbox_idle(&self) -> bool {
        self.generation.load(Ordering::Acquire) == 1
            && self.request.load(Ordering::Acquire) == 0
            && self.acknowledgement.load(Ordering::Acquire) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::el2_stage1_translation::TableStorage;
    #[test]
    fn stack_arena_has_four_disjoint_guarded_abi_aligned_stacks() {
        assert_eq!(STACK_ARENA_SIZE, 272 * 1024);
        let base = 0x4100000;
        for cpu in 0..MAX_CPUS {
            let s = stack_layout(base, cpu).unwrap();
            assert_eq!(s.bottom - s.guard_start, 4096);
            assert_eq!(s.top - s.bottom, 65536);
            assert_eq!(s.top & 15, 0);
            if cpu + 1 < MAX_CPUS {
                assert_eq!(s.top, stack_layout(base, cpu + 1).unwrap().guard_start);
            } else {
                assert_eq!(s.top, base + STACK_ARENA_SIZE);
            }
        }
        assert!(stack_layout(base, MAX_CPUS).is_err());
        assert!(stack_layout(base + 1, 0).is_err());
        assert!(stack_layout(u64::MAX & !4095, 0).is_err());
    }
    #[test]
    fn stack_mapping_keeps_every_guard_unmapped_and_rejects_a_later_guard_mapping() {
        let mut storage = TableStorage::<16>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x8000000, 44).unwrap();
        // Force the arena across a 2 MiB boundary: root, L2 and two L3 tables.
        let base = 0x41f0000;
        map_stacks(&mut tables, base).unwrap();
        assert_eq!(tables.used_pages(), 4);
        let s = stack_layout(base, 2).unwrap();
        tables
            .map(Mapping {
                va: s.guard_start,
                pa: s.guard_start,
                size: GUARD_SIZE,
                memory_type: MemoryType::Normal,
                writable: true,
                executable: false,
            })
            .unwrap();
        assert!(validate_stacks(&tables, base).is_err());
    }
    #[test]
    fn insufficient_table_storage_fails_explicitly() {
        let mut storage = TableStorage::<2>::zeroed();
        let mut tables = TableSet::new(&mut storage, 0x8000000, 44).unwrap();
        assert_eq!(
            map_stacks(&mut tables, 0x4100000),
            Err(TranslationError::TableExhausted)
        );
    }
    #[test]
    fn context_starts_without_an_active_vcpu_and_startup_is_not_online() {
        let context = PcpuContext::<()>::empty();
        assert!(context.active_vcpu.is_null());
        assert_eq!(context.host_sp, 0);
        let state = CpuStartupState::new();
        assert!(!state.is_prepared());
        assert!(state.mailbox_idle());
        state.publish_prepared().unwrap();
        assert!(state.is_prepared());
        assert!(state.publish_prepared().is_err());
        assert_eq!(
            state.lifecycle.load(Ordering::Acquire),
            CpuLifecycle::Prepared as u8
        );
    }
}
