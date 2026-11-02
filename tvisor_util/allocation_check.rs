//! Independent validation of published, live test allocations. Callers must
//! hold a rendezvous: no participant may change a range until validation ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub bytes: u64,
    pub alignment: u64,
}
impl Range {
    pub const EMPTY: Self = Self {
        start: 0,
        bytes: 0,
        alignment: 1,
    };
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidRange,
    Overlap,
}
pub fn validate(ranges: &[Range]) -> Result<(), Error> {
    for (i, r) in ranges.iter().enumerate() {
        let end = r.start.checked_add(r.bytes).ok_or(Error::InvalidRange)?;
        if r.start == 0
            || r.bytes == 0
            || !r.alignment.is_power_of_two()
            || r.start % r.alignment != 0
        {
            return Err(Error::InvalidRange);
        }
        for other in &ranges[..i] {
            // Earlier ranges have already passed overflow validation.
            if r.start < other.start + other.bytes && other.start < end {
                return Err(Error::Overlap);
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn r(start: u64, bytes: u64) -> Range {
        Range {
            start,
            bytes,
            alignment: 64,
        }
    }
    #[test]
    fn accepts_adjacency_and_unordered_disjoint_ranges() {
        assert_eq!(
            validate(&[r(8192, 4096), r(4096, 4096), r(128, 129)]),
            Ok(())
        );
    }
    #[test]
    fn rejects_duplicate_nested_and_partial_overlap() {
        for other in [r(4096, 4096), r(4160, 64), r(8128, 128)] {
            assert_eq!(validate(&[r(4096, 4096), other]), Err(Error::Overlap));
        }
    }
    #[test]
    fn rejects_overflow_null_empty_and_bad_alignment() {
        for range in [
            r(u64::MAX - 63, 128),
            r(0, 64),
            r(64, 0),
            r(65, 64),
            Range {
                start: 64,
                bytes: 64,
                alignment: 3,
            },
        ] {
            assert_eq!(validate(&[range]), Err(Error::InvalidRange));
        }
    }
}
