# Tvisor physical memory map on Raspberry Pi 4

| Address Range 1 | Address Range 2 | Size | Classification | Usage | Note |
| --- | --- | ---: | --- | --- | --- |
| `0x0000_0000–0x3800_0000` | `0x0000_0000–0x0000_1000` | 4 KiB | Reserved | DTB-reserved low page | `0x0000_0000` is the beginning of the first DTB RAM bank; this page is excluded permanently |
|  | `0x0000_1000–0x0400_0000` | 65,532 KiB | RAM | Unassigned low RAM | Inside the dynamic reservation's allocation window, so the current conservative allocator does not use it |
|  | `0x0400_0000–0x0400_1000` | 4 KiB | Tvisor image | ELF headers and alignment | Also contained in the raw ELF downloaded by U-Boot |
|  | `0x0400_1000–0x0405_1000` | 320 KiB | Tvisor image | Executable `.text` | Read-only and executable under the EL2 table policy |
|  | `0x0405_1000–0x0405_2000` | 4 KiB | Tvisor image | EL2 exception-vector page | First 2 KiB contains the 16 vector slots; `VBAR_EL2 = 0x0405_1000` |
|  | `0x0405_2000–0x0407_2000` | 128 KiB | Tvisor image | `.rodata` and unwind data | Read-only and execute-never under the EL2 table policy |
|  | `0x0407_2000–0x040b_3000` | 260 KiB | Tvisor image | `.data`, `.got`, and `.bss` | Includes the two 128 KiB physical-page-allocator bitmaps and global heap wrapper; read/write and execute-never |
|  | `0x040b_3000–0x040b_4000` | 4 KiB | Tvisor guard | Unmapped boot-stack guard | Deliberately has no EL2 descriptor |
|  | `0x040b_4000–0x040c_4000` | 64 KiB | Tvisor stack | Private EL2 boot stack | Initial `SP` is `0x040c_4000`; the stack grows downward |
|  | `0x040c_4000–0x043e_2bf0` | ≈3.120 MiB | U-Boot handoff | Raw ELF staging bytes outside the tvisor runtime image | Reclaimable after no-return takeover; the end changes with `${filesize}` |
|  | `0x043e_2bf0–0x2eff_1000` | ≈684 MiB | RAM | Unassigned low RAM | The portion below `0x3000_0000` is currently excluded by the conservative dynamic-reservation policy |
|  | `0x2eff_1000–0x2f00_0000` | 60 KiB | Retained handoff data | Pages containing the live DTB | Identity-mapped read-only/XN before takeover and retained afterward without copying |
|  | `0x2f00_0000–0x3000_0000` | 16 MiB | RAM | Unassigned low RAM | Currently excluded by the conservative dynamic-reservation policy |
|  | `0x3000_0000–0x3001_0000` | 64 KiB | Tvisor page tables | Allocator-owned EL2 table store | Allocated contiguously from initial `Unused` RAM and retained `InUse` |
|  | `0x3001_0000–0x3011_0000` | 1 MiB | Tvisor Rust heap | Fixed `linked_list_allocator` arena | Allocated after takeover as 256 contiguous pages; identity mapping gives `VA = PA`; pages remain `InUse` |
|  | `0x3011_0000–0x36b2_b000` | 108,652 KiB | Allocator-managed RAM | Low-bank physical pages | First-fit physical-page allocation begins here after heap initialization |
|  | `0x36b2_b000–0x3800_0000` | 21,332 KiB | Reclaimed allocator RAM | Former U-Boot runtime arena | Reclaimed only after the no-return takeover; Phase 8 hardware testing allocates `0x36b2_b000` |
| `0x3800_0000–0x4000_0000` | `0x3800_0000–0x3ef6_6280` | ≈111.399 MiB | Firmware carve-out | Firmware/GPU-owned memory | Not allocatable ARM RAM |
|  | `0x3ef6_6280–0x3ef6_6670` | ≈0.984 KiB | Reserved firmware | Permanent DTB `no-map` reservation | Also present in U-Boot's LMB list |
|  | `0x3ef6_6670–0x4000_0000` | ≈16.600 MiB | Firmware carve-out | Firmware/GPU-owned memory | Not allocatable ARM RAM |
| `0x4000_0000–0xfc00_0000` | `0x4000_0000–0xfc00_0000` | 3008 MiB | Allocator-managed RAM | Second DTB RAM bank | U-Boot LMB reserves it before takeover; Phase 8 reclaims and identity-maps it as Normal RW/XN RAM |
| `0xfc00_0000–0x1_0000_0000` | `0xfc00_0000–0xfe00_0000` | 32 MiB | MMIO | BCM2711 peripheral window | Translated from `/soc` child range `0x7c00_0000–0x7e00_0000` |
|  | `0xfe00_0000–0xff80_0000` | 24 MiB | MMIO | BCM2711 main peripheral window | Contains GPIO, PL011, AUX, and mini UART; console registers are `0xfe21_5040–0xfe21_5080` |
|  | `0xff80_0000–0x1_0000_0000` | 8 MiB | MMIO | ARM-local peripheral window | Contains the GIC region |
