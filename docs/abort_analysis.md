# Worked example: analyzing an AArch64 synchronous abort

This document records how a U-Boot abort seen after returning from tvisor was
analyzed. It is intended as a reusable method for investigating similar
AArch64 exceptions.

## 1. Full error log

The following is the complete abort report captured from U-Boot. Terminal
redraw escape sequences that appeared before the report have been omitted,
but no line from the exception report itself has been removed.

```text
"Synchronous Abort" handler, esr 0x96000004, far 0x6d656d5f637601
elr: 00000000000b6bd4 lr : 00000000000b6e1c (reloc)
elr: 0000000037f83bd4 lr : 0000000037f83e1c
x0 : 006d656d5f637601 x1 : 0000000000000023
x2 : 000060f240041f58 x3 : 5f03c054ffffc1b8
x4 : 0000000000000000 x5 : 0000000037b3a2b2
x6 : 0000000000000046 x7 : 0000000037b3a740
x8 : 0000000000000001 x9 : 00000000ffffffd0
x10: 0000000004017df0 x11: 0000000000000000
x12: 0000000000000035 x13: 0000000000000000
x14: 00000000ffffffff x15: 0000000037b3a2b2
x16: 00000000000000ff x17: 0000000000000000
x18: 00000000000000ff x19: 006d656d5f637601
x20: 0000000000000023 x21: 0000000000000002
x22: 0000000037be9ec0 x23: 0000000000000002
x24: 0000000037fe586c x25: 0000000000000000
x26: 0000000000000000 x27: 0000000000000000
x28: 0000000037b54ee0 x29: 0000000037b3a200

Code: a90153f3 12001c34 aa0003f3 f90013f5 (f9400001)
Resetting CPU ...

resetting ...
```

Just before this report, tvisor had printed that it was restoring U-Boot's
stack pointer and link register and executing `ret`. Therefore, the exception
occurred after control had returned to U-Boot.

## 2. Start with the exception location

`ELR_ELx` contains the address of the instruction that caused a synchronous
exception. U-Boot prints it as `elr`:

```text
elr: 00000000000b6bd4 ... (reloc)
elr: 0000000037f83bd4 ...
```

The first value is the link-time or relocation-relative U-Boot address. The
second is its runtime address. The runtime address `0x37f83bd4` is in relocated
U-Boot, not in tvisor, which was executing near `0x04000000`.

This immediately narrows the problem:

- tvisor successfully executed its return instruction;
- U-Boot resumed execution;
- U-Boot then faulted while using state inherited from the returned program.

The `lr` value is also in relocated U-Boot. It identifies the caller of the
faulting U-Boot function and can be useful when matching the dump to a U-Boot
ELF and map file.

## 3. Decode `ESR_ELx`

The exception syndrome was:

```text
ESR = 0x96000004
```

For an AArch64 exception, first separate the main fields:

- `EC = ESR[31:26] = 0x25`: Data Abort taken without a change in Exception
  Level ("same EL").
- `IL = ESR[25] = 1`: the trapped instruction is 32 bits long.
- `ISS = ESR[24:0] = 0x000004`: details of the Data Abort.

For this Data Abort ISS:

- `DFSC = ISS[5:0] = 0x04`: translation fault, level 0.
- `WnR = ISS[6] = 0`: the failing operation was a read, not a write.
- `S1PTW = ISS[7] = 0`: the fault was not reported as occurring during a
  stage-1 page-table walk caused by a stage-2 translation.
- `FnV = ISS[10] = 0`: `FAR_ELx` is valid for this exception.
- `ISV = ISS[24] = 0`: the instruction-syndrome fields such as access size and
  target register are not valid; inspect the instruction itself instead.

Thus, before looking at any source code, the syndrome says: a 32-bit
instruction in U-Boot tried to read through an address for which translation
failed at level 0.

## 4. Correlate `FAR_ELx`, registers, and the instruction

For a Data Abort, `FAR_ELx` normally records the faulting virtual address:

```text
FAR = 0x006d656d5f637601
x0  = 0x006d656d5f637601
```

The exact equality between `FAR` and `x0` is a strong clue that the faulting
instruction dereferenced `x0`.

U-Boot prints the instruction at `ELR` in parentheses. Disassembling the final
word gives:

```asm
f9400001    ldr x1, [x0]
```

This loads eight bytes from the address in `x0` into `x1`. It agrees with every
part of the syndrome:

- it is a read (`WnR = 0`);
- its memory operand uses `x0`;
- `x0` equals `FAR`;
- that nonsensical address has no valid translation, producing the level-0
  translation fault.

The preceding words provide useful history. In particular:

```asm
aa0003f3    mov x19, x0
...
f9400001    ldr x1, [x0]     // fault
```

This explains why both `x0` and `x19` contain the bad address. U-Boot copied
`x0` into `x19` immediately before the fault. Therefore, the register dump did
**not** prove that tvisor directly corrupted `x19`; that was only an initial
hypothesis.

The value `0x006d656d5f637601` also resembles bytes of data rather than an
aligned pointer. This is another warning sign, but recognizing text in a value
is supporting evidence only. The instruction/FAR correlation is the stronger
evidence.

## 5. Isolate the first operation that exposes the problem

Large diagnostic functions were reduced to small phases, using a distinct
return code for each successful boundary. Every test was performed after a
fresh board reset.

| Test | Work performed before returning | Result |
|---|---|---|
| Phase 0 | Assembly entry and immediate return | `rc = 0x10` |
| Phase 1 | Enter minimal Rust code | `rc = 0x11` |
| Phase 2 | Initialize and finish UART debug output | `rc = 0x12` |
| Phase 3 | Construct diagnostic state | Failed after return |
| Reduced test | Read only `CurrentEL` | Failed after return |
| Control test | Construct `DiagState::default()`; execute no `MRS` | Same U-Boot abort |

The last control test was especially important. It ruled out the EL2 system
register reads themselves. Merely changing the generated Rust code was enough
to expose the failure, which suggested a calling-convention or handoff-state
problem rather than a bad `TCR_EL2`, `TTBR0_EL2`, or `MAIR_EL2` access.

## 6. Inspect the generated machine code

Source-level reasoning was insufficient because register allocation is chosen
by the compiler. Disassembly of `DiagState::default()` showed generated code
using `x18` as a temporary, including a sequence of this form:

```asm
ldr x18, [sp, #0x30]
...
str x18, [x9, #0x28]
```

On AArch64, `x18` is the platform register. Its meaning and whether ordinary
code may use it are platform-dependent. The bare-metal Rust target used for
tvisor allowed LLVM to allocate `x18` as a temporary. U-Boot, however, relied
on its `x18` platform state still being valid when the application returned.

Consequently, both sides were internally reasonable under their own platform
assumptions, but the handoff boundary did not preserve the state expected by
U-Boot. The new Rust path happened to make LLVM use `x18`, revealing the latent
interface bug. The diagnostic structure itself was not the cause.

## 7. Prove the hypothesis with an A/B test

The assembly entry wrapper was changed to save `x18` through `x29` and `x30`
before calling Rust, then restore them before returning to U-Boot. The tests
were repeated:

- empty diagnostic state returned successfully with `rc = 0x1f`;
- the `CurrentEL` diagnostic returned successfully with `rc = 0x20`;
- the complete diagnostic returned successfully with `rc = 0x0`;
- U-Boot remained responsive.

This A/B result ties the failure to register preservation. Standard AAPCS64
calls already require `x19` through `x29` to be preserved, while the generated
code specifically demonstrated new use of platform register `x18`. Therefore,
`x18` is the decisive difference and the overwhelmingly likely corrupted
state. The live A/B test saved the whole `x18`-`x29` set, so it did not test
`x18` alone in isolation; this distinction is worth retaining in a rigorous
debug record.

No private stack was needed to fix this particular problem. A private stack
may later be appropriate when tvisor owns its complete execution environment,
but it is independent of preserving the caller's register state when returning
to U-Boot.

## 8. Reusable abort-analysis procedure

For future AArch64 aborts, use this order:

1. Preserve the complete log: exception type, ESR, FAR, ELR, LR, every general
   register, and the instruction words.
2. Locate `ELR` in the correct image. Account for relocation and verify whether
   the fault occurred in U-Boot, tvisor, or a guest.
3. Decode `ESR`: begin with `EC`, then interpret `ISS` using the format selected
   by that EC. Do not decode all ESR values as Data Aborts.
4. If `FAR` is valid, compare it with the base and index registers used by the
   faulting instruction.
5. Disassemble the instruction at `ELR` and several instructions before it.
   The parenthesized word in a U-Boot dump is the current instruction.
6. Use nearby instructions to determine where suspicious register values came
   from. A bad value visible in a register is not necessarily the register that
   was originally corrupted.
7. Match runtime addresses against unstripped ELF files using tools such as
   `llvm-addr2line`, `llvm-objdump`, `nm`, and the linker map.
8. Reduce the program into phased tests with unique success codes. Change one
   variable at a time and reset the board before each run.
9. Inspect compiler-generated assembly when the failure changes after an
   innocent source edit. Calling conventions and register allocation exist at
   the machine-code boundary, not at the Rust source boundary.
10. Confirm the proposed cause with an A/B test: fail without exactly one
    controlled change, pass with it, and then restore the complete workload.

The central lesson is to connect independent pieces of evidence:

```text
ESR says read Data Abort
        +
FAR equals x0
        +
ELR instruction is ldr x1, [x0]
        +
ELR lies in U-Boot after tvisor returned
        +
generated Rust uses the unpreserved platform register x18
        +
register-preservation A/B test passes
        =
handoff register-state corruption, not an EL2 register-read fault
```
