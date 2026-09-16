use core::fmt;

use dtoolkit::{
    Node, Property,
    error::FdtParseError,
    fdt::{Fdt, FdtNode},
    standard::NodeStandard,
};
use spin::Once;

use crate::gicv2::GicV2Info;
use crate::system_info::{ConsoleInfo, ConsoleKind, PhysAddr, PhysRegion};

const MAX_UBOOT_ARGS: usize = 16;
const MAX_UBOOT_ARG_LEN: usize = 64;
const FDT_ARG_PREFIX: &[u8] = b"fdt=";
const IMAGE_ARG_PREFIX: &[u8] = b"image=";
const MAX_FDT_PATH: usize = 256;

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

/// A separately loaded Linux Image supplied by U-Boot. `size` is the exact
/// TFTP file length, not the header-declared in-memory Image extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UbootImage {
    pub address: *const u8,
    pub size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageArgError {
    InvalidArgCount,
    NullArgv,
    NullArgument,
    ArgumentTooLong,
    MissingImage,
    DuplicateImage,
    InvalidAddress,
    InvalidSize,
    AddressOverflow,
    ZeroAddress,
    ZeroSize,
}

impl fmt::Display for ImageArgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgCount => formatter.write_str("invalid U-Boot argument count"),
            Self::NullArgv => formatter.write_str("U-Boot argv is null"),
            Self::NullArgument => formatter.write_str("a U-Boot argument is null"),
            Self::ArgumentTooLong => formatter.write_str("a U-Boot argument is too long"),
            Self::MissingImage => formatter.write_str("the image= argument is missing"),
            Self::DuplicateImage => formatter.write_str("multiple image= arguments were supplied"),
            Self::InvalidAddress => formatter.write_str("the image address is not hexadecimal"),
            Self::InvalidSize => formatter.write_str("the image size is not hexadecimal"),
            Self::AddressOverflow => {
                formatter.write_str("the image address or size overflows usize")
            }
            Self::ZeroAddress => formatter.write_str("the image address is zero"),
            Self::ZeroSize => formatter.write_str("the image size is zero"),
        }
    }
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

/// Finds `image=<hex-address>,<hex-byte-size>` in U-Boot's standalone
/// arguments. The explicit source byte count lets tvisor zero-fill a compact
/// Image's BSS tail instead of reading beyond the TFTP payload.
///
/// # Safety
///
/// Has the same `argc`/`argv` requirements as [`fdt_address_from_uboot_args`].
pub unsafe fn image_from_uboot_args(
    argc: isize,
    argv: *const *const u8,
) -> Result<UbootImage, ImageArgError> {
    let argc = usize::try_from(argc).map_err(|_| ImageArgError::InvalidArgCount)?;
    if argc == 0 || argc > MAX_UBOOT_ARGS {
        return Err(ImageArgError::InvalidArgCount);
    }
    if argv.is_null() {
        return Err(ImageArgError::NullArgv);
    }

    let mut image = None;
    for index in 0..argc {
        let argument = unsafe { *argv.add(index) };
        if argument.is_null() {
            return Err(ImageArgError::NullArgument);
        }
        let argument = unsafe { bounded_c_string(argument) }.map_err(|error| match error {
            FdtArgError::ArgumentTooLong => ImageArgError::ArgumentTooLong,
            _ => unreachable!("bounded_c_string only returns ArgumentTooLong"),
        })?;
        let Some(value) = argument.strip_prefix(IMAGE_ARG_PREFIX) else {
            continue;
        };
        if image.is_some() {
            return Err(ImageArgError::DuplicateImage);
        }
        let Some(separator) = value.iter().position(|byte| *byte == b',') else {
            return Err(ImageArgError::InvalidSize);
        };
        let (address, size) = (&value[..separator], &value[separator + 1..]);
        let address = parse_hex_address(address).map_err(map_image_address_error)?;
        let size = parse_hex_address(size).map_err(map_image_size_error)?;
        if address == 0 {
            return Err(ImageArgError::ZeroAddress);
        }
        if size == 0 {
            return Err(ImageArgError::ZeroSize);
        }
        image = Some(UbootImage {
            address: address as *const u8,
            size,
        });
    }
    image.ok_or(ImageArgError::MissingImage)
}

const fn map_image_address_error(error: FdtArgError) -> ImageArgError {
    match error {
        FdtArgError::InvalidAddress => ImageArgError::InvalidAddress,
        FdtArgError::AddressOverflow => ImageArgError::AddressOverflow,
        _ => ImageArgError::InvalidAddress,
    }
}

const fn map_image_size_error(error: FdtArgError) -> ImageArgError {
    match error {
        FdtArgError::InvalidAddress => ImageArgError::InvalidSize,
        FdtArgError::AddressOverflow => ImageArgError::AddressOverflow,
        _ => ImageArgError::InvalidSize,
    }
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
    MissingInterrupt,
    InvalidInterrupt,
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
            Self::MissingInterrupt => "the console has no interrupt specifier",
            Self::InvalidInterrupt => "the console interrupt specifier is unsupported or invalid",
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

    let kind = if node.is_compatible(ConsoleKind::MiniUart.compatible_str()) {
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

    if register_size < kind.min_register_size() {
        return Err(ConsoleDiscoveryError::InvalidRegister);
    }

    // Translate register address from bus address to cpu address.
    let physical_address = translate_to_cpu_address(fdt, path, bus_address)?;
    if physical_address == 0 || !physical_address.is_multiple_of(kind.min_alignment()) {
        return Err(ConsoleDiscoveryError::InvalidRegister);
    }
    let registers = PhysRegion::new(PhysAddr::new(physical_address), register_size)
        .map_err(|_| ConsoleDiscoveryError::InvalidRegister)?;

    // Raspberry Pi's Mini UART is wired to the GIC with the standard
    // three-cell SPI specifier: <0, spi-number, level-high>.  Keep this
    // physical source in the console description so EL2 can wake an idle
    // guest when host-console input arrives.
    let interrupt_property = node
        .property("interrupts")
        .ok_or(ConsoleDiscoveryError::MissingInterrupt)?;
    let interrupts = interrupt_property.value();
    let irq = decode_console_gic_spi(interrupts)?;

    Ok(ConsoleInfo {
        kind,
        registers,
        irq,
    })
}

fn decode_console_gic_spi(interrupts: &[u8]) -> Result<u32, ConsoleDiscoveryError> {
    let interrupt_cells: [u8; 12] = interrupts
        .get(..12)
        .ok_or(ConsoleDiscoveryError::MissingInterrupt)?
        .try_into()
        .map_err(|_| ConsoleDiscoveryError::InvalidInterrupt)?;
    let interrupt_type = u32::from_be_bytes(
        interrupt_cells[0..4]
            .try_into()
            .map_err(|_| ConsoleDiscoveryError::InvalidInterrupt)?,
    );
    let interrupt_number = u32::from_be_bytes(
        interrupt_cells[4..8]
            .try_into()
            .map_err(|_| ConsoleDiscoveryError::InvalidInterrupt)?,
    );
    let interrupt_flags = u32::from_be_bytes(
        interrupt_cells[8..12]
            .try_into()
            .map_err(|_| ConsoleDiscoveryError::InvalidInterrupt)?,
    );
    if interrupt_type != 0 || interrupt_flags != 4 {
        return Err(ConsoleDiscoveryError::InvalidInterrupt);
    }
    interrupt_number
        .checked_add(32)
        .filter(|irq| *irq < 1020)
        .ok_or(ConsoleDiscoveryError::InvalidInterrupt)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GicV2DiscoveryError {
    MissingController,
    DisabledController,
    InvalidRegister,
    MissingRegister,
    InvalidRegion,
    MissingParent,
    MissingRanges,
    AddressNotMapped,
    AddressOverflow,
    PathTooLong,
}

impl fmt::Display for GicV2DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingController => "no supported GICv2 interrupt controller is present",
            Self::DisabledController => "the GICv2 interrupt controller is disabled",
            Self::InvalidRegister => "the GICv2 reg property is invalid",
            Self::MissingRegister => "the GICv2 reg property lacks a required interface",
            Self::InvalidRegion => "a GICv2 interface is too small or unaligned",
            Self::MissingParent => "the GICv2 node has no parent bus",
            Self::MissingRanges => "a GICv2 parent bus has no ranges property",
            Self::AddressNotMapped => "a GICv2 register address is not covered by parent ranges",
            Self::AddressOverflow => "a GICv2 register address overflows",
            Self::PathTooLong => "the GICv2 DTB path exceeds the supported length",
        };
        formatter.write_str(message)
    }
}

/// Discovers the four GICv2 interfaces required for hardware-backed List
/// Register delivery. The `reg` entries are translated through parent buses to
/// CPU physical addresses; their order is GICD, GICC, GICH, then GICV.
pub fn discover_gic_v2(fdt: Fdt<'_>) -> Result<GicV2Info, GicV2DiscoveryError> {
    let mut path = [0_u8; MAX_FDT_PATH];
    path[0] = b'/';
    find_gic_v2(fdt, fdt.root(), &mut path, 1)?.ok_or(GicV2DiscoveryError::MissingController)
}

fn find_gic_v2(
    fdt: Fdt<'_>,
    node: FdtNode<'_>,
    path: &mut [u8; MAX_FDT_PATH],
    path_len: usize,
) -> Result<Option<GicV2Info>, GicV2DiscoveryError> {
    if node.is_compatible("arm,gic-400") || node.is_compatible("arm,cortex-a15-gic") {
        if node
            .status()
            .map_err(|_| GicV2DiscoveryError::DisabledController)?
            != dtoolkit::standard::Status::Okay
        {
            return Err(GicV2DiscoveryError::DisabledController);
        }
        let path = core::str::from_utf8(&path[..path_len])
            .map_err(|_| GicV2DiscoveryError::InvalidRegister)?;
        return decode_gic_v2(fdt, node, path).map(Some);
    }

    for child in node.children() {
        let separator = usize::from(path_len != 1);
        let child_name = child.name().as_bytes();
        let next_len = path_len
            .checked_add(separator)
            .and_then(|length| length.checked_add(child_name.len()))
            .ok_or(GicV2DiscoveryError::PathTooLong)?;
        if next_len > path.len() {
            return Err(GicV2DiscoveryError::PathTooLong);
        }
        if separator != 0 {
            path[path_len] = b'/';
        }
        path[path_len + separator..next_len].copy_from_slice(child_name);
        if let Some(info) = find_gic_v2(fdt, child, path, next_len)? {
            return Ok(Some(info));
        }
    }
    Ok(None)
}

fn decode_gic_v2(
    fdt: Fdt<'_>,
    node: FdtNode<'_>,
    path: &str,
) -> Result<GicV2Info, GicV2DiscoveryError> {
    let mut registers = node
        .reg()
        .map_err(|_| GicV2DiscoveryError::InvalidRegister)?
        .ok_or(GicV2DiscoveryError::MissingRegister)?;
    let mut regions = [None; 4];
    for region in &mut regions {
        let register = registers
            .next()
            .ok_or(GicV2DiscoveryError::MissingRegister)?;
        let address = register
            .address::<u64>()
            .map_err(|_| GicV2DiscoveryError::InvalidRegister)?;
        let size = register
            .size::<u64>()
            .map_err(|_| GicV2DiscoveryError::InvalidRegister)?;
        let address = translate_gic_to_cpu_address(fdt, path, address)?;
        *region = Some(
            PhysRegion::new(PhysAddr::new(address), size)
                .map_err(|_| GicV2DiscoveryError::InvalidRegion)?,
        );
    }

    if let [
        Some(distributor),
        Some(cpu_interface),
        Some(hypervisor_interface),
        Some(virtual_cpu_interface),
    ] = regions
    {
        GicV2Info::new(
            distributor,
            cpu_interface,
            hypervisor_interface,
            virtual_cpu_interface,
        )
        .ok_or(GicV2DiscoveryError::InvalidRegion)
    } else {
        Err(GicV2DiscoveryError::MissingRegister)
    }
}

fn translate_gic_to_cpu_address(
    fdt: Fdt<'_>,
    device_path: &str,
    mut address: u64,
) -> Result<u64, GicV2DiscoveryError> {
    let mut bus_path = parent_path(device_path).ok_or(GicV2DiscoveryError::MissingParent)?;
    while bus_path != "/" {
        let bus = fdt
            .find_node(bus_path)
            .ok_or(GicV2DiscoveryError::MissingParent)?;
        let mut ranges = bus
            .ranges()
            .map_err(|_| GicV2DiscoveryError::MissingRanges)?
            .ok_or(GicV2DiscoveryError::MissingRanges)?;
        if let Some(first) = ranges.next() {
            let mut translated = None;
            for range in core::iter::once(first).chain(ranges) {
                let child = range
                    .child_bus_address::<u64>()
                    .map_err(|_| GicV2DiscoveryError::AddressOverflow)?;
                let parent = range
                    .parent_bus_address::<u64>()
                    .map_err(|_| GicV2DiscoveryError::AddressOverflow)?;
                let length = range
                    .length::<u64>()
                    .map_err(|_| GicV2DiscoveryError::AddressOverflow)?;
                let Some(offset) = address.checked_sub(child) else {
                    continue;
                };
                if offset < length {
                    translated = Some(
                        parent
                            .checked_add(offset)
                            .ok_or(GicV2DiscoveryError::AddressOverflow)?,
                    );
                    break;
                }
            }
            address = translated.ok_or(GicV2DiscoveryError::AddressNotMapped)?;
        }
        bus_path = parent_path(bus_path).ok_or(GicV2DiscoveryError::MissingParent)?;
    }
    Ok(address)
}
#[cfg(test)]
mod tests {
    use super::*;
    use dtoolkit::fdt::Fdt;
    use dtoolkit::model::{DeviceTree, DeviceTreeNode, DeviceTreeProperty};
    use std::vec::Vec;

    fn property(name: &str, value: Vec<u8>) -> DeviceTreeProperty {
        DeviceTreeProperty::new_unchecked(name, value)
    }

    fn gic_registers() -> Vec<u8> {
        let mut registers = Vec::new();
        for (address, size) in [
            (0x1000_u32, 0x1000_u32),
            (0x2000, 0x1000),
            (0x4000, 0x1000),
            (0x6000, 0x2000),
        ] {
            registers.extend_from_slice(&address.to_be_bytes());
            registers.extend_from_slice(&size.to_be_bytes());
        }
        registers
    }

    #[test]
    fn discovers_gicv2_regions_through_parent_ranges() {
        let mut tree = DeviceTree::new();
        tree.root
            .add_property(property("#address-cells", 2_u32.to_be_bytes().to_vec()));
        tree.root
            .add_property(property("#size-cells", 1_u32.to_be_bytes().to_vec()));

        let mut gic = DeviceTreeNode::new_unchecked("interrupt-controller@1000");
        gic.add_property(property("compatible", b"arm,gic-400\0".to_vec()));
        gic.add_property(property("reg", gic_registers()));

        let mut soc = DeviceTreeNode::new_unchecked("soc");
        soc.add_property(property("#address-cells", 1_u32.to_be_bytes().to_vec()));
        soc.add_property(property("#size-cells", 1_u32.to_be_bytes().to_vec()));
        let mut ranges = Vec::new();
        ranges.extend_from_slice(&0_u32.to_be_bytes());
        ranges.extend_from_slice(&0xff84_0000_u64.to_be_bytes());
        ranges.extend_from_slice(&0x0010_0000_u32.to_be_bytes());
        soc.add_property(property("ranges", ranges));
        soc.add_child(gic);
        tree.root.add_child(soc);

        let blob = tree.to_dtb();
        let info = discover_gic_v2(Fdt::new(&blob).unwrap()).unwrap();
        assert_eq!(info.distributor().start().value(), 0xff84_1000);
        assert_eq!(info.cpu_interface().start().value(), 0xff84_2000);
        assert_eq!(info.hypervisor_interface().start().value(), 0xff84_4000);
        assert_eq!(info.virtual_cpu_interface().start().value(), 0xff84_6000);
        assert_eq!(info.virtual_cpu_interface().size(), 0x2000);
    }

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
    fn decodes_image_address_and_exact_file_size() {
        let entry = b"4001010\0";
        let image = b"image=0x04000000,0x305008\0";
        let argv = [entry.as_ptr(), image.as_ptr()];

        let source = unsafe { image_from_uboot_args(argv.len() as isize, argv.as_ptr()) }
            .expect("valid image argument");

        assert_eq!(source.address, 0x0400_0000 as *const u8);
        assert_eq!(source.size, 0x305008);
    }

    #[test]
    fn rejects_image_without_a_file_size() {
        let image = b"image=4000000\0";
        let argv = [image.as_ptr()];
        assert_eq!(
            unsafe { image_from_uboot_args(argv.len() as isize, argv.as_ptr()) },
            Err(ImageArgError::InvalidSize)
        );
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

    #[test]
    fn decodes_level_high_gic_spi_for_console() {
        assert_eq!(
            decode_console_gic_spi(&[0, 0, 0, 0, 0, 0, 0, 93, 0, 0, 0, 4]),
            Ok(125)
        );
        assert_eq!(
            decode_console_gic_spi(&[0, 0, 0, 1, 0, 0, 0, 93, 0, 0, 0, 4]),
            Err(ConsoleDiscoveryError::InvalidInterrupt)
        );
    }
}
