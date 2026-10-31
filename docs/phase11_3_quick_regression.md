# Phase 11.3 quick regression with one parked secondary

Task 14, 2026-10-31: **PASS on lab-rpi4.** The complete quick profile ran with
CPU1 parked and a one-vCPU Linux guest. Four boots, the smoke probe, the full
600-second soak and the final short probe all passed. Final relay power was
independently verified off. Task 15's final consolidated review remains pending.

## Results

| Check | Result |
|---|---|
| Normal-fixture cold boots | 2/2 passed |
| Diagnostic boots, before/after soak | 2/2 passed |
| CPU1 checkpoint on each boot | Generation 1, Success/Parked, zero local failures |
| Other physical secondaries | CPU2/CPU3 release cells stayed zero on each boot |
| Guest console commands | 18 passed |
| Independent soak console/timer checks | 10 passed |
| Timer IRQ counter, first to last soak sample | 20,797 to 155,988; every sample increased |
| Smoke probe | 35.987 s, 128 wakeups, 279 memory passes, maximum lateness 7.630 ms |
| Full soak | 600.453 s, 2,136 wakeups, 8,133 memory passes, maximum lateness 7.585 ms |
| Final short probe | 5.628 s, 20 wakeups, maximum lateness 4.000 ms |
| Wakeups over 500 ms | Zero in all three probes |
| Fatal/reset/guest or hypervisor error markers | None in any fresh boot segment |
| Cleanup | Runner verified off; separate rpictl relay query confirmed off and identity verified |

The final five-second probe intentionally covers the idle timer phase and
reports zero memory passes; the smoke and full soak exercised the memory worker.
The soak alternated idle/load phases while repeatedly writing/verifying 32 MiB.
The runner required increasing timer counters and heartbeat/wakeup progress,
bounded console responses, and host/guest clock agreement within its 2%/5 s
threshold. Wakeup lateness above 2 s would fail; none exceeded 500 ms.

The run took about 13 minutes 45 seconds, from 13:22:32 to 13:36:17 UTC
(22:22:32–22:36:17 JST), plus roughly five minutes of image preparation.

## Tested images and preparation

The existing skill's preparation/runner/probe were used without modification.
The tested tvisor is a **normal debug build**, with default features and no
`secondary-startup-timeout-test`. The injected Task 13 image was not used.
Formatting, all 166 native host tests, a warning-free build, linked entry/
publication checks and all six negative binary checks passed before execution.
The normal ELF was checked for absence of timeout-test instrumentation.

The normal guest Image was copied unchanged. A separate diagnostic kernel was
built with proc/sysfs and a diagnostic initramfs containing the static probe;
the recorded configuration diff includes their Kconfig dependencies. Its embedded
probe was verified against the prepared executable. No established image was
replaced. Three fresh unique TFTP files were published and hash-verified.

| Artifact | SHA256 |
|---|---|
| Normal debug tvisor | `7efb8db6f03cc3ed53570ac5f8e19a202703f3e3371e762bd946c11501f0b070` |
| Unchanged normal Linux Image | `3a72c904e211f56e5f05d601b41495dfa4ec9029b3d7b1ee3556a72f9bb33b15` |
| Diagnostic Linux Image | `2a1bca8ceab64470c653f8f11026da9312d7a4de77c38a0dbd03022fddcc7cd9` |

The checkout is dirty at base revision
`de73e049e604cf310fe44fabb625042560dfa19b`. Explicit snapshots and hashes retain
the meaningful untracked implementation as well as tracked changes. Source
hashes still matched the checkout after the run. No implementation was changed
during regression; nothing was committed or pushed.

## Hardware and evidence review

The board was available and verified off before preparation. Fresh U-Boot
`bdinfo`/environment evidence on every boot matched RAM banks, relocation
`0x37f4d000`, MAC `dc:a6:32:91:38:be` and DTB `0x2eff1f00`. Linker bounds fit
the staging/runtime window; the debug runtime extent ended at `0x04129000`,
clear of guest staging, the DTB and U-Boot reservations. Runtime DT discovery
validated the four distinct reserved spin-table cells and their initial zeros.
No environment was saved and no SD-card write was performed.

Every fresh boot segment contained exactly one CPU1 generation-1 checkpoint,
EL2/affinity evidence, zero local failures, Success/Parked and the selected cell
`0xe0=0x04001000`. Cells `0xd8`, `0xe8` and `0xf0` remained zero. The guest was
entered through the normal consuming permit. This supports coexistence with
one parked secondary; it does not establish Online, SGI routing, all-core
concurrency or live cross-core TLB maintenance.

The bounded service ran the complete quick profile with a 600-second soak and
automatic failure/completion power-off cleanup. UART was used exclusively by
the runner. Known rpictl prompt-wait notices were assessed against fresh guest
output and unique successful command markers. Historical ring-buffer snapshots
were excluded from the per-boot secondary/error review. The finished event
reported PASS after verified cleanup; an additional relay query independently
confirmed power-off. The transient systemd unit was inactive after completion.

The ignored bundle is `guest/build/phase113-task14-20261031/`:

- `report.md`, `status.json`, `events.jsonl` and four `*.uart.log` files retain
  the generic regression results. The report's existing Phase 10 title describes
  the runner's guest checks; this document and `phase113-assessment.json` add
  the Phase 11.3 per-boot parked-secondary assessment.
- `manifest.json`, `source-manifest.json`, `source/`, `source-changes.patch`,
  actual frozen images/configurations, harness copies and build/check logs tie
  the run to its sources and fixtures.
- `power-off-independent.json` preserves the separate final relay query.
- `artifact-sha256.json` indexes the retained root artifacts and source snapshots;
  the reproducible kernel build directory is excluded from that index.

The generic overnight baseline used a different tvisor revision and six-hour
duration. This quick pass completes V8's requested profile; it is not an overnight
pass or a claim of real-time latency guarantees.
