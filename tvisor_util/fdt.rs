use core::fmt;
use core::mem::size_of;

use dtoolkit::{Node, Property, error::FdtParseError, fdt::Fdt, standard::NodeStandard};
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

const MINI_UART_COMPATIBLE: &str = "brcm,bcm2835-aux-uart";
const MINI_UART_MIN_REGISTER_SIZE: u64 = 0x18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleKind {
    MiniUart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleInfo {
    pub kind: ConsoleKind,
    /// CPU physical address of the UART's first register.
    pub register_base: usize,
    pub register_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleDiscoveryError {
    MissingChosen,
    MissingStdoutPath,
    InvalidStdoutPath,
    MissingAliases,
    MissingAlias,
    MissingConsoleNode,
    ConsoleDisabled,
    UnsupportedConsole,
    MissingRegister,
    InvalidRegister,
    MissingParent,
    MissingRanges,
    AddressNotMapped,
    AddressOverflow,
}

impl fmt::Display for ConsoleDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingChosen => "the /chosen node is missing",
            Self::MissingStdoutPath => "the /chosen stdout-path property is missing",
            Self::InvalidStdoutPath => "the stdout-path property is invalid",
            Self::MissingAliases => "stdout-path uses an alias but /aliases is missing",
            Self::MissingAlias => "the stdout-path alias is missing or invalid",
            Self::MissingConsoleNode => "the stdout-path console node is missing",
            Self::ConsoleDisabled => "the stdout-path console is not enabled",
            Self::UnsupportedConsole => "the stdout-path console type is unsupported",
            Self::MissingRegister => "the console has no reg entry",
            Self::InvalidRegister => "the console reg entry is invalid",
            Self::MissingParent => "the console path has no parent bus",
            Self::MissingRanges => "a console parent bus has no ranges property",
            Self::AddressNotMapped => "the console address is not covered by parent ranges",
            Self::AddressOverflow => "the translated console address overflows",
        };
        formatter.write_str(message)
    }
}

/// Discovers the active console without performing MMIO.
///
/// /chosen/stdout-path may contain either an absolute path or an alias and
/// may include serial options after a colon. The first reg address is
/// translated through every ancestor bus's ranges property into the CPU
/// physical address space.
pub fn discover_console(fdt: Fdt<'_>) -> Result<ConsoleInfo, ConsoleDiscoveryError> {
    let path = resolve_stdout_path(fdt)?;
    let node = fdt
        .find_node(path)
        .ok_or(ConsoleDiscoveryError::MissingConsoleNode)?;

    if node
        .status()
        .map_err(|_| ConsoleDiscoveryError::ConsoleDisabled)?
        != dtoolkit::standard::Status::Okay
    {
        return Err(ConsoleDiscoveryError::ConsoleDisabled);
    }

    let kind = if node.is_compatible(MINI_UART_COMPATIBLE) {
        ConsoleKind::MiniUart
    } else {
        return Err(ConsoleDiscoveryError::UnsupportedConsole);
    };

    let mut registers = node
        .reg()
        .map_err(|_| ConsoleDiscoveryError::InvalidRegister)?
        .ok_or(ConsoleDiscoveryError::MissingRegister)?;
    let register = registers
        .next()
        .ok_or(ConsoleDiscoveryError::MissingRegister)?;
    let bus_address = register
        .address::<u64>()
        .map_err(|_| ConsoleDiscoveryError::InvalidRegister)?;
    let register_size = register
        .size::<u64>()
        .map_err(|_| ConsoleDiscoveryError::InvalidRegister)?;

    if register_size < MINI_UART_MIN_REGISTER_SIZE {
        return Err(ConsoleDiscoveryError::InvalidRegister);
    }

    let physical_address = translate_to_cpu_address(fdt, path, bus_address)?;
    let register_base =
        usize::try_from(physical_address).map_err(|_| ConsoleDiscoveryError::AddressOverflow)?;
    let register_size =
        usize::try_from(register_size).map_err(|_| ConsoleDiscoveryError::AddressOverflow)?;
    if register_base == 0 || !register_base.is_multiple_of(size_of::<u32>()) {
        return Err(ConsoleDiscoveryError::InvalidRegister);
    }

    Ok(ConsoleInfo {
        kind,
        register_base,
        register_size,
    })
}

fn resolve_stdout_path<'a>(fdt: Fdt<'a>) -> Result<&'a str, ConsoleDiscoveryError> {
    let chosen = fdt.chosen().ok_or(ConsoleDiscoveryError::MissingChosen)?;
    let stdout_path = chosen
        .stdout_path()
        .map_err(|_| ConsoleDiscoveryError::InvalidStdoutPath)?
        .ok_or(ConsoleDiscoveryError::MissingStdoutPath)?;
    let selector = stdout_path
        .split(':')
        .next()
        .filter(|value| !value.is_empty())
        .ok_or(ConsoleDiscoveryError::InvalidStdoutPath)?;

    if selector.starts_with('/') {
        return Ok(selector);
    }

    let aliases = fdt
        .find_node("/aliases")
        .ok_or(ConsoleDiscoveryError::MissingAliases)?;
    aliases
        .property(selector)
        .ok_or(ConsoleDiscoveryError::MissingAlias)?
        .as_str()
        .map_err(|_| ConsoleDiscoveryError::MissingAlias)
}

fn translate_to_cpu_address(
    fdt: Fdt<'_>,
    device_path: &str,
    mut address: u64,
) -> Result<u64, ConsoleDiscoveryError> {
    let mut bus_path = parent_path(device_path).ok_or(ConsoleDiscoveryError::MissingParent)?;

    while bus_path != "/" {
        let bus = fdt
            .find_node(bus_path)
            .ok_or(ConsoleDiscoveryError::MissingParent)?;
        let mut ranges = bus
            .ranges()
            .map_err(|_| ConsoleDiscoveryError::MissingRanges)?
            .ok_or(ConsoleDiscoveryError::MissingRanges)?;

        if let Some(first) = ranges.next() {
            let mut translated = None;
            for range in core::iter::once(first).chain(ranges) {
                let child = range
                    .child_bus_address::<u64>()
                    .map_err(|_| ConsoleDiscoveryError::AddressOverflow)?;
                let parent = range
                    .parent_bus_address::<u64>()
                    .map_err(|_| ConsoleDiscoveryError::AddressOverflow)?;
                let length = range
                    .length::<u64>()
                    .map_err(|_| ConsoleDiscoveryError::AddressOverflow)?;

                let Some(offset) = address.checked_sub(child) else {
                    continue;
                };
                if offset >= length {
                    continue;
                }

                translated = Some(
                    parent
                        .checked_add(offset)
                        .ok_or(ConsoleDiscoveryError::AddressOverflow)?,
                );
                break;
            }
            address = translated.ok_or(ConsoleDiscoveryError::AddressNotMapped)?;
        }

        bus_path = parent_path(bus_path).ok_or(ConsoleDiscoveryError::MissingParent)?;
    }

    Ok(address)
}

fn parent_path(path: &str) -> Option<&str> {
    if path == "/" || !path.starts_with('/') {
        return None;
    }
    let separator = path.rfind('/')?;
    Some(if separator == 0 {
        "/"
    } else {
        &path[..separator]
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

    #[test]
    fn finds_parent_paths() {
        assert_eq!(parent_path("/soc/serial@7e215040"), Some("/soc"));
        assert_eq!(parent_path("/soc"), Some("/"));
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path("relative"), None);
    }
}
