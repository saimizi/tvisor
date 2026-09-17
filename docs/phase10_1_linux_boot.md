# Phase 10.1: Linux boot ABI and guest memory layout

This checkpoint defines the Linux-facing contract and loads a separately
staged Linux `Image` into the guest. U-Boot owns downloading the Image; tvisor
copies it into allocator-owned guest RAM before entering EL1.

The initial one-vCPU Linux VM has one contiguous 512 MiB guest-RAM region:

| Guest IPA range | Purpose |
|---|---|
| `0x4000_0000..0x6000_0000` | One contiguous 512 MiB guest-RAM region |
| `2 MiB-aligned RAM base + Image text_offset` | Linux `Image` entry and contents |
| immediately after declared Image extent | Generated DTB reservation (maximum 2 MiB) |
| after the DTB window | Optional initrd, page aligned |

`tvisor_util::linux_boot::LinuxImageHeader` parses the standard 64-byte arm64
Linux `Image` header. `LinuxBootLayout` uses its little-endian `text_offset`,
`image_size`, and `flags` to validate placement before page allocation. Its
values are IPAs only: `VmCtl` remains the sole authority that chooses the
backing physical pages and installs stage-2 mappings.

At entry tvisor must return to EL1 in AArch64 with:

```text
PC = 2 MiB-aligned guest-RAM base + Image text_offset
x0 = guest DTB IPA
x1 = x2 = x3 = 0
```

`Vcpu::new_linux()` records precisely that state. `SP_EL1` begins as zero;
Linux owns stack setup and all EL1 stage-1 translation-register initialization.
No host address, Phase 9 stack address, or HVC checkpoint value is part of this
ABI.

The image loader allocates the entire RAM range as normal guest RAM, copies and
cache-publishes the Image, writes the generated DTB after the header-declared
Image extent, and maps only that RAM through stage 2. The DTB advertises the
contiguous IPA RAM region and no host physical address.

## U-Boot handoff

Load tvisor and the guest kernel at distinct RAM addresses, then pass both the
guest Image address and its exact TFTP byte count. The Image header describes
the runtime extent but may omit a zero-initialized tail, so `image=` carries
the file length required for a safe copy.

```text
tftpboot ${tvisor_addr_r} tvisor
tftpboot ${kernel_addr_r} Image
go ${tvisor_entry} fdt=${fdt_addr} image=${kernel_addr_r},${filesize}
```

`kernel_addr_r` must not overlap tvisor, the live DTB, or U-Boot runtime
memory. Tvisor reserves the page-rounded source range through its private EL2
transition, copies the exact file bytes, and zero-fills through the Image
header's `image_size` extent.
