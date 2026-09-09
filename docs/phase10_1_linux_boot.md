# Phase 10.1: Linux boot ABI and guest memory layout

This checkpoint defines the Linux-facing contract without changing the Phase 9
test payload. Until an explicit Linux `Image` source and loader are added,
`run_guest()` remains the Phase 9 verification path.

The initial one-vCPU Linux VM has one contiguous 512 MiB guest-RAM region:

| Guest IPA range | Purpose |
|---|---|
| `0x4000_0000..0x4020_0000` | Generated DTB reservation (maximum 2 MiB) |
| `0x4020_0000..` | Linux `Image`, 2-MiB aligned |
| after rounded Image | Optional initrd, page aligned |
| remainder through `0x6000_0000` | Guest RAM available to Linux |

`tvisor_util::linux_boot::LinuxBootLayout` validates all placement before page
allocation. Its values are IPAs only: `VmCtl` remains the sole authority that
chooses the backing physical pages and installs stage-2 mappings.

At entry tvisor must return to EL1 in AArch64 with:

```text
PC = Linux Image IPA
x0 = guest DTB IPA
x1 = x2 = x3 = 0
```

`Vcpu::new_linux()` records precisely that state. `SP_EL1` begins as zero;
Linux owns stack setup and all EL1 stage-1 translation-register initialization.
No host address, Phase 9 stack address, or HVC checkpoint value is part of this
ABI.

The future image-loader change must allocate the entire RAM range as normal
guest RAM, copy and cache-publish the Image, write the generated DTB into its
reserved window, and map only that RAM through stage 2. The DTB must advertise
the contiguous IPA RAM region and no host physical address.
