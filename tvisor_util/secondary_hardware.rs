//! Direct publication primitives. PA zero is accessed only by assembly.
//! Hosted fault injection is compiled exclusively for unit tests.
use crate::startup_publication::PublicationError;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
core::arch::global_asm!(include_str!("../src/secondary_publication.S"));
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
unsafe extern "C" {
    fn __secondary_clean_line(address: u64);
    fn __secondary_publication_barrier();
    fn __secondary_read_cell(address: u64) -> u64;
    fn __secondary_store_cell(address: u64, value: u64) -> u64;
    fn __secondary_send_event();
}
// Each operation requires the boot CPU, a validated frozen identity regime,
// and retained resources. Keep the five calls visible in the release function.
/// # Safety
/// Caller must hold the validated frozen boot-only identity regime.
pub unsafe fn clean_cache_line(address: u64) -> Result<(), PublicationError> {
    #[cfg(test)]
    {
        return test_io::execute(test_io::Event::Clean(address)).map(|_| ());
    }
    #[cfg(all(not(test), target_arch = "aarch64", target_os = "none"))]
    {
        unsafe { __secondary_clean_line(address) };
        Ok(())
    }
    #[cfg(all(not(test), not(all(target_arch = "aarch64", target_os = "none"))))]
    {
        let _ = address;
        Err(PublicationError::Io)
    }
}
/// # Safety
/// Only CPU0 in the validated startup publication/release procedure.
pub unsafe fn publication_barrier() -> Result<(), PublicationError> {
    #[cfg(test)]
    {
        return test_io::execute(test_io::Event::Barrier).map(|_| ());
    }
    #[cfg(all(not(test), target_arch = "aarch64", target_os = "none"))]
    {
        unsafe { __secondary_publication_barrier() };
        Ok(())
    }
    #[cfg(all(not(test), not(all(target_arch = "aarch64", target_os = "none"))))]
    {
        Err(PublicationError::Io)
    }
}
/// # Safety
/// Address must be an aligned, owned, identity-mapped firmware cell (PA zero allowed).
pub unsafe fn read_release_cell(address: u64) -> Result<u64, PublicationError> {
    #[cfg(test)]
    {
        return test_io::execute(test_io::Event::Read(address)).map(u64::from_le);
    }
    #[cfg(all(not(test), target_arch = "aarch64", target_os = "none"))]
    {
        Ok(u64::from_le(unsafe { __secondary_read_cell(address) }))
    }
    #[cfg(all(not(test), not(all(target_arch = "aarch64", target_os = "none"))))]
    {
        let _ = address;
        Err(PublicationError::Io)
    }
}
// The assembly timestamp precedes the STR. Convert once at the boundary.
/// # Safety
/// Address must be the validated target cell and value the checked physical entry.
pub unsafe fn write_release_cell_le64(address: u64, value: u64) -> Result<u64, PublicationError> {
    #[cfg(test)]
    {
        return test_io::execute(test_io::Event::Store(address, value.to_le()));
    }
    #[cfg(all(not(test), target_arch = "aarch64", target_os = "none"))]
    {
        Ok(unsafe { __secondary_store_cell(address, value.to_le()) })
    }
    #[cfg(all(not(test), not(all(target_arch = "aarch64", target_os = "none"))))]
    {
        let _ = (address, value);
        Err(PublicationError::Io)
    }
}
/// # Safety
/// Caller must have completed publication barriers before the doorbell.
pub unsafe fn send_event() -> Result<(), PublicationError> {
    #[cfg(test)]
    {
        return test_io::execute(test_io::Event::Sev).map(|_| ());
    }
    #[cfg(all(not(test), target_arch = "aarch64", target_os = "none"))]
    {
        unsafe { __secondary_send_event() };
        Ok(())
    }
    #[cfg(all(not(test), not(all(target_arch = "aarch64", target_os = "none"))))]
    {
        Err(PublicationError::Io)
    }
}
#[cfg(test)]
pub(crate) mod test_io {
    use super::*;
    use std::{cell::RefCell, collections::BTreeMap, vec::Vec};
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Event {
        Clean(u64),
        Barrier,
        Read(u64),
        Store(u64, u64),
        Sev,
    }
    #[derive(Default)]
    pub struct Hardware {
        pub events: Vec<Event>,
        pub cells: BTreeMap<u64, u64>,
        pub fail_at: Option<usize>,
        pub nonzero_at_read: Option<usize>,
    }
    std::thread_local! {pub static IO:RefCell<Hardware>=RefCell::new(Hardware::default());}
    pub fn reset() {
        IO.with(|io| *io.borrow_mut() = Hardware::default());
    }
    pub fn execute(event: Event) -> Result<u64, PublicationError> {
        IO.with(|io| {
            let mut h = io.borrow_mut();
            h.events.push(event.clone());
            let result = match event {
                Event::Read(a) => {
                    if h.nonzero_at_read == Some(h.events.len()) {
                        1
                    } else {
                        *h.cells.get(&a).unwrap_or(&0)
                    }
                }
                Event::Store(a, v) => {
                    h.cells.insert(a, v);
                    123
                }
                _ => 0,
            };
            // Store errors are deliberately ambiguous: the write has happened.
            if h.fail_at == Some(h.events.len()) {
                Err(PublicationError::Io)
            } else {
                Ok(result)
            }
        })
    }
}
