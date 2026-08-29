# Tvisor physical memory map on Raspberry Pi 4

| Address Range 1 | Address Range 2 | Size | Classification | Usage | Note |
| --- | --- | ---: | --- | --- | --- |
| `0x0000_0000–0x3800_0000` | `0x0000_0000–0x0000_1000` | 4 KiB | Reserved | DTB-reserved low page | `0x0000_0000` is the beginning of the first DTB RAM bank; this page is excluded permanently |
|  | `0x0000_1000–0x0400_0000` | 65,532 KiB | RAM | Unassigned low RAM | Inside the dynamic reservation's allocation window, so the current conservative allocator does not use it |
|  | `0x0400_0000–0x0400_1000` | 4 KiB | Tvisor image | ELF headers and alignment | Also contained in the raw ELF downloaded by U-Boot |
|  | `0x0400_1000–0x0404_4000` | 268 KiB | Tvisor image | Executable `.text` | Read-only and executable under the Phase 7 table policy |
|  | `0x0404_4000–0x0404_5000` | 4 KiB | Tvisor image | EL2 exception-vector page | First 2 KiB contains the 16 vector slots; `VBAR_EL2 = 0x0404_4000` |
|  | `0x0404_5000–0x0405_f000` | 104 KiB | Tvisor image | `.rodata` and unwind data | Read-only and execute-never under the Phase 7 table policy |
|  | `0x0405_f000–0x0406_0000` | 4 KiB | Tvisor image | `.data`, `.got`, and `.bss` | Read/write and execute-never under the Phase 7 table policy |
|  | `0x0406_0000–0x0406_1000` | 4 KiB | Tvisor guard | Unmapped boot-stack guard | Deliberately has no Phase 7 descriptor |
|  | `0x0406_1000–0x0407_1000` | 64 KiB | Tvisor stack | Private EL2 boot stack | Initial `SP` is `0x0407_1000`; the stack grows downward |
|  | `0x0407_1000–0x0430_f690` | ≈2.619 MiB | U-Boot handoff | Raw ELF staging bytes outside the tvisor runtime image | Reclaimable after no-return takeover; the end changes with `${filesize}` |
|  | `0x0430_f690–0x2eff_1000` | ≈684.882 MiB | RAM | Unassigned low RAM | The portion below `0x3000_0000` is currently excluded by the conservative dynamic-reservation policy |
|  | `0x2eff_1000–0x2f00_0000` | 60 KiB | U-Boot handoff | Pages containing the live DTB | DTB blob begins at `0x2eff_1f00`; reclaimable after required information is copied |
|  | `0x2f00_0000–0x3000_0000` | 16 MiB | RAM | Unassigned low RAM | Currently excluded by the conservative dynamic-reservation policy |
|  | `0x3000_0000–0x3001_0000` | 64 KiB | Tvisor page tables | Phase 7 bootstrap table arena | Permanently reserved; the builder records the number of pages actually used |
|  | `0x3001_0000–0x36b2_b000` | 109,676 KiB | Initial usable RAM | Remaining bootstrap allocation area | First normalized `INITIAL` region after the Phase 7 reservation |
|  | `0x36b2_b000–0x3800_0000` | 21,332 KiB | U-Boot handoff | U-Boot runtime arena | Includes relocated code, stack, heap, and translation tables; reclaimable after no-return takeover |
| `0x3800_0000–0x4000_0000` | `0x3800_0000–0x3ef6_6280` | ≈111.399 MiB | Firmware carve-out | Firmware/GPU-owned memory | Not allocatable ARM RAM |
|  | `0x3ef6_6280–0x3ef6_6670` | ≈0.984 KiB | Reserved firmware | Permanent DTB `no-map` reservation | Also present in U-Boot's LMB list |
|  | `0x3ef6_6670–0x4000_0000` | ≈16.600 MiB | Firmware carve-out | Firmware/GPU-owned memory | Not allocatable ARM RAM |
| `0x4000_0000–0xfc00_0000` | `0x4000_0000–0xfc00_0000` | 3008 MiB | RAM / U-Boot handoff | Second DTB RAM bank | U-Boot LMB reserves it before takeover; usable afterward subject to permanent reservations |
| `0xfc00_0000–0x1_0000_0000` | `0xfc00_0000–0xfe00_0000` | 32 MiB | MMIO | BCM2711 peripheral window | Translated from `/soc` child range `0x7c00_0000–0x7e00_0000` |
|  | `0xfe00_0000–0xff80_0000` | 24 MiB | MMIO | BCM2711 main peripheral window | Contains GPIO, PL011, AUX, and mini UART; console registers are `0xfe21_5040–0xfe21_5080` |
|  | `0xff80_0000–0x1_0000_0000` | 8 MiB | MMIO | ARM-local peripheral window | Contains the GIC region |
