use core::fmt;

use dtoolkit::{error::FdtParseError, fdt::Fdt};
use spin::Once;

const MAX_UBOOT_ARGS: usize = 16;
const MAX_UBOOT_ARG_LEN: usize = 64;
const FDT_ARG_PREFIX: &[u8] = b"fdt=";

static GLOBAL_FDT: Once<Fdt<'static>> = Once::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdtArgError {
    InvalidArgCount,
    NullArgv,
    NullArgument,
    ArgumentTooLong,
    MissingAddress,
    DuplicateAddress,
    InvalidAddress,
    AddressOverflow,
    ZeroAddress,
}

impl fmt::Display for FdtArgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgCount => write!(formatter, "invalid U-Boot argument count"),
            Self::NullArgv => write!(formatter, "U-Boot argv is null"),
            Self::NullArgument => write!(formatter, "a U-Boot argument is null"),
            Self::ArgumentTooLong => write!(formatter, "a U-Boot argument is too long"),
            Self::MissingAddress => write!(formatter, "the fdt= argument is missing"),
            Self::DuplicateAddress => write!(formatter, "multiple fdt= arguments were supplied"),
            Self::InvalidAddress => write!(formatter, "the fdt= address is not hexadecimal"),
            Self::AddressOverflow => write!(formatter, "the fdt= address overflows usize"),
            Self::ZeroAddress => write!(formatter, "the fdt= address is zero"),
        }
    }
}

#[derive(Debug)]
pub enum FdtInitError {
    AlreadyInitialized,
    Parse(FdtParseError),
}

impl fmt::Display for FdtInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialized => {
                write!(formatter, "the global FDT has already been initialized")
            }
            Self::Parse(error) => write!(formatter, "failed to parse FDT: {error}"),
        }
    }
}

/// Parses the DTB at `dtb_base` and installs it as tvisor's global FDT.
///
/// The global handle borrows the original DTB. Its memory must therefore stay
/// readable and unchanged for as long as callers can access [`fdt`].
///
/// # Safety
///
/// `dtb_base` must point to a readable FDT header. The complete memory range
/// described by the header's `totalsize` field must also be readable. Passing
/// an invalid pointer or a header whose size extends outside accessible memory
/// can cause undefined behavior before the parser can report an error.
pub unsafe fn fdt_init(dtb_base: *const u8) -> Result<&'static Fdt<'static>, FdtInitError> {
    if let Some(global) = GLOBAL_FDT.get() {
        return if core::ptr::eq(global.data().as_ptr(), dtb_base) {
            Ok(global)
        } else {
            Err(FdtInitError::AlreadyInitialized)
        };
    }

    // SAFETY: The caller guarantees that the pointer and the complete range
    // selected by the FDT header's totalsize field are readable. `from_raw`
    // validates the FDT contents before returning the handle.
    let parsed = unsafe { Fdt::from_raw(dtb_base) }.map_err(FdtInitError::Parse)?;
    let global = GLOBAL_FDT.call_once(|| parsed);

    if core::ptr::eq(global.data().as_ptr(), dtb_base) {
        Ok(global)
    } else {
        Err(FdtInitError::AlreadyInitialized)
    }
}

/// Returns the initialized global FDT.
pub fn fdt() -> Option<&'static Fdt<'static>> {
    GLOBAL_FDT.get()
}

/// Finds tvisor's tagged FDT address in U-Boot's standalone-application
/// `argc`/`argv` arguments.
///
/// Both `bootelf` and `go` pass strings through this ABI, but `go` also
/// includes its entry address as `argv[0]`. Searching for an explicit
/// `fdt=<hex-address>` tag avoids depending on its position.
///
/// # Safety
///
/// For a positive `argc`, `argv` must point to at least `argc` readable
/// argument pointers. Each non-null argument pointer must identify a valid
/// U-Boot NUL-terminated string.
pub unsafe fn fdt_address_from_uboot_args(
    argc: isize,
    argv: *const *const u8,
) -> Result<*const u8, FdtArgError> {
    let argc = usize::try_from(argc).map_err(|_| FdtArgError::InvalidArgCount)?;
    if argc == 0 || argc > MAX_UBOOT_ARGS {
        return Err(FdtArgError::InvalidArgCount);
    }
    if argv.is_null() {
        return Err(FdtArgError::NullArgv);
    }

    let mut address = None;
    for index in 0..argc {
        // SAFETY: The caller guarantees that argv contains argc readable
        // pointers.
        let argument = unsafe { *argv.add(index) };
        if argument.is_null() {
            return Err(FdtArgError::NullArgument);
        }

        // SAFETY: The caller guarantees a valid NUL-terminated U-Boot argument.
        let argument = unsafe { bounded_c_string(argument)? };
        let Some(value) = argument.strip_prefix(FDT_ARG_PREFIX) else {
            continue;
        };

        if address.is_some() {
            return Err(FdtArgError::DuplicateAddress);
        }
        address = Some(parse_hex_address(value)?);
    }

    let address = address.ok_or(FdtArgError::MissingAddress)?;
    if address == 0 {
        return Err(FdtArgError::ZeroAddress);
    }

    Ok(address as *const u8)
}

unsafe fn bounded_c_string<'a>(pointer: *const u8) -> Result<&'a [u8], FdtArgError> {
    for length in 0..MAX_UBOOT_ARG_LEN {
        // SAFETY: The caller guarantees that pointer identifies a valid
        // NUL-terminated U-Boot argument.
        if unsafe { *pointer.add(length) } == 0 {
            // SAFETY: All bytes through length were readable, as guaranteed by
            // the caller and established by the reads above.
            return Ok(unsafe { core::slice::from_raw_parts(pointer, length) });
        }
    }

    Err(FdtArgError::ArgumentTooLong)
}

fn parse_hex_address(value: &[u8]) -> Result<usize, FdtArgError> {
    let value = value
        .strip_prefix(b"0x")
        .or_else(|| value.strip_prefix(b"0X"))
        .unwrap_or(value);
    if value.is_empty() {
        return Err(FdtArgError::InvalidAddress);
    }

    value.iter().try_fold(0_usize, |address, byte| {
        let digit = match byte {
            b'0'..=b'9' => usize::from(byte - b'0'),
            b'a'..=b'f' => usize::from(byte - b'a') + 10,
            b'A'..=b'F' => usize::from(byte - b'A') + 10,
            _ => return Err(FdtArgError::InvalidAddress),
        };

        address
            .checked_mul(16)
            .and_then(|address| address.checked_add(digit))
            .ok_or(FdtArgError::AddressOverflow)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bootelf_argument_layout() {
        let fdt = b"fdt=37b3aca0\0";
        let argv = [fdt.as_ptr()];

        let address = unsafe { fdt_address_from_uboot_args(argv.len() as isize, argv.as_ptr()) };

        assert_eq!(address, Ok(0x37b3_aca0 as *const u8));
    }

    #[test]
    fn decodes_go_argument_layout() {
        let entry = b"4001010\0";
        let fdt = b"fdt=0x37B3ACA0\0";
        let argv = [entry.as_ptr(), fdt.as_ptr()];

        let address = unsafe { fdt_address_from_uboot_args(argv.len() as isize, argv.as_ptr()) };

        assert_eq!(address, Ok(0x37b3_aca0 as *const u8));
    }

    #[test]
    fn rejects_missing_fdt_argument() {
        let entry = b"4001010\0";
        let argv = [entry.as_ptr()];

        let result = unsafe { fdt_address_from_uboot_args(argv.len() as isize, argv.as_ptr()) };

        assert_eq!(result, Err(FdtArgError::MissingAddress));
    }

    #[test]
    fn rejects_duplicate_fdt_arguments() {
        let first = b"fdt=1000\0";
        let second = b"fdt=2000\0";
        let argv = [first.as_ptr(), second.as_ptr()];

        let result = unsafe { fdt_address_from_uboot_args(argv.len() as isize, argv.as_ptr()) };

        assert_eq!(result, Err(FdtArgError::DuplicateAddress));
    }

    #[test]
    fn rejects_non_hexadecimal_fdt_address() {
        let fdt = b"fdt=not-an-address\0";
        let argv = [fdt.as_ptr()];

        let result = unsafe { fdt_address_from_uboot_args(argv.len() as isize, argv.as_ptr()) };

        assert_eq!(result, Err(FdtArgError::InvalidAddress));
    }

    #[test]
    fn rejects_zero_fdt_address() {
        let fdt = b"fdt=0\0";
        let argv = [fdt.as_ptr()];

        let result = unsafe { fdt_address_from_uboot_args(argv.len() as isize, argv.as_ptr()) };

        assert_eq!(result, Err(FdtArgError::ZeroAddress));
    }
}
