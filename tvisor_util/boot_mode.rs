use core::fmt;

const MAX_ARGS: usize = 16;
const MAX_ARG_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakeoverRequest {
    pub test_sync_fault: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootModeError {
    InvalidArgCount,
    NullArgv,
    NullArgument,
    UnterminatedArgument,
    DuplicateMode,
    UnknownMode,
    DuplicateFault,
    UnknownFault,
    FaultWithoutTakeover,
}

impl fmt::Display for BootModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

pub fn parse_mode<'a>(
    arguments: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Option<TakeoverRequest>, BootModeError> {
    let mut takeover = None;
    let mut sync_fault = None;
    for argument in arguments {
        if let Some(value) = argument.strip_prefix(b"mode=") {
            if takeover.is_some() {
                return Err(BootModeError::DuplicateMode);
            }
            takeover = Some(match value {
                b"diagnostic" => false,
                b"takeover" => true,
                _ => return Err(BootModeError::UnknownMode),
            });
        }
        if let Some(value) = argument.strip_prefix(b"fault=") {
            if sync_fault.is_some() {
                return Err(BootModeError::DuplicateFault);
            }
            sync_fault = Some(match value {
                b"none" => false,
                b"sync" => true,
                _ => return Err(BootModeError::UnknownFault),
            });
        }
    }
    match (takeover.unwrap_or(false), sync_fault.unwrap_or(false)) {
        (false, true) => Err(BootModeError::FaultWithoutTakeover),
        (false, false) => Ok(None),
        (true, test_sync_fault) => Ok(Some(TakeoverRequest { test_sync_fault })),
    }
}

/// Parses optional `mode=takeover` and `fault=sync` U-Boot arguments.
///
/// mode:
///     default: diagnostic mode which will return to u-boot
///     takeover: execute takeover mode which doesn't return to u-boot
/// fault: only usable for `takeover` mode
///     sync: execute exception handler test by executing brk#0x600
///     none: do nothing
///
/// # Safety
///
/// `argv` must contain `argc` readable pointers to NUL-terminated arguments.
pub unsafe fn takeover_request_from_args(
    argc: isize,
    argv: *const *const u8,
) -> Result<Option<TakeoverRequest>, BootModeError> {
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
    parse_mode(arguments[..argc].iter().map(|argument| argument.unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_takeover_modes() {
        assert_eq!(parse_mode([b"fdt=1000".as_slice()]), Ok(None));
        assert_eq!(
            parse_mode([b"mode=takeover".as_slice(), b"fault=sync".as_slice()]),
            Ok(Some(TakeoverRequest {
                test_sync_fault: true
            }))
        );
    }

    #[test]
    fn rejects_fault_on_returnable_path_and_duplicates() {
        assert_eq!(
            parse_mode([b"fault=sync".as_slice()]),
            Err(BootModeError::FaultWithoutTakeover)
        );
        assert_eq!(
            parse_mode([b"mode=takeover".as_slice(), b"mode=diagnostic".as_slice()]),
            Err(BootModeError::DuplicateMode)
        );
    }
}
