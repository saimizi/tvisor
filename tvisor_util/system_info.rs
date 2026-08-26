use core::fmt;

pub const MAX_RAM_REGIONS: usize = 8;
pub const MAX_RESERVED_REGIONS: usize = 64;
pub const MAX_DYNAMIC_RESERVATIONS: usize = 16;
pub const MAX_DYNAMIC_ALLOC_RANGES: usize = 4;
pub const MAX_MMIO_REGIONS: usize = 64;
pub const MAX_BUS_TRANSLATIONS: usize = 16;
pub const MAX_CPUS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(u64);

impl PhysAddr {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn checked_add(self, offset: u64) -> Option<Self> {
        match self.0.checked_add(offset) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#018x}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionError {
    Empty,
    EndBeforeStart,
    AddressOverflow,
}

impl fmt::Display for RegionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("physical region is empty"),
            Self::EndBeforeStart => formatter.write_str("physical region ends before it starts"),
            Self::AddressOverflow => formatter.write_str("physical region end overflows"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysRegion {
    start: PhysAddr,
    size: u64,
}

impl PhysRegion {
    pub const fn new(start: PhysAddr, size: u64) -> Result<Self, RegionError> {
        if size == 0 {
            return Err(RegionError::Empty);
        }
        if start.checked_add(size).is_none() {
            return Err(RegionError::AddressOverflow);
        }

        Ok(Self { start, size })
    }

    pub const fn from_bounds(start: PhysAddr, end: PhysAddr) -> Result<Self, RegionError> {
        if end.value() < start.value() {
            return Err(RegionError::EndBeforeStart);
        }
        if end.value() == start.value() {
            return Err(RegionError::Empty);
        }

        Self::new(start, end.value() - start.value())
    }

    pub const fn start(self) -> PhysAddr {
        self.start
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn end(self) -> PhysAddr {
        // Construction proves that this addition cannot overflow.
        PhysAddr::new(self.start.value() + self.size)
    }

    pub const fn contains_address(self, address: PhysAddr) -> bool {
        address.value() >= self.start.value() && address.value() < self.end().value()
    }

    pub const fn contains_region(self, other: Self) -> bool {
        other.start.value() >= self.start.value() && other.end().value() <= self.end().value()
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.start.value() < other.end().value() && other.start.value() < self.end().value()
    }

    pub const fn is_adjacent(self, other: Self) -> bool {
        self.end().value() == other.start.value() || other.end().value() == self.start.value()
    }
}

impl fmt::Display for PhysRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{}, {})", self.start, self.end())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityError {
    capacity: usize,
}

impl CapacityError {
    pub const fn capacity(self) -> usize {
        self.capacity
    }
}

impl fmt::Display for CapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "fixed-capacity list is full (capacity {})",
            self.capacity
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedList<T: Copy, const N: usize> {
    entries: [Option<T>; N],
    len: usize,
}

impl<T: Copy, const N: usize> FixedList<T, N> {
    pub const fn new() -> Self {
        Self {
            entries: [None; N],
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    pub fn push(&mut self, value: T) -> Result<(), CapacityError> {
        if self.is_full() {
            return Err(CapacityError { capacity: N });
        }

        self.entries[self.len] = Some(value);
        self.len += 1;
        Ok(())
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        self.entries[index].as_ref()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> + DoubleEndedIterator {
        self.entries[..self.len]
            .iter()
            .map(|entry| entry.as_ref().expect("initialized list entry"))
    }
}

impl<T: Copy, const N: usize> Default for FixedList<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, T: Copy, const N: usize> IntoIterator for &'a FixedList<T, N> {
    type Item = &'a T;
    type IntoIter = core::iter::Map<core::slice::Iter<'a, Option<T>>, fn(&'a Option<T>) -> &'a T>;

    fn into_iter(self) -> Self::IntoIter {
        fn initialized<T>(entry: &Option<T>) -> &T {
            entry.as_ref().expect("initialized list entry")
        }

        self.entries[..self.len].iter().map(initialized::<T>)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamSource {
    DeviceTree,
    Firmware,
    FirmwareCarveout,
}

impl fmt::Display for RamSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceTree => formatter.write_str("DeviceTree"),
            Self::Firmware => formatter.write_str("Firmware"),
            Self::FirmwareCarveout => formatter.write_str("Firmware Carve-out"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamRegion {
    pub region: PhysRegion,
    pub source: RamSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationOrigin {
    FdtReservationBlock,
    ReservedMemoryNode,
    Firmware,
    Device,
    Bootloader,
    Dtb,
    TvisorImage,
    LinuxPolicy,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationOwner {
    Firmware,
    Device,
    Bootloader,
    Tvisor,
    HostPolicy,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReservationAttributes {
    pub no_map: bool,
    pub reusable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedRegion {
    pub region: PhysRegion,
    pub origin: ReservationOrigin,
    pub owner: ReservationOwner,
    pub attributes: ReservationAttributes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicReservationError {
    Empty,
    InvalidAlignment,
}

impl fmt::Display for DynamicReservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("dynamic reservation is empty"),
            Self::InvalidAlignment => {
                formatter.write_str("dynamic reservation alignment is not a power of two")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynamicReservation {
    size: u64,
    alignment: Option<u64>,
    origin: ReservationOrigin,
    owner: ReservationOwner,
    attributes: ReservationAttributes,
    alloc_ranges: FixedList<PhysRegion, MAX_DYNAMIC_ALLOC_RANGES>,
}

impl DynamicReservation {
    pub const fn new(
        size: u64,
        alignment: Option<u64>,
        origin: ReservationOrigin,
        owner: ReservationOwner,
        attributes: ReservationAttributes,
    ) -> Result<Self, DynamicReservationError> {
        if size == 0 {
            return Err(DynamicReservationError::Empty);
        }
        if let Some(alignment) = alignment
            && !alignment.is_power_of_two()
        {
            return Err(DynamicReservationError::InvalidAlignment);
        }

        Ok(Self {
            size,
            alignment,
            origin,
            owner,
            attributes,
            alloc_ranges: FixedList::new(),
        })
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn alignment(self) -> Option<u64> {
        self.alignment
    }

    pub const fn origin(self) -> ReservationOrigin {
        self.origin
    }

    pub const fn owner(self) -> ReservationOwner {
        self.owner
    }

    pub const fn attributes(self) -> ReservationAttributes {
        self.attributes
    }

    pub const fn alloc_ranges(&self) -> &FixedList<PhysRegion, MAX_DYNAMIC_ALLOC_RANGES> {
        &self.alloc_ranges
    }

    pub fn add_alloc_range(&mut self, region: PhysRegion) -> Result<(), CapacityError> {
        self.alloc_ranges.push(region)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioKind {
    Console,
    InterruptController,
    Timer,
    BusWindow,
    Device,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRegion {
    pub region: PhysRegion,
    pub kind: MmioKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusTranslation {
    child: PhysRegion,
    parent: PhysRegion,
}

impl BusTranslation {
    pub const fn new(
        child_start: PhysAddr,
        parent_start: PhysAddr,
        size: u64,
    ) -> Result<Self, RegionError> {
        Ok(Self {
            child: match PhysRegion::new(child_start, size) {
                Ok(region) => region,
                Err(error) => return Err(error),
            },
            parent: match PhysRegion::new(parent_start, size) {
                Ok(region) => region,
                Err(error) => return Err(error),
            },
        })
    }

    pub const fn child(self) -> PhysRegion {
        self.child
    }

    pub const fn parent(self) -> PhysRegion {
        self.parent
    }

    pub const fn size(self) -> u64 {
        self.child.size()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuStatus {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuEnableMethod {
    AlreadyRunning,
    SpinTable { release_address: PhysAddr },
    Psci,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuInfo {
    pub affinity: u64,
    pub status: CpuStatus,
    pub enable_method: CpuEnableMethod,
    pub is_current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleKind {
    MiniUart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleInfo {
    pub kind: ConsoleKind,
    pub registers: PhysRegion,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SystemInfo {
    ram: FixedList<RamRegion, MAX_RAM_REGIONS>,
    reserved: FixedList<ReservedRegion, MAX_RESERVED_REGIONS>,
    dynamic_reserved: FixedList<DynamicReservation, MAX_DYNAMIC_RESERVATIONS>,
    mmio: FixedList<MmioRegion, MAX_MMIO_REGIONS>,
    bus_translations: FixedList<BusTranslation, MAX_BUS_TRANSLATIONS>,
    cpus: FixedList<CpuInfo, MAX_CPUS>,
    console: Option<ConsoleInfo>,
}

impl SystemInfo {
    pub const fn new() -> Self {
        Self {
            ram: FixedList::new(),
            reserved: FixedList::new(),
            dynamic_reserved: FixedList::new(),
            mmio: FixedList::new(),
            bus_translations: FixedList::new(),
            cpus: FixedList::new(),
            console: None,
        }
    }

    pub const fn ram(&self) -> &FixedList<RamRegion, MAX_RAM_REGIONS> {
        &self.ram
    }

    pub const fn reserved(&self) -> &FixedList<ReservedRegion, MAX_RESERVED_REGIONS> {
        &self.reserved
    }

    pub const fn dynamic_reserved(
        &self,
    ) -> &FixedList<DynamicReservation, MAX_DYNAMIC_RESERVATIONS> {
        &self.dynamic_reserved
    }

    pub const fn mmio(&self) -> &FixedList<MmioRegion, MAX_MMIO_REGIONS> {
        &self.mmio
    }

    pub const fn bus_translations(&self) -> &FixedList<BusTranslation, MAX_BUS_TRANSLATIONS> {
        &self.bus_translations
    }

    pub const fn cpus(&self) -> &FixedList<CpuInfo, MAX_CPUS> {
        &self.cpus
    }

    pub const fn console(&self) -> Option<ConsoleInfo> {
        self.console
    }

    pub fn add_ram(&mut self, region: RamRegion) -> Result<(), CapacityError> {
        self.ram.push(region)
    }

    pub fn add_reserved(&mut self, region: ReservedRegion) -> Result<(), CapacityError> {
        self.reserved.push(region)
    }

    pub fn add_dynamic_reserved(
        &mut self,
        reservation: DynamicReservation,
    ) -> Result<(), CapacityError> {
        self.dynamic_reserved.push(reservation)
    }

    pub fn add_mmio(&mut self, region: MmioRegion) -> Result<(), CapacityError> {
        self.mmio.push(region)
    }

    pub fn add_bus_translation(
        &mut self,
        translation: BusTranslation,
    ) -> Result<(), CapacityError> {
        self.bus_translations.push(translation)
    }

    pub fn add_cpu(&mut self, cpu: CpuInfo) -> Result<(), CapacityError> {
        self.cpus.push(cpu)
    }

    pub fn set_console(&mut self, console: ConsoleInfo) {
        self.console = Some(console);
    }
}

impl Default for SystemInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SystemInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "System information:")?;
        let mut printed_ram = [false; MAX_RAM_REGIONS];
        for _ in 0..self.ram.len() {
            let Some((index, ram)) = self
                .ram
                .iter()
                .enumerate()
                .filter(|(index, _)| !printed_ram[*index])
                .min_by_key(|(_, ram)| ram.region.start())
            else {
                break;
            };
            printed_ram[index] = true;
            writeln!(formatter, "  RAM        {} {}", ram.region, ram.source)?;
        }
        for reserved in &self.reserved {
            writeln!(
                formatter,
                "  RESERVED   {} origin={:?} owner={:?} no_map={} reusable={}",
                reserved.region,
                reserved.origin,
                reserved.owner,
                reserved.attributes.no_map,
                reserved.attributes.reusable
            )?;
        }
        for reserved in &self.dynamic_reserved {
            writeln!(
                formatter,
                "  DYNAMIC    size={:#x} alignment={:?} origin={:?} owner={:?} no_map={} reusable={}",
                reserved.size(),
                reserved.alignment(),
                reserved.origin(),
                reserved.owner(),
                reserved.attributes().no_map,
                reserved.attributes().reusable
            )?;
            for range in reserved.alloc_ranges() {
                writeln!(formatter, "             alloc_range={}", range)?;
            }
        }
        for mmio in &self.mmio {
            writeln!(formatter, "  MMIO       {} {:?}", mmio.region, mmio.kind)?;
        }
        for translation in &self.bus_translations {
            writeln!(
                formatter,
                "  SOC RANGE  child={} -> parent={} size={:#x}",
                translation.child(),
                translation.parent(),
                translation.size()
            )?;
        }
        for cpu in &self.cpus {
            writeln!(
                formatter,
                "  CPU        affinity={:#x} status={:?} enable={:?} current={}",
                cpu.affinity, cpu.status, cpu.enable_method, cpu.is_current
            )?;
        }
        if let Some(console) = self.console {
            writeln!(
                formatter,
                "  CONSOLE    {} {:?}",
                console.registers, console.kind
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;
    use std::vec::Vec;

    fn region(start: u64, size: u64) -> PhysRegion {
        PhysRegion::new(PhysAddr::new(start), size).unwrap()
    }

    #[test]
    fn constructs_regions_at_low_and_high_addresses() {
        let low = region(0x1000, 0x2000);
        let high = region(0x1_0000_0000, 0x4000);

        assert_eq!(low.end(), PhysAddr::new(0x3000));
        assert_eq!(high.end(), PhysAddr::new(0x1_0000_4000));
    }

    #[test]
    fn rejects_invalid_regions() {
        assert_eq!(
            PhysRegion::new(PhysAddr::new(0x1000), 0),
            Err(RegionError::Empty)
        );
        assert_eq!(
            PhysRegion::from_bounds(PhysAddr::new(0x2000), PhysAddr::new(0x1000)),
            Err(RegionError::EndBeforeStart)
        );
        assert_eq!(
            PhysRegion::from_bounds(PhysAddr::new(0x1000), PhysAddr::new(0x1000)),
            Err(RegionError::Empty)
        );
        assert_eq!(
            PhysRegion::new(PhysAddr::new(u64::MAX), 1),
            Err(RegionError::AddressOverflow)
        );
    }

    #[test]
    fn checks_address_and_region_containment_at_boundaries() {
        let outer = region(0x1000, 0x2000);
        let inner = region(0x1800, 0x800);

        assert!(outer.contains_address(PhysAddr::new(0x1000)));
        assert!(outer.contains_address(PhysAddr::new(0x2fff)));
        assert!(!outer.contains_address(PhysAddr::new(0x3000)));
        assert!(outer.contains_region(inner));
        assert!(outer.contains_region(outer));
        assert!(!inner.contains_region(outer));
    }

    #[test]
    fn distinguishes_overlap_and_adjacency() {
        let base = region(0x1000, 0x1000);
        let disjoint = region(0x3000, 0x1000);
        let adjacent = region(0x2000, 0x1000);
        let partial = region(0x1800, 0x1000);
        let contained = region(0x1400, 0x100);

        assert!(!base.overlaps(disjoint));
        assert!(!base.is_adjacent(disjoint));
        assert!(!base.overlaps(adjacent));
        assert!(base.is_adjacent(adjacent));
        assert!(base.overlaps(partial));
        assert!(partial.overlaps(base));
        assert!(base.overlaps(contained));
    }

    #[test]
    fn checked_address_addition_detects_overflow() {
        assert_eq!(
            PhysAddr::new(0x1000).checked_add(0x20),
            Some(PhysAddr::new(0x1020))
        );
        assert_eq!(PhysAddr::new(u64::MAX).checked_add(1), None);
    }

    #[test]
    fn fixed_list_preserves_insertion_order_and_capacity() {
        let mut list = FixedList::<u32, 2>::new();
        assert!(list.is_empty());
        assert_eq!(list.capacity(), 2);

        list.push(10).unwrap();
        list.push(20).unwrap();
        assert!(list.is_full());
        assert_eq!(list.len(), 2);
        assert_eq!(list.get(0), Some(&10));
        assert_eq!(list.get(1), Some(&20));
        assert_eq!(list.get(2), None);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), [10, 20]);
        assert_eq!((&list).into_iter().copied().collect::<Vec<_>>(), [10, 20]);

        let error = list.push(30).unwrap_err();
        assert_eq!(error.capacity(), 2);
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), [10, 20]);
    }

    #[test]
    fn zero_capacity_list_is_consistently_full() {
        let mut list = FixedList::<u32, 0>::new();
        assert!(list.is_empty());
        assert!(list.is_full());
        assert_eq!(list.push(1), Err(CapacityError { capacity: 0 }));
        assert_eq!(list.iter().len(), 0);
    }

    #[test]
    fn validates_dynamic_reservations() {
        let attributes = ReservationAttributes {
            no_map: true,
            reusable: false,
        };
        let reservation = DynamicReservation::new(
            0x4000,
            Some(0x1000),
            ReservationOrigin::ReservedMemoryNode,
            ReservationOwner::HostPolicy,
            attributes,
        )
        .unwrap();

        assert_eq!(reservation.size(), 0x4000);
        assert_eq!(reservation.alignment(), Some(0x1000));
        assert_eq!(reservation.origin(), ReservationOrigin::ReservedMemoryNode);
        assert_eq!(reservation.owner(), ReservationOwner::HostPolicy);
        assert_eq!(reservation.attributes(), attributes);
        assert!(reservation.alloc_ranges().is_empty());
        assert_eq!(
            DynamicReservation::new(
                0,
                None,
                ReservationOrigin::Unknown,
                ReservationOwner::Unknown,
                attributes
            ),
            Err(DynamicReservationError::Empty)
        );
        assert_eq!(
            DynamicReservation::new(
                0x1000,
                Some(0),
                ReservationOrigin::Unknown,
                ReservationOwner::Unknown,
                attributes
            ),
            Err(DynamicReservationError::InvalidAlignment)
        );
        assert_eq!(
            DynamicReservation::new(
                0x1000,
                Some(3),
                ReservationOrigin::Unknown,
                ReservationOwner::Unknown,
                attributes
            ),
            Err(DynamicReservationError::InvalidAlignment)
        );
    }

    #[test]
    fn constructs_all_record_kinds_and_system_info() {
        let ram = RamRegion {
            region: region(0, 0x1000),
            source: RamSource::DeviceTree,
        };
        let reserved = ReservedRegion {
            region: region(0x1000, 0x1000),
            origin: ReservationOrigin::Dtb,
            owner: ReservationOwner::Bootloader,
            attributes: ReservationAttributes::default(),
        };
        let dynamic = DynamicReservation::new(
            0x2000,
            None,
            ReservationOrigin::ReservedMemoryNode,
            ReservationOwner::HostPolicy,
            ReservationAttributes {
                no_map: false,
                reusable: true,
            },
        )
        .unwrap();
        let mmio = MmioRegion {
            region: region(0xfe00_0000, 0x1000),
            kind: MmioKind::Device,
        };
        let translation = BusTranslation::new(
            PhysAddr::new(0x7e00_0000),
            PhysAddr::new(0xfe00_0000),
            0x0180_0000,
        )
        .unwrap();
        let cpu = CpuInfo {
            affinity: 0,
            status: CpuStatus::Enabled,
            enable_method: CpuEnableMethod::Psci,
            is_current: true,
        };
        let console = ConsoleInfo {
            kind: ConsoleKind::MiniUart,
            registers: region(0xfe21_5040, 0x40),
        };

        let mut info = SystemInfo::new();
        info.add_ram(ram).unwrap();
        info.add_reserved(reserved).unwrap();
        info.add_dynamic_reserved(dynamic).unwrap();
        info.add_mmio(mmio).unwrap();
        info.add_bus_translation(translation).unwrap();
        info.add_cpu(cpu).unwrap();
        info.set_console(console);

        assert_eq!(info.ram().get(0), Some(&ram));
        assert_eq!(info.reserved().get(0), Some(&reserved));
        assert_eq!(info.dynamic_reserved().get(0), Some(&dynamic));
        assert_eq!(info.mmio().get(0), Some(&mmio));
        assert_eq!(info.bus_translations().get(0), Some(&translation));
        assert_eq!(info.cpus().get(0), Some(&cpu));
        assert_eq!(info.console(), Some(console));
    }

    #[test]
    fn formats_addresses_regions_and_errors_stably() {
        assert_eq!(
            format!("{}", PhysAddr::new(0x0400_0000)),
            "0x0000000004000000"
        );
        assert_eq!(
            format!("{}", region(0x0400_0000, 0x20_0000)),
            "[0x0000000004000000, 0x0000000004200000)"
        );
        assert_eq!(
            format!("{}", RegionError::AddressOverflow),
            "physical region end overflows"
        );
        assert_eq!(
            format!("{}", CapacityError { capacity: 4 }),
            "fixed-capacity list is full (capacity 4)"
        );
    }
}
