# Phase 11.3 physical arrival and park acceptance

Task 12, 2026-10-31. **PASS for the one-secondary cold-boot gate on lab-rpi4.**
CPU1 completed generation 1 with `outcome=Success worker=Ok(Parked)` and zero
local validation failures. CPU2 and CPU3 release cells remained zero. The
one-vCPU Linux guest booted and answered a fresh console command. Final relay
power was independently verified off. This does not complete Phase 11.3:
deliberate failure and the full quick regression remain Tasks 13 and 14.

## Implementation and local validation

Added CPU0-only post-arrival release-cell readback in `src/smp.rs`, called before
minting the guest permit in `src/secondary.rs`. It reads only DT-validated cells
through checked existing identity mappings, reports every value, and requires
the selected cell to equal the linked entry and every other cell to remain zero.
A read/mismatch error finalizes the boot attempt as failed, prevents guest entry
and retains resources until reset. It does not write cells or change mappings.

The diagnostic supplies direct post-arrival evidence missing from Task 11's
image. After this change, formatting/whitespace checks, all 166 host tests in
each of debug and release, both warning-free target builds, both linked binary
checks and all six negative mutations per build passed.

## Fresh board and boot evidence

The board was available, with no conflicting regression service, and verified
powered off before cold power-on. U-Boot 2024.01 reported the expected MAC
`dc:a6:32:91:38:be`, DTB `0x2eff1f00`, RAM banks and relocation layout. The DT
advertised the Pi 4 Model B/BCM2711 profile, four Cortex-A72 CPUs with distinct
spin-table cells, no PSCI, and firmware reservation `[0, 0x1000)`. All four
cells read zero before tvisor ran. The tested runtime image occupies
`[0x04000000, 0x0409d000)`, separate from guest staging at `0x30100000`, the DTB
and U-Boot's reserved region.

The unchanged Linux Image fixture was copied, not rebuilt. TFTP used new unique
names; transfer byte counts matched both frozen files. No environment was saved
and no SD-card write was performed. Tvisor selected only CPU1, affinity 1, cell
`0xe0`, with a two-second deadline at the observed 54 MHz counter frequency.

| Checkpoint | Observed value |
|---|---|
| Generation / outcome / worker | 1 / Success / Parked |
| EL / MPIDR / MIDR | `0x8` / `0x80000001` / `0x410fd083` (A72 r0p3) |
| SP / private stack | `0x30010ec0` / `[0x30001000, 0x30011000)` |
| TPIDR / VBAR | `0x407afc0` / `0x4028000` |
| MAIR / TCR / TTBR / SCTLR | `0x4ff` / `0x80843519` / `0x408d000` / `0x30cd183d` |
| DAIF / HCR / CPTR | `0x3c0` / `0x80000000` / `0x33ff` |
| HSTR / VTTBR / CNTVOFF / CNTHCTL | 0 / 0 / 0 / 3 |
| CNTHP / CNTP / CNTV controls / SPSel | 0 / 0 / 0 / 1 |
| Original SCTLR / CPUECTLR | `0x30c50830` / `0x40` |
| Local validation failure mask | 0 |

The original entry observations satisfy the backend's little-endian,
MMU/data-cache-off and A72 SMPEN predicates. The final checkpoint matches the
shared host policy and private context/stack. `Parked` follows successful
validation and arrival in the production mailbox protocol; it is not Online.

Post-arrival cell readback was:

| Physical cell | CPU | Value |
|---|---|---|
| `0xd8` | boot CPU0 | 0 |
| `0xe0` | selected CPU1 | `0x04001000`, the linked `__secondary_entry` |
| `0xe8` | CPU2 | 0 |
| `0xf0` | CPU3 | 0 |

Together with the single-cell release implementation and the firmware contract,
this verifies CPU2/CPU3 were not released by tvisor. It does not independently
sample their program counters. There was exactly one secondary checkpoint in
the fresh boot capture.

Linux 6.18.2 reached `BusyBox initramfs ready`, reported one activated CPU and
answered `PHASE113_TASK12_CONSOLE_OK` plus `uname -a`; the unique rpictl completion
marker reported exit status zero. No fatal/reset marker was present. The normal
fixture's expected proc/sysfs mount messages reflect its disabled filesystems;
the diagnostic fixture and timer/memory regression are reserved for Task 14.

## Provenance and cleanup

Evidence is retained in ignored `docs/evidence/phase11_3_task12_readback/`:
manifest with hashes, actual ELF and guest fixture, source snapshots and tracked
patch, local-check logs, transfer results, contiguous raw UART captures and
`boot.uart.log`, console response, power-off operation and independent
`power-off-verified.json`. The checkout is dirty at base revision
`de73e049e604cf310fe44fabb625042560dfa19b`; nothing was committed or pushed.

- Tested tvisor SHA256:
  `4d782f0669295728f05775cee1d71f72484420c5b613372456cd39abb82afd2c`.
- Guest Image SHA256:
  `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15`.

Initial preparation and fresh U-Boot layout/power-on evidence are preserved in
`docs/evidence/phase11_3_task12/`. Its first image was superseded before any
`bootelf` execution to add readback diagnostics; it was not a failed hardware
attempt. There was one executed Task 12 boot, using the readback image.

The boot and console RPCs timed out waiting for their expected prompt. Complete
new UART output, contiguous cursors, matching console generation and the unique
zero-exit completion marker establish execution; the RPC timeout itself was not
used as success evidence. Both captures were untruncated with valid UTF-8.

After the RAM-only guest check, forced relay-off completed. A separate
`get_power_state` returned `state=off` with `identity_verified=true`; serial state
was not used as proof of power-off. Tasks 13–15 remain outstanding.
