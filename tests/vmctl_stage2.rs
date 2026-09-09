extern crate alloc;

use std::alloc::{Layout, alloc_zeroed};

mod mm {
    use super::*;
    use tvisor_util::PAGE_SIZE;
    use tvisor_util::page_allocator::AllocatorError;
    use tvisor_util::system_info::PhysAddr;

    fn allocate_zeroed_pages(pages: usize) -> Result<PhysAddr, AllocatorError> {
        let layout = Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE)
            .map_err(|_| AllocatorError::AddressOverflow)?;
        // SAFETY: the layout has non-zero size and page alignment. Test
        // allocations intentionally live until process exit.
        let page = unsafe { alloc_zeroed(layout) };
        if page.is_null() {
            return Err(AllocatorError::Exhausted);
        }
        Ok(PhysAddr::new(page as u64))
    }

    pub fn allocate_page() -> Result<PhysAddr, AllocatorError> {
        allocate_zeroed_pages(1)
    }

    pub fn allocate_contiguous_pages(pages: usize) -> Result<PhysAddr, AllocatorError> {
        allocate_zeroed_pages(pages)
    }

    pub fn free_page(_page: PhysAddr) -> Result<(), AllocatorError> {
        Ok(())
    }
}

#[path = "../src/vmctl.rs"]
#[allow(dead_code)]
mod vmctl;
