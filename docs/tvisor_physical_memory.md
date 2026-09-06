# Tvisor physical memory map on Raspberry Pi 4

| Address Range 1 | Address Range 2 | Size | Classification | Usage | Note |
| --- | --- | ---: | --- | --- | --- |
| `0x0000_0000–0x3800_0000` | `0x0000_0000–0x0000_1000` | 4 KiB | Reserved | DTB-reserved low page | `0x0000_0000` is the beginning of the first DTB RAM bank; this page is excluded permanently |
|  | `0x0000_1000–0x0400_0000` | 65,532 KiB | RAM | Unassigned low RAM | Inside the dynamic reservation's allocation window, so the current conservative allocator does not use it |
|  | `0x0400_0000–0x0400_1000` | 4 KiB | Tvisor image | ELF headers and alignment | Also contained in the raw ELF downloaded by U-Boot |
|  | `0x0400_1000–0x0404_d000` | 304 KiB | Tvisor image | Executable `.text` | Read-only and executable under the EL2 table policy |
|  | `0x0404_d000–0x0404_e000` | 4 KiB | Tvisor payload | Controlled EL1 test payload | Read-only in EL2; copied into guest backing RAM before execution |
|  | `0x0404_e000–0x0404_f000` | 4 KiB | Tvisor image | EL2 exception-vector page | First 2 KiB contains the 16 vector slots; `VBAR_EL2 = 0x0404_e000` |
|  | `0x0404_f000–0x0406_e000` | 124 KiB | Tvisor image | `.rodata` and unwind data | Read-only and execute-never under the EL2 table policy |
|  | `0x0406_e000–0x040b_1000` | 268 KiB | Tvisor image | `.data`, `.got`, and `.bss` | Includes allocator bitmaps and pending post-switch memory-map storage |
|  | `0x040b_1000–0x040b_2000` | 4 KiB | Tvisor guard | Unmapped boot-stack guard | Deliberately has no EL2 descriptor |
|  | `0x040b_2000–0x040c_2000` | 64 KiB | Tvisor stack | Private EL2 boot stack | Initial `SP` is `0x040c_2000`; the stack grows downward |
|  | `0x040c_2000–0x040d_2000` | 64 KiB | Tvisor page tables | Linker-owned bootstrap-table arena | `NOLOAD`, explicitly zeroed before use, Normal RW/XN, and never allocator-managed |
|  | `0x040d_2000–0x2eff_1000` | ≈683 MiB | RAM | Unassigned low RAM | Below `0x3000_0000`, so currently excluded by the conservative dynamic-reservation policy |
|  | `0x2eff_1000–0x2f00_0000` | 60 KiB | Tvisor reservation | Pages containing the live DTB | Identity-mapped read-only/XN and retained without copying |
|  | `0x2f00_0000–0x3000_0000` | 16 MiB | RAM | Unassigned low RAM | Currently excluded by the conservative dynamic-reservation policy |
|  | `0x3000_0000–0x3800_0000` | 128 MiB | Allocator-managed RAM | Low-bank physical pages | Allocator is initialized only after takeover; first-fit begins at `0x3000_0000` |
| `0x3800_0000–0x4000_0000` | `0x3800_0000–0x3ef6_6280` | ≈111.399 MiB | Firmware carve-out | Firmware/GPU-owned memory | Not allocatable ARM RAM |
|  | `0x3ef6_6280–0x3ef6_6670` | ≈0.984 KiB | Reserved firmware | Permanent DTB `no-map` reservation | Also present in U-Boot's LMB list |
|  | `0x3ef6_6670–0x4000_0000` | ≈16.600 MiB | Firmware carve-out | Firmware/GPU-owned memory | Not allocatable ARM RAM |
| `0x4000_0000–0xfc00_0000` | `0x4000_0000–0xfc00_0000` | 3008 MiB | Allocator-managed RAM | Second DTB RAM bank | Included directly after takeover and identity-mapped as Normal RW/XN RAM |
| `0xfc00_0000–0x1_0000_0000` | `0xfc00_0000–0xfe00_0000` | 32 MiB | MMIO | BCM2711 peripheral window | Translated from `/soc` child range `0x7c00_0000–0x7e00_0000` |
|  | `0xfe00_0000–0xff80_0000` | 24 MiB | MMIO | BCM2711 main peripheral window | Contains GPIO, PL011, AUX, and mini UART; console registers are `0xfe21_5040–0xfe21_5080` |
|  | `0xff80_0000–0x1_0000_0000` | 8 MiB | MMIO | ARM-local peripheral window | Contains the GIC region |
