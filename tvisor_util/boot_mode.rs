use core::fmt;

const MAX_ARGS: usize = 16;
const MAX_ARG_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakeoverRequest {
    pub fault_test: FaultTest,
    pub switch_page_tables: bool,
}

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
    DuplicateMode,
    UnknownMode,
    DuplicateFault,
    UnknownFault,
    FaultWithoutTakeover,
    DuplicateMmu,
    UnknownMmu,
    MmuWithoutTakeover,
    TranslationFaultWithoutMmuSwitch,
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
    let mut fault_test = None;
    let mut switch_page_tables = None;
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
        if let Some(value) = argument.strip_prefix(b"mmu=") {
            if switch_page_tables.is_some() {
                return Err(BootModeError::DuplicateMmu);
            }
            switch_page_tables = Some(match value {
                b"inherit" => false,
                b"switch" => true,
                _ => return Err(BootModeError::UnknownMmu),
            });
        }
    }
    match (
        takeover.unwrap_or(false),
        fault_test.unwrap_or_default(),
        switch_page_tables.unwrap_or(false),
    ) {
        (false, FaultTest::Sync | FaultTest::Guard | FaultTest::Unmapped, _) => {
            Err(BootModeError::FaultWithoutTakeover)
        }
        (false, FaultTest::None, true) => Err(BootModeError::MmuWithoutTakeover),
        (false, FaultTest::None, false) => Ok(None),
        (true, FaultTest::Guard | FaultTest::Unmapped, false) => {
            Err(BootModeError::TranslationFaultWithoutMmuSwitch)
        }
        (true, fault_test, switch_page_tables) => Ok(Some(TakeoverRequest {
            fault_test,
            switch_page_tables,
        })),
    }
}

/// Parses optional `mode=takeover` and `fault=sync` U-Boot arguments.
///
/// mode:
///     default: diagnostic mode which will return to u-boot
///     takeover: execute takeover mode which doesn't return to u-boot
/// fault: only usable for `takeover` mode
///     sync: execute exception handler test by executing brk#0x600
///     guard: deliberately write the unmapped private-stack guard page
///     unmapped: deliberately read a representative unmapped VA
///     none: do nothing
/// mmu: only usable for `takeover` mode
///     inherit: retain U-Boot's active EL2 stage-1 tables
///     switch: install tvisor's EL2 stage-1 tables
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
                fault_test: FaultTest::Sync,
                switch_page_tables: false,
            }))
        );
        assert_eq!(
            parse_mode([b"mode=takeover".as_slice(), b"mmu=switch".as_slice()]),
            Ok(Some(TakeoverRequest {
                fault_test: FaultTest::None,
                switch_page_tables: true,
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
        assert_eq!(
            parse_mode([b"mmu=switch".as_slice()]),
            Err(BootModeError::MmuWithoutTakeover)
        );
        assert_eq!(
            parse_mode([b"mode=takeover".as_slice(), b"fault=guard".as_slice()]),
            Err(BootModeError::TranslationFaultWithoutMmuSwitch)
        );
    }
}
