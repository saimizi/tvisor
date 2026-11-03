# VMs, vCPUs and the Linux guest platform

The current runtime creates one VM (VMID 1), with one vCPU executing on physical
CPU0. Other physical CPUs are host workers. The generated guest DTB exposes one
CPU; guest CPU_ON, virtual IPIs, scheduling and migration are not implemented.

## VM and vCPU ownership

[`VmCtl`](../../src/vmctl.rs) owns the VM ID, vCPU collection, guest backing
allocations and stage-2 table allocations. Guest IPA region types are distinct
from host PA types. VM memory records distinguish IPA-backed allocation,
IPA-only holes and physical-only allocations such as translation tables.
Allocation budgets include 512 MiB RAM plus room for stage-2 tables.

[`Vcpu`](../../src/vcpu.rs) owns guest architectural context, exit information,
virtual timer state and virtual GIC state. Host `PcpuContext` instead holds the
owner CPU's saved host SP and transient active-vCPU pointer. These fields do
not transfer VM ownership to a physical CPU.

The Linux run path creates one `MmioDispatcher` for its virtual distributor
and PL011 and passes it through the guest loop. It is currently local to that
run, not a general multi-vCPU device-ownership model. The host GIC/UART remain
host-owned. Mapping the hardware virtual CPU interface does not make those
physical pages VM allocations.

On a returned guest run, the caller deactivates stage 2, releases VM allocations
and checks the physical allocator baseline. This does not establish generalized
interrupt/device teardown for arbitrary guests; see [remaining IRQ work](interrupts.md#remaining-work).

## Guest address space

[guest_platform.rs](../../tvisor_util/guest_platform.rs) defines the guest IPA
ABI. Runtime placement of its backing physical pages belongs to VM management.

| Guest IPA range | Purpose and implementation |
|---|---|
| `[0x40000000, 0x60000000)` | Contiguous 512 MiB guest RAM; allocator-owned backing |
| `[0x08000000, 0x08010000)` | Trapped minimal virtual GICv2 distributor |
| `[0x08010000, 0x08012000)` | Device mapping of host GICV, including GICV_DIR page |
| `[0x09000000, 0x09001000)` | Trapped virtual PL011, virtual SPI 33, 24 MHz clock |

The host distributor, UART and firmware RAM are not guest passthrough mappings.
GICV mapping preserves the host region's page offset; generated DT values use
the exact device offset/extent. Guest RAM is currently Normal WB/WA, RW and
executable at stage 2; Linux supplies its own finer stage-1 permissions.
The 39-bit IPA, 4 KiB stage-2 regime starts at L1 and the current VM mapper
installs L3 RAM pages and subordinate tables.

```text
Linux virtual address → guest EL1 stage 1 → guest IPA → EL2 stage 2 → host PA
EL2 virtual address   → host EL2 stage 1 → host PA
```

Host mappings and guest stage 2 are independent translation regimes. Linux
changing its own page tables does not change host tables or grant access beyond
stage 2. No cross-CPU guest stage-2 update protocol is implemented yet.

## Linux Image and entry ABI

[`LinuxImageHeader`](../../tvisor_util/linux_boot.rs) parses the 64-byte arm64
Image header and validates little-endian flags, magic, text offset and declared
runtime extent. `LinuxBootLayout` checks guest placement before allocation.
Entry is the 2 MiB-aligned guest RAM base plus the header's `text_offset`.
The generated DTB occupies a maximum 2 MiB reservation immediately after the
declared Image extent. The layout helper can describe an optional page-aligned
initrd, but the current loader uses the kernel's embedded initramfs and does
not load a separate initrd.

U-Boot passes the exact Image file size separately from the header's runtime
size. The loader copies those file bytes, zero-fills the remaining declared
extent and generates the guest DTB in VM-owned RAM. It publishes Image/DTB cache
lines and invalidates instruction cache before guest entry. Source pages stay
reserved in the host memory map.

At Linux entry, PC is the computed Image entry, x0 is the guest DTB IPA and
x1–x3 are zero. SP_EL1 starts at zero; Linux establishes its own stack and EL1
translation regime. No host address or old test-payload stack is part of this ABI.
The [guest DTB builder](../../tvisor_util/guest_fdt.rs) supplies one CPU, guest RAM,
GICv2, timer, PL011, clock and console boot arguments. It does not pass the host
tree or advertise physical worker CPUs.

## World-switch state

`VcpuContext` is 896 bytes, aligned to 16. It preserves x0–x30, SP_EL0/SP_EL1,
ELR_EL2/SPSR_EL2, SCTLR/CPACR/TTBR0/TTBR1/TCR/MAIR/VBAR/CONTEXTIDR_EL1,
TPIDR_EL1/TPIDRRO_EL0, Q0–Q31 and FPCR/FPSR. This includes Linux current-task and
thread-pointer state across IRQs and emulated MMIO exits.

`Vcpu` places context at offset 0, exit at 896, timer at 928 and GIC state at
952; its total size is 976 bytes. The assembly uses the physical CPU's host SP
and active-vCPU fields at offsets 0/8, never the guest's stack for EL2 execution.
Layout assertions and [linked checks](../testing/host-tests.md#linked-image-checks)
protect these interfaces. Exit records capture vector, ESR, FAR and HPFAR;
stage-2 fault IPA combines HPFAR's page with FAR's page offset.

## Stage-2 activation and deactivation

[Activation](../../src/guest.rs) publishes descriptors with DSB ISHST, installs
VTCR/VTTBR/CPTR and guest VMPIDR while VM is clear, synchronizes, then performs
`TLBI VMALLS12E1IS; DSB ISH; ISB` with the selected guest VMID installed. Only
then is the full [guest HCR profile](interrupts.md#explicit-el2-control-profiles)
enabled and checked by readback.

Normal emulated/IRQ exits retain that context for resumption. Full deactivation,
with IRQs masked, invalidates while the guest VMID is still installed, completes
DSB/ISB, restores the complete host HCR and clears VTTBR with synchronization.
Clearing VTTBR first would target the wrong VMID. HCR alone is not teardown.

## Virtual devices and interrupts

PL011 and distributor IPAs are intentionally absent from stage-2 mappings.
Accesses trap as lower-EL data aborts. [MMIO decoding](../../tvisor_util/mmio.rs)
checks syndrome validity, direction, width and register encoding. Successful
emulation writes the read result and advances guest PC by four; unsupported
access/decode failures do not count as completed instructions. Unmapped IPAs
outside supported virtual-device windows terminate the run.

The PL011 supports console TX, identification/configuration registers, a bounded
RX FIFO and receive mask/pending state. TX bytes go to the serialized host
console. Physical UART RX feeds the FIFO; its pending state requests a software
virtual SPI through LR1. This is a limited console model: several registers
are read-as-zero/write-ignored and full hardware behavior is not implemented.

The minimal virtual distributor exposes Linux's discovery/configuration view
for one vCPU. It retains CTLR but many register banks are reset-value or
read-as-zero/write-ignored. It does not emulate general virtual SGIs or multiple
CPU targeting. Linux accesses the hardware GICV CPU interface directly; GICH
VMCR, LR0 and LR1 are saved/restored with the vCPU.

Per-vCPU timer state contains CNTVOFF_EL2, CNTV_CVAL_EL0 and CNTV_CTL_EL0.
The initial offset is zero. Expiry is hardware-driven through physical PPI 27,
then injected as a hardware-backed virtual PPI in LR0. No guest-counter polling
is used to infer expiry. [Interrupt ownership](interrupts.md) defines the distinct
timer/UART completion rules and remaining backpressure/teardown work.

For guest build and execution see [build](../setup/build.md#linux-guest) and
[Raspberry Pi setup](../setup/raspberry-pi4.md). The Linux boot contract is based
on the [arm64 boot protocol](https://docs.kernel.org/arch/arm64/booting.html).
