//! In-place, boot-only construction followed by immutable publication.
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct BootPublished<T> {
    ready: AtomicBool,
    value: UnsafeCell<T>,
}
// SAFETY: initialization is restricted by `initialize`'s boot-only contract;
// once published, shared access is safe only when T is Sync.
unsafe impl<T: Sync> Sync for BootPublished<T> {}
impl<T> BootPublished<T> {
    pub const fn new(value: T) -> Self {
        Self {
            ready: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }
    pub fn get(&self) -> Option<&T> {
        if !self.ready.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: publication follows the last mutable access, and the boot
        // initialization contract forbids further writes.
        Some(unsafe { &*self.value.get() })
    }
    /// Initialize the existing static object in place and publish on success.
    /// The closure cannot return a reference into the mutable object.
    ///
    /// # Safety
    /// Call at most once, on the boot CPU, with no concurrent initialization.
    /// Concurrent readers may only call `get`; they cannot access the object
    /// before publication. No secondary CPU may access this storage
    /// until the platform's shared-memory/coherency contract is established.
    pub unsafe fn initialize<E>(
        &self,
        init: impl FnOnce(&mut T) -> Result<(), E>,
    ) -> Result<&T, E> {
        // SAFETY: the caller guarantees exclusive boot-time initialization.
        init(unsafe { &mut *self.value.get() })?;
        self.ready.store(true, Ordering::Release);
        // SAFETY: the mutable borrow ended before publication; no writes follow.
        Ok(unsafe { &*self.value.get() })
    }
}

#[cfg(test)]
mod tests {
    use super::BootPublished;
    #[test]
    fn failed_initialization_is_not_published() {
        let slot = BootPublished::new([0u64; 4]);
        assert!(slot.get().is_none());
        // SAFETY: this is the only initialization attempt and no readers exist.
        assert!(
            unsafe {
                slot.initialize(|v| {
                    v[0] = 1;
                    Err::<(), _>(())
                })
            }
            .is_err()
        );
        assert!(slot.get().is_none());
    }
    #[test]
    fn publishes_initialized_data_in_place_to_concurrent_readers() {
        let slot = BootPublished::new([0u64; 1024]);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let slot = &slot;
                scope.spawn(move || {
                    while slot.get().is_none() {
                        std::thread::yield_now();
                    }
                    let data = slot.get().unwrap();
                    assert!(
                        data.iter()
                            .enumerate()
                            .all(|(i, value)| *value == i as u64 + 1)
                    );
                });
            }
            // SAFETY: this is the single writer; readers only call get, which
            // returns no reference until publication. Host memory is coherent.
            let value = unsafe {
                slot.initialize::<()>(|data| {
                    for (i, v) in data.iter_mut().enumerate() {
                        *v = i as u64 + 1;
                    }
                    Ok(())
                })
            }
            .unwrap();
            assert_eq!(value.as_ptr(), slot.value.get().cast::<u64>());
        });
    }
}
