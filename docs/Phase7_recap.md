# Phase 7 recap: tvisor-owned EL2 stage-1 translation

## 1. Where Phase 7 starts

U-Boot hands control to tvisor at EL2 with its own EL2 stage-1 translation
regime active. Phase 6 established the private execution foundations needed to
leave that environment safely:

- a tvisor-owned `SP_EL2` stack;
- a tvisor-owned vector table in `VBAR_EL2`;
- masked asynchronous exceptions; and
- a synchronous exception path that can report faults and return from the
  deliberate `BRK #0x600` test.

Phase 7 keeps those foundations and replaces only the inherited EL2 stage-1
translation regime. It does not enable guest stage-2 translation, start an
allocator, reclaim U-Boot memory, or return to U-Boot after the switch.

## 2. Goal

Build, validate, and install tvisor-owned EL2 stage-1 page tables without
losing:

- instruction fetch from tvisor text and vectors;
- access to read-only and writable image data;
- access to the private stack;
- access to the page tables themselves;
- UART output; or
- EL2 synchronous exception handling.

The first map is deliberately an identity map: every mapped virtual address is
equal to its physical address. This makes the transition code valid under both
U-Boot's old tables and tvisor's new tables.

## 3. Preconditions checked before takeover

The U-Boot handoff path prepares the transition and halts before the no-return
boundary unless:

- `CurrentEL` is EL2;
- execution is little-endian;
- `HCR_EL2.VM == 0`, so guest stage-2 translation is disabled;
- `HCR_EL2.E2H == 0`, so the non-VHE EL2 register and descriptor model applies;
- `ID_AA64MMFR0_EL1.TGran4` reports 4 KiB granule support;
- `ID_AA64MMFR0_EL1.PARange` represents every mapped physical address;
- a UART physical address was discovered from the DTB;
- a 64 KiB bootstrap table arena fits in normalized `INITIAL` RAM; and
- every mandatory mapping passes the software table walker.

Failure here prints an error when UART is available and halts. Once the private
EL2 entry is taken, failure is likewise fatal and requires a reset or power
cycle. Tvisor has no path back to U-Boot.

## 4. Translation geometry

Phase 7 uses a 39-bit VA space with 4 KiB granules. Translation starts at L1:

| Level | Entry coverage | Leaf type |
| --- | ---: | --- |
| L1 | 1 GiB | Block |
| L2 | 2 MiB | Block |
| L3 | 4 KiB | Page |

Each table contains 512 eight-byte descriptors and therefore occupies one
4 KiB page. The builder selects the largest valid leaf permitted by alignment,
size, and attribute boundaries rather than assuming a fixed table shape.

The current Raspberry Pi 4 layout uses six pages from the reserved arena at
`0x3000_0000–0x3001_0000`; unused arena pages remain reserved for Phase 7.

## 5. Initial identity mappings

The linker exports page-aligned boundaries so that permissions can be assigned
without sharing a page between incompatible regions.

| Region | Memory type | Write | Execute |
| --- | --- | --- | --- |
| `.text` | Normal WB/WA | No | Yes |
| EL2 vectors | Normal WB/WA | No | Yes |
| `.rodata` and unwind data | Normal WB/WA | No | No |
| `.data`, `.got`, and `.bss` | Normal WB/WA | Yes | No |
| Private EL2 stack | Normal WB/WA | Yes | No |
| Bootstrap table arena | Normal WB/WA | Yes | No |
| UART page | Device-nGnRE | Yes | No |
| Stack guard page | Unmapped | No | No |

Only resources required immediately after the switch are mapped. The original
DTB, U-Boot runtime memory, raw ELF staging tail, general RAM, and unrelated
MMIO are not part of this bootstrap map.

## 6. Descriptor policy

Every valid leaf sets the access flag. Normal memory is Inner Shareable and
uses MAIR attribute index 0; UART Device-nGnRE memory uses index 1 and is
Non-shareable. Read-only mappings set the stage-1 access-permission field.

For the non-VHE EL2 stage-1 regime, descriptor bit 54 is `XN`. It is clear only
for tvisor text and vectors and set for all other leaves. The EL0/EL1 regime's
`PXN` interpretation must not be used to determine EL2 execution permission.

The software walker independently verifies the translated PA, memory type,
write permission, and execute permission. It also proves that both ends of the
guard page have no valid translation.

## 7. Constructed register values

The implementation constructs new values instead of copying U-Boot's
translation registers:

```text
MAIR_EL2 = 0x0000_0000_0000_04ff
  Attr0 = 0xff  Normal WB/WA
  Attr1 = 0x04  Device-nGnRE

TCR_EL2 = 0x0000_0000_8084_3519 on the tested Raspberry Pi 4
  RES1  = bits 31 and 23
  T0SZ  = 25       39-bit VA
  TG0   = 0b00     4 KiB
  SH0   = 0b11     Inner Shareable
  IRGN0 = 0b01     WB/WA
  ORGN0 = 0b01     WB/WA
  PS    = PARange  44-bit PA on the tested board

TTBR0_EL2 = 0x0000_0000_3000_0000
  BADDR = bootstrap L1 root
  CnP   = 0

SCTLR_EL2 = 0x0000_0000_30cd_183d
  required RES1 policy plus M, C, SA, I, and WXN
  EE remains clear
```

The exact `TCR_EL2.PS` and therefore the complete TCR value are derived from
the processor's reported `PARange`; the values above record the tested
Cortex-A72 board.

## 8. Transition sequence

The no-return path first installs the private Phase 6 stack and vectors. A
small leaf assembly routine then receives all new register values in general
registers and performs the critical transition without stack access, literal
loads, calls, or `ret`:

```text
publish completed table stores with DSB ISHST
enter private SP_EL2 and VBAR_EL2; mask DAIF
DSB SY
clear SCTLR_EL2.M
ISB
write MAIR_EL2, TCR_EL2, and TTBR0_EL2
ISB
TLBI ALLE2
DSB SY
ISB
write the final SCTLR_EL2 value
ISB
branch directly to the identity-mapped post-switch checkpoint
```

Data and instruction caches stay enabled during the short MMU-disabled
interval. The old and new regimes use compatible cacheability for the touched
RAM and introduce no VA aliases.

`TLBI ALLE2` is suitable for the current single active tvisor PE. SMP support
will require a separate shareability and TLB-maintenance design.

## 9. Post-switch validation

The first Rust checkpoint under tvisor's tables:

1. writes a fixed message through the DTB-discovered UART page;
2. reads back `MAIR_EL2`, `TCR_EL2`, `TTBR0_EL2`, and `SCTLR_EL2`;
3. compares each value with the constructed value;
4. verifies a stack-local canary;
5. reads a canary from read-only image data;
6. writes and reads a canary in writable image data; and
7. optionally invokes one explicit exception or translation-fault test.

Success ends in a `WFE` loop. There is intentionally no path back to U-Boot.

## 10. Runtime test arguments

Tvisor always takes over EL2 and installs its page tables. The parser accepts
only an optional post-switch fault test:

| Argument | Meaning |
| --- | --- |
| `fault=none` | Do not trigger a deliberate fault |
| `fault=sync` | Execute `BRK #0x600`; the handler advances `ELR_EL2` and returns with `eret` |
| `fault=guard` | Write to the unmapped stack-guard page; report and halt |
| `fault=unmapped` | Read VA `0x2000_0000`; report and halt |

Each run, including a run without a deliberate fault, must begin from a fresh
board boot.

## 11. Hardware results

The positive Raspberry Pi 4 test completed with UART active under the new
tables and printed:

```text
Phase 7 checkpoint 2: tvisor EL2 page tables active
  MAIR_EL2=0x00000000000004ff
   TCR_EL2=0x0000000080843519
 TTBR0_EL2=0x0000000030000000
 SCTLR_EL2=0x0000000030cd183d
Phase 7 checkpoint 3: register, stack, and image validation passed
Returned from deliberate synchronous exception under tvisor tables
Phase 7 checkpoint complete; halting
```

The guard-page test entered tvisor's current-EL/SPx synchronous vector and
reported:

```text
ESR_EL2=0x0000000096000047
FAR_EL2=0x0000000004062000
Unexpected exception; halting
```

This confirms that the guard page is absent from the active tables and that
the private vector, private stack, exception frame, UART mapping, and fault
reporting remain usable after the switch.

## 12. Suggested review order

1. `scripts/rpi.ld`: verify page boundaries and section ordering.
2. `tvisor_util/el2_translation.rs`: review descriptor bits, table allocation,
   mapping conflict rules, walker, and constructed register values.
3. `src/mm.rs`: review arena selection, mandatory mappings, guard omission,
   validation, and table-publication barrier.
4. `tvisor_util/boot_mode.rs`: review the explicit switch and fault-test gates.
5. `src/main.rs`: review all preconditions before entering the no-return path.
6. `src/boot.rs`: review private entry, the assembly-only critical interval,
   post-switch readback, canaries, and fault triggers.
7. `src/exception.rs`: confirm that only the recognized `BRK #0x600` resumes
   and all translation faults report and halt.
8. `docs/tvisor_physical_memory.md`: compare the documented implemented
   regions with the current linker symbols.

The most safety-sensitive review points are EL2 `XN` encoding, architectural
RES1 bits, Normal-versus-Device attributes, identity coverage of every address
touched by the transition, the unmapped guard, and the barrier/TLB sequence.
