use core::fmt;

const MAX_ARGS: usize = 16;
const MAX_ARG_LEN: usize = 64;

#[repr(u64)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FaultTest {
    #[default]
    None = 0,
    Sync = 1,
    Guard = 2,
    Unmapped = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootModeError {
    InvalidArgCount,
    NullArgv,
    NullArgument,
    UnterminatedArgument,
    DuplicateFault,
    UnknownFault,
}

impl fmt::Display for BootModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

pub fn parse_fault_test<'a>(
    arguments: impl IntoIterator<Item = &'a [u8]>,
) -> Result<FaultTest, BootModeError> {
    let mut fault_test = None;
    for argument in arguments {
        if let Some(value) = argument.strip_prefix(b"fault=") {
            if fault_test.is_some() {
                return Err(BootModeError::DuplicateFault);
            }
            fault_test = Some(match value {
                b"none" => FaultTest::None,
                b"sync" => FaultTest::Sync,
                b"guard" => FaultTest::Guard,
                b"unmapped" => FaultTest::Unmapped,
                _ => return Err(BootModeError::UnknownFault),
            });
        }
    }
    Ok(fault_test.unwrap_or_default())
}

/// Parses the optional `fault=` U-Boot test argument.
///
/// Tvisor always takes ownership from U-Boot and installs its EL2 stage-1
/// tables. The argument only selects an optional post-switch test:
///     sync: execute exception handler test by executing brk#0x600
///     guard: deliberately write the unmapped private-stack guard page
///     unmapped: deliberately read a representative unmapped VA
///     none: do nothing
///
/// # Safety
///
/// `argv` must contain `argc` readable pointers to NUL-terminated arguments.
pub unsafe fn fault_test_from_args(
    argc: isize,
    argv: *const *const u8,
) -> Result<FaultTest, BootModeError> {
    let argc = usize::try_from(argc).map_err(|_| BootModeError::InvalidArgCount)?;
    if argc == 0 || argc > MAX_ARGS {
        return Err(BootModeError::InvalidArgCount);
    }
    if argv.is_null() {
        return Err(BootModeError::NullArgv);
    }
    let mut arguments: [Option<&[u8]>; MAX_ARGS] = [None; MAX_ARGS];
    for (index, slot) in arguments[..argc].iter_mut().enumerate() {
        // SAFETY: Guaranteed by the caller.
        let pointer = unsafe { *argv.add(index) };
        if pointer.is_null() {
            return Err(BootModeError::NullArgument);
        }
        let mut length = None;
        for offset in 0..MAX_ARG_LEN {
            // SAFETY: Guaranteed by the caller up to the NUL terminator.
            if unsafe { *pointer.add(offset) } == 0 {
                length = Some(offset);
                break;
            }
        }
        let length = length.ok_or(BootModeError::UnterminatedArgument)?;
        // SAFETY: The bounded scan proved this prefix readable.
        *slot = Some(unsafe { core::slice::from_raw_parts(pointer, length) });
    }
    parse_fault_test(arguments[..argc].iter().map(|argument| argument.unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_fault_modes() {
        assert_eq!(
            parse_fault_test([b"fdt=1000".as_slice()]),
            Ok(FaultTest::None)
        );
        assert_eq!(
            parse_fault_test([b"fdt=1000".as_slice(), b"fault=sync".as_slice()]),
            Ok(FaultTest::Sync)
        );
        assert_eq!(
            parse_fault_test([b"fault=guard".as_slice()]),
            Ok(FaultTest::Guard)
        );
    }

    #[test]
    fn rejects_duplicate_and_unknown_faults() {
        assert_eq!(
            parse_fault_test([b"fault=sync".as_slice(), b"fault=none".as_slice()]),
            Err(BootModeError::DuplicateFault)
        );
        assert_eq!(
            parse_fault_test([b"fault=other".as_slice()]),
            Err(BootModeError::UnknownFault)
        );
    }
}
