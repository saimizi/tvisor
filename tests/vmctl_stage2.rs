extern crate alloc;

mod mm {
    use std::{
        alloc::{Layout, alloc_zeroed, dealloc},
        cell::RefCell,
        collections::BTreeMap,
        ptr::NonNull,
    };
    use tvisor_util::PAGE_SIZE;
    use tvisor_util::page_allocator::AllocatorError;
    use tvisor_util::system_info::PhysAddr;

    // Per-test-thread physical memory. Contiguous allocations are freed only
    // after VmCtl returns every constituent page; never deallocate an interior
    // pointer as if it were an independent host allocation.
    struct Allocation {
        pointer: NonNull<u8>,
        layout: Layout,
        live: Vec<bool>,
        remaining: usize,
    }

    impl Drop for Allocation {
        fn drop(&mut self) {
            // SAFETY: this owns the original pointer and layout. Removing the
            // allocation after its last page is returned releases it once.
            unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
        }
    }

    thread_local! {
        static ALLOCATIONS: RefCell<BTreeMap<usize, Allocation>> = const {
            RefCell::new(BTreeMap::new())
        };
    }

    fn allocate_zeroed_pages(pages: usize) -> Result<PhysAddr, AllocatorError> {
        let size = pages
            .checked_mul(PAGE_SIZE)
            .filter(|size| *size != 0)
            .ok_or(AllocatorError::AddressOverflow)?;
        let layout = Layout::from_size_align(size, PAGE_SIZE)
            .map_err(|_| AllocatorError::AddressOverflow)?;
        // SAFETY: the checked layout is nonempty and page-aligned.
        let pointer =
            NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or(AllocatorError::Exhausted)?;
        let base = pointer.as_ptr() as usize;
        ALLOCATIONS.with_borrow_mut(|allocations| {
            assert!(
                allocations
                    .insert(
                        base,
                        Allocation {
                            pointer,
                            layout,
                            live: vec![true; pages],
                            remaining: pages,
                        },
                    )
                    .is_none()
            );
        });
        Ok(PhysAddr::new(base as u64))
    }

    pub fn allocate_page() -> Result<PhysAddr, AllocatorError> {
        allocate_zeroed_pages(1)
    }

    pub fn allocate_contiguous_pages(pages: usize) -> Result<PhysAddr, AllocatorError> {
        allocate_zeroed_pages(pages)
    }

    pub fn free_page(page: PhysAddr) -> Result<(), AllocatorError> {
        let address = page.value() as usize;
        assert_eq!(address % PAGE_SIZE, 0, "unaligned page returned by VmCtl");
        ALLOCATIONS.with_borrow_mut(|allocations| {
            let (&base, allocation) = allocations
                .range_mut(..=address)
                .next_back()
                .expect("VmCtl freed a page it does not own");
            let index = (address - base) / PAGE_SIZE;
            let live = allocation
                .live
                .get_mut(index)
                .expect("VmCtl freed a page outside its allocation");
            assert!(*live, "VmCtl freed the same page twice");
            *live = false;
            allocation.remaining -= 1;
            if allocation.remaining == 0 {
                allocations.remove(&base);
            }
        });
        Ok(())
    }

    pub fn live_pages() -> usize {
        ALLOCATIONS.with_borrow(|allocations| allocations.values().map(|a| a.remaining).sum())
    }
}

// Use the production vCPU data/layout, not a dummy type. Privileged execution
// and Linux launch are compiled only for the bare-metal target.
#[path = "../src/vcpu.rs"]
#[allow(dead_code)]
mod vcpu;

#[path = "../src/vmctl.rs"]
#[allow(dead_code)]
mod vmctl;

#[test]
fn vmctl_release_and_drop_return_owned_pages() {
    use tvisor_util::PAGE_SIZE;
    use vmctl::{IpaAddr, VmCtl, VmMemUsage};

    assert_eq!(mm::live_pages(), 0);
    {
        let mut vm = VmCtl::new(1);
        vm.vm_mem_alloc(VmMemUsage::GuestStage2PageTable, None, PAGE_SIZE)
            .unwrap();
        vm.vm_mem_alloc(
            VmMemUsage::GuestRam,
            Some(IpaAddr::new(0x4000_0000)),
            2 * PAGE_SIZE,
        )
        .unwrap();
        vm.vm_mem_alloc(
            VmMemUsage::Guard,
            Some(IpaAddr::new(0x4000_2000)),
            PAGE_SIZE,
        )
        .unwrap();
        assert_eq!(mm::live_pages(), 3);
        vm.release_all();
        assert_eq!(mm::live_pages(), 0);

        // Reuse the VM after release and let Drop exercise the same ownership
        // contract. A second release of old pages would fail in the adapter.
        vm.vm_mem_alloc(
            VmMemUsage::GuestRam,
            Some(IpaAddr::new(0x4000_0000)),
            2 * PAGE_SIZE,
        )
        .unwrap();
        assert_eq!(mm::live_pages(), 2);
    }
    assert_eq!(mm::live_pages(), 0);
}
