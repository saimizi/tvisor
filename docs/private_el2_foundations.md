# Phase 6: private EL2 foundations

## 1. Purpose

Phase 6 gives tvisor a private stack and exception-recovery path before it
replaces U-Boot's EL2 translation tables. It does not switch page tables.

The existing diagnostic remains a returnable U-Boot application. A new,
explicit takeover entry mode changes to tvisor-owned state and never returns
to U-Boot. Keeping these paths separate prevents an accidental return through
an abandoned U-Boot stack or vector environment.

## 2. Entry modes

```text
main
  |
  +-- diagnostic-return (default)
  |     preserve U-Boot registers and stack
  |     collect and print diagnostics
  |     restore state and return an rc
  |
  `-- takeover-no-return (explicit argument)
        validate and copy handoff information
        mask asynchronous exceptions
        select SP_EL2 and tvisor boot stack
        install tvisor VBAR_EL2
        continue on private foundations
        never return to U-Boot
```

The takeover mode must require an unambiguous tagged argument. Missing or
unknown mode information continues to select the safe diagnostic path during
development.

## 3. Private boot stack

The first implementation reserves one statically linked boot stack so it is
available before a page allocator exists:

- size: 64 KiB;
- alignment: 16 bytes, with page-aligned section boundaries;
- direction: grows downward from `__boot_stack_top`;
- use: boot CPU only, until per-CPU stacks exist;
- attributes in Phase 7: Normal WB/WA, read/write, execute-never.

The linker exports `__boot_stack_bottom` and `__boot_stack_top`. The stack is
part of the tvisor image reservation and therefore cannot overlap U-Boot or
allocator-managed RAM.

Phase 7 places an invalid guard page immediately below the stack. Until tvisor
owns the page tables, the linker can reserve the page but cannot make it
unmapped under U-Boot's translation regime.

## 4. EL2 vector table

The vector table is a 2048-byte-aligned, 2048-byte section containing all 16
AArch64 vector slots. Each slot is exactly 128 bytes and branches to a common
entry after recording a vector identifier:

```text
current EL using SP_EL0: synchronous, IRQ, FIQ, SError
current EL using SP_ELx: synchronous, IRQ, FIQ, SError
lower EL using AArch64:  synchronous, IRQ, FIQ, SError
lower EL using AArch32:  synchronous, IRQ, FIQ, SError
```

`VBAR_EL2` is written only after the table address and alignment have been
validated and the private stack is active. The diagnostic-return path never
changes `VBAR_EL2`.

## 5. Exception-frame ABI

The assembly entry saves a fixed frame on the private stack containing:

- `x0` through `x30`;
- the vector identifier;
- `ELR_EL2`;
- `SPSR_EL2`;
- `ESR_EL2`; and
- `FAR_EL2`.

The frame size is rounded to a multiple of 16 bytes. Rust receives a pointer
to a `#[repr(C)]` structure whose compile-time size and field offsets must
match the assembly constants.

The first handler prints a bounded report through the already initialized
UART and then halts with interrupts masked. It does not attempt recovery.
Phase 6 may later add one deliberate synchronous-exception test with a known
resume address, but generic recovery is out of scope.

## 6. Transition ordering

The no-return path performs:

1. Complete DTB and memory-map discovery while U-Boot state is intact.
2. Verify EL2, little-endian operation, stack/vector alignment, and required
   architectural features.
3. Ensure UART initialization is complete.
4. Execute `msr daifset, #0xf` and `isb`.
5. Set `SPSel.SP = 1` so ordinary `sp` accesses use `SP_EL2`.
6. Load `sp = __boot_stack_top`.
7. Write `VBAR_EL2 = __el2_vectors` and execute `isb`.
8. Call the no-return Rust continuation.

Steps 4–8 are one auditable assembly routine. No compiler-generated stack
access may occur between abandoning U-Boot's stack and selecting the private
stack.

## 7. Linker and mapping requirements

Phase 6 adds:

- `.vectors`, aligned to 2048 bytes and placed in the executable image;
- `.boot_stack_guard`, one page reserved for the future invalid mapping;
- `.boot_stack`, page-aligned and included in the image reservation; and
- exported start/end symbols for validation and Phase 7 mappings.

The vector section is read-only and executable. The stack and guard storage
are NOBITS writable sections. Phase 7 must apply distinct permissions using
page-aligned boundaries.

## 8. Implementation modules

- `src/boot.rs`: entry-mode selection and stack/vector transition assembly;
- `src/exception.rs`: vector assembly, exception frame, and bounded reporter;
- `scripts/rpi.ld`: new sections and boundary symbols;
- `src/main.rs`: shared discovery followed by mode dispatch; and
- `tvisor_util/aarch64_reg.rs`: only reusable typed register operations.

## 9. Verification sequence

1. Host tests validate exception-frame size/alignment and mode parsing.
2. The AArch64 build verifies vector/stack section alignment with `readelf`
   and confirms the stack-switch sequence with `objdump`.
3. Raspberry Pi testing first reruns the unchanged diagnostic-return path.
4. After a fresh reset, explicit takeover mode prints checkpoints from the
   private stack and verifies `SP`, `SPSel`, and `VBAR_EL2`.
5. A deliberate exception verifies entry into the private vector handler.

Takeover testing is expected to halt rather than return. Smart-plug power
recovery must be available before the first run.

## 10. Phase boundary

Phase 6 finishes when tvisor can run and report an exception using only its
own stack and vector table. It still uses U-Boot's EL2 page tables. Phase 7
then maps these exact foundations and replaces the inherited translation
regime.

