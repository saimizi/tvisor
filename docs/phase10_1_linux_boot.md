# Phase 10.1: Linux boot ABI and guest memory layout

This checkpoint defines the Linux-facing contract without changing the Phase 9
test payload. Until an explicit Linux `Image` source and loader are added,
`run_guest()` remains the Phase 9 verification path.

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

The future image-loader change must allocate the entire RAM range as normal
guest RAM, copy and cache-publish the Image, write the generated DTB after the
header-declared Image extent, and map only that RAM through stage 2. The DTB
must advertise the contiguous IPA RAM region and no host physical address.
