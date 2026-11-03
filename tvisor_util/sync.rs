//! IRQ-save resource locking. Do not nest these locks or format/allocate while
//! holding an allocator/table lock. A guard belongs to the acquiring CPU.
use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};
use spin::{Mutex, MutexGuard};

/// CPU-local IRQ state provider, also injectable for unprivileged host tests.
///
/// # Safety
/// Implementations must mask IRQ before returning from save_and_mask and restore
/// the saved state on the same CPU. Neither operation may allocate or acquire locks.
pub unsafe trait InterruptControl {
    /// A value snapshot of the local mask, not an owned resource.
    type State: Copy;
    fn save_and_mask() -> Self::State;
    fn restore(state: Self::State);
}
pub struct LocalIrq;
// SAFETY: only bare-metal AArch64 executes privileged instructions. Hosted
// tests use a no-op policy; mock policies test the restoration protocol.
unsafe impl InterruptControl for LocalIrq {
    type State = u64;
    fn save_and_mask() -> u64 {
        #[cfg(all(target_arch = "aarch64", target_os = "none"))]
        unsafe {
            let state;
            core::arch::asm!("mrs {}, daif", "msr daifset, #2", out(reg) state, options(nostack, preserves_flags));
            state
        }
        #[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
        {
            0
        }
    }
    fn restore(state: u64) {
        #[cfg(all(target_arch = "aarch64", target_os = "none"))]
        unsafe {
            // Restore only I: leave unrelated D/A/F changes untouched.
            if state & (1 << 7) == 0 {
                core::arch::asm!("msr daifclr, #2", options(nostack, preserves_flags));
            } else {
                core::arch::asm!("msr daifset, #2", options(nostack, preserves_flags));
            }
        }
        #[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
        let _ = state;
    }
}

/// Restore the acquiring CPU's IRQ mask when this scope ends. Callers may
/// temporarily change the mask while retaining the original state for Drop.
/// This owns no lock and changes no D/A/F bits with the LocalIrq policy.
///
/// Restoration must stay on the acquiring CPU, including on error/unwind:
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<tvisor_util::sync::RestoreIrq>();
/// ```
///
/// ```compile_fail
/// fn require_sync<T: Sync>() {}
/// require_sync::<tvisor_util::sync::RestoreIrq>();
/// ```
#[must_use = "dropping the guard immediately restores the previous IRQ mask"]
pub struct RestoreIrq<I: InterruptControl = LocalIrq> {
    saved: I::State,
    // An IRQ state belongs to the CPU that captured it, not another thread.
    local: PhantomData<*mut ()>,
}
impl<I: InterruptControl> RestoreIrq<I> {
    #[inline(always)]
    pub fn save_and_mask() -> Self {
        Self {
            saved: I::save_and_mask(),
            local: PhantomData,
        }
    }
}
impl<I: InterruptControl> Drop for RestoreIrq<I> {
    #[inline(always)]
    fn drop(&mut self) {
        I::restore(self.saved);
    }
}

pub struct IrqSpinLock<T, I: InterruptControl = LocalIrq> {
    inner: Mutex<T>,
    policy: PhantomData<fn() -> I>,
}
impl<T, I: InterruptControl> IrqSpinLock<T, I> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
            policy: PhantomData,
        }
    }
    pub fn lock(&self) -> IrqGuard<'_, T, I> {
        let irq = RestoreIrq::<I>::save_and_mask();
        IrqGuard {
            inner: Some(self.inner.lock()),
            _irq: irq,
        }
    }
    /// One bounded attempt; failure restores IRQ state too.
    pub fn try_lock(&self) -> Option<IrqGuard<'_, T, I>> {
        let irq = RestoreIrq::<I>::save_and_mask();
        let inner = self.inner.try_lock()?;
        Some(IrqGuard {
            inner: Some(inner),
            _irq: irq,
        })
    }
}
pub struct IrqGuard<'a, T, I: InterruptControl> {
    inner: Option<MutexGuard<'a, T>>,
    // Dropped automatically after Drop releases inner; also prevents Send/Sync.
    _irq: RestoreIrq<I>,
}
impl<T, I: InterruptControl> Deref for IrqGuard<'_, T, I> {
    type Target = T;
    fn deref(&self) -> &T {
        self.inner.as_ref().unwrap()
    }
}
impl<T, I: InterruptControl> DerefMut for IrqGuard<'_, T, I> {
    fn deref_mut(&mut self) -> &mut T {
        self.inner.as_mut().unwrap()
    }
}
impl<T, I: InterruptControl> Drop for IrqGuard<'_, T, I> {
    fn drop(&mut self) {
        drop(self.inner.take()); // Release before restoring interrupts.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    std::thread_local! { static IRQ_MASKED: Cell<bool> = const { Cell::new(false) }; }
    struct MockIrq;
    // SAFETY: thread-local state models the acquiring CPU's IRQ mask.
    unsafe impl InterruptControl for MockIrq {
        type State = bool;
        fn save_and_mask() -> bool {
            IRQ_MASKED.with(|v| v.replace(true))
        }
        fn restore(state: bool) {
            IRQ_MASKED.with(|v| v.set(state));
        }
    }
    #[test]
    fn scope_restores_original_mask_after_temporary_unmasking() {
        assert_eq!(
            core::mem::size_of::<RestoreIrq>(),
            core::mem::size_of::<u64>()
        );
        for initial in [false, true] {
            IRQ_MASKED.with(|v| v.set(initial));
            let restore = RestoreIrq::<MockIrq>::save_and_mask();
            assert!(IRQ_MASKED.with(Cell::get));
            MockIrq::restore(false);
            assert!(!IRQ_MASKED.with(Cell::get));
            drop(restore);
            assert_eq!(IRQ_MASKED.with(Cell::get), initial);
        }
    }
    #[test]
    fn scope_restores_on_early_error_and_unwind() {
        fn early_error() -> Result<(), ()> {
            let _restore = RestoreIrq::<MockIrq>::save_and_mask();
            MockIrq::restore(false);
            Err(())
        }
        for initial in [false, true] {
            IRQ_MASKED.with(|v| v.set(initial));
            assert_eq!(early_error(), Err(()));
            assert_eq!(IRQ_MASKED.with(Cell::get), initial);
            let result = std::panic::catch_unwind(|| {
                let _restore = RestoreIrq::<MockIrq>::save_and_mask();
                MockIrq::restore(false);
                panic!("test");
            });
            assert!(result.is_err());
            assert_eq!(IRQ_MASKED.with(Cell::get), initial);
        }
    }
    #[test]
    fn restores_enabled_and_disabled_state_and_failed_try_lock() {
        let lock = IrqSpinLock::<_, MockIrq>::new(0);
        for initial in [false, true] {
            IRQ_MASKED.with(|v| v.set(initial));
            let mut guard = lock.lock();
            assert!(IRQ_MASKED.with(Cell::get));
            assert!(lock.try_lock().is_none());
            assert!(IRQ_MASKED.with(Cell::get));
            *guard += 1;
            drop(guard);
            assert_eq!(IRQ_MASKED.with(Cell::get), initial);
            assert!(lock.try_lock().is_some());
            assert_eq!(IRQ_MASKED.with(Cell::get), initial);
        }
    }
    #[test]
    fn contended_try_lock_restores_unmasked_irq_state() {
        let lock = IrqSpinLock::<_, MockIrq>::new(());
        // Hold the underlying mutex without changing this simulated CPU's mask,
        // modelling another CPU's ownership.
        let _owner = lock.inner.lock();
        for initial in [false, true] {
            IRQ_MASKED.with(|v| v.set(initial));
            assert!(lock.try_lock().is_none());
            assert_eq!(IRQ_MASKED.with(Cell::get), initial);
        }
    }
    #[test]
    fn restores_state_on_unwind() {
        let lock = IrqSpinLock::<_, MockIrq>::new(());
        IRQ_MASKED.with(|v| v.set(false));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock.lock();
            panic!("test");
        }));
        assert!(!IRQ_MASKED.with(Cell::get));
        assert!(lock.try_lock().is_some());
    }
    #[test]
    fn unlock_precedes_irq_restoration() {
        struct OrderIrq;
        static LOCK: IrqSpinLock<(), OrderIrq> = IrqSpinLock::new(());
        // SAFETY: host-only policy observes release order without hardware IRQs.
        unsafe impl InterruptControl for OrderIrq {
            type State = ();
            fn save_and_mask() {}
            fn restore(_: ()) {
                assert!(LOCK.inner.try_lock().is_some());
            }
        }
        drop(LOCK.lock());
    }
    #[test]
    fn contended_updates_are_exclusive_and_published() {
        let lock = IrqSpinLock::<_, MockIrq>::new(0usize);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let lock = &lock;
                scope.spawn(move || {
                    for _ in 0..10_000 {
                        *lock.lock() += 1;
                    }
                    assert!(!IRQ_MASKED.with(Cell::get));
                });
            }
        });
        assert_eq!(*lock.lock(), 40_000);
    }
    #[test]
    fn physical_pages_have_unique_live_owners_under_contention() {
        use crate::{
            PAGE_SIZE,
            page_allocator::{PageAllocator, PageBitmap},
            system_info::{FixedList, PhysAddr, PhysRegion},
        };
        use core::sync::atomic::{AtomicBool, Ordering};
        let mut managed = PageBitmap::<16>::zeroed();
        let mut used = PageBitmap::<16>::zeroed();
        let mut regions = FixedList::<_, 1>::new();
        regions
            .push(PhysRegion::new(PhysAddr::new(0), (128 * PAGE_SIZE) as u64).unwrap())
            .unwrap();
        let allocator = IrqSpinLock::<_, MockIrq>::new(
            PageAllocator::new(&regions, &regions, &regions, &mut managed, &mut used).unwrap(),
        );
        let owners: [AtomicBool; 128] = [const { AtomicBool::new(false) }; 128];
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let allocator = &allocator;
                let owners = &owners;
                scope.spawn(move || {
                    for _ in 0..2000 {
                        let page = allocator.lock().allocate_contiguous(2).unwrap();
                        let index = page.value() as usize / PAGE_SIZE;
                        for owner in &owners[index..index + 2] {
                            assert!(!owner.swap(true, Ordering::SeqCst), "duplicate live page");
                        }
                        std::thread::yield_now();
                        for owner in &owners[index..index + 2] {
                            assert!(owner.swap(false, Ordering::SeqCst));
                        }
                        let mut guard = allocator.lock();
                        guard.free(page).unwrap();
                        guard
                            .free(page.checked_add(PAGE_SIZE as u64).unwrap())
                            .unwrap();
                        let stats = guard.stats();
                        assert_eq!(stats.in_use_pages + stats.unused_pages, 128);
                    }
                });
            }
        });
        let stats = allocator.lock().stats();
        assert_eq!(stats.in_use_pages, 0);
        assert_eq!(stats.unused_pages, 128);
    }
}
