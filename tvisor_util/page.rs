pub const PAGE_SIZE: usize = 0x1000;
pub const fn is_page_aligned(addr: usize) -> bool {
    is_aligned(addr, PAGE_SIZE)
}

pub const fn is_aligned(addr: usize, alignment: usize) -> bool {
    alignment.is_power_of_two() && addr & (alignment - 1) == 0
}

pub const fn align_down(value: usize, alignment: usize) -> Option<usize> {
    if !alignment.is_power_of_two() {
        return None;
    }
    Some(value & !(alignment - 1))
}

pub const fn align_up(value: usize, alignment: usize) -> Option<usize> {
    if !alignment.is_power_of_two() {
        return None;
    }
    match value.checked_add(alignment - 1) {
        Some(v) => Some(v & !(alignment - 1)),
        None => None,
    }
}

pub const fn page_offset(addr: usize) -> usize {
    addr & (PAGE_SIZE - 1)
}

pub const fn page_address(addr: usize) -> usize {
    addr & !(PAGE_SIZE - 1)
}

pub const fn round_up_to_page(value: usize) -> usize {
    if is_page_aligned(value) {
        value / PAGE_SIZE
    } else {
        value / PAGE_SIZE + 1
    }
}
