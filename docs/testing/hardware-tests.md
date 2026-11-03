# Hardware acceptance and regression

Use the supported Raspberry Pi 4 Model B, existing rpictl daemon and verified
single-vCPU Linux fixtures. Host tests and linked checks establish prerequisites;
they cannot establish physical coherency, entry state or cross-CPU behavior.
Run local checks first and retain exact source/artifact provenance.

The current helper filenames and their `--phase` argument are retained verbatim
below so commands remain executable. They will be renamed together with their
parsers; the test descriptions use functional names.

## Test selection

| Purpose | Prepare options | Runner and mode | Required observation |
|---|---|---|---|
| Quiet normal-image regression | `--kind normal` | `phase116_acceptance.py --mode quiet` | Four boots, full quick soak, no detailed acceptance output |
| Physical CPU bring-up | `--kind normal --diagnostics` | `phase116_acceptance.py --mode bringup` | Unique contexts/stacks/targets, management, parked responsiveness, Linux console |
| EL2 mapping updates | `--kind mapping` | `phase116_acceptance.py --mode mapping` | Exact 48 remaps: 32 Online, 16 Parked; backing-reuse checks; 35-second Linux probe |
| Mapping timeout | `--kind mapping-timeout` | `phase116_acceptance.py --mode mapping-timeout` | CPU2 late ack rejected, mapping/backing retained, guest blocked |
| Shared-resource concurrency | `--kind stress` | `phase116_acceptance.py --mode stress` | Allocator/heap/logging/SGI workloads, restored baselines, 120-second Linux probe |
| Group startup timeout | `--kind timeout` | `phase115_acceptance.py --mode timeout` | Late CPU2 arrival cannot reopen failed boot, group preserved, guest blocked |
| Diagnostic quick regression | `--kind normal --diagnostics` | `phase116_acceptance.py --mode quick` | Four boots with physical CPU checks and full 600-second soak |
| Diagnostic overnight regression | `--kind normal --diagnostics --mode overnight` | `phase116_acceptance.py --mode overnight` | Eleven boots with physical CPU checks and full 21,600-second soak |

Preparation uses `scripts/checks/phase115_prepare.py --phase 11.6` for this table.
Mapping and stress use release builds; other kinds use debug. Diagnostics alone
does not allocate scratch pages or enable remapping. New normal/stress bundles
declare zero expected remaps; older frozen bundles retain their recorded expectations.
Do not rewrite their manifests to match new behavior.

Single-secondary timeout and management IRQ acceptance features are separately
buildable but are not selected by this preparation helper. Their hardware review
must capture the respective feature's complete output, terminal guest blocking
for timeout, and masked EL1/EL2 delivery, GPR/SIMD/PSTATE preservation,
non-injection and HCR activation/deactivation checks for management. Do not
claim the table above automatically runs those feature-specific workloads.

## Prerequisites and board ownership

The validated lab uses an ARM64 Linux host, `/tmp/rpictl-mcp.sock`, configured
board ID `lab-rpi4`, UART `/dev/ttyUSB0`, TFTP root `/srv/tftp` and server
`192.168.28.1` on `eth0`. These are deployment configuration, not generic tvisor
requirements. Inspect the existing network/daemon and board ownership rather
than replacing them. No other service or serial client may own the board/UART.

Verify a fresh [board layout](../setup/raspberry-pi4.md#validate-the-live-layout),
power-control identity and intended fixture before running. The acceptance runner
checks the recorded MAC, DTB and RAM/U-Boot layout. A mismatch requires a profile
review. Never change expected addresses just to make a failed check pass.

Hardware runners power-cycle and finish by powering off the known RAM-only
guest. Do not run them against another workload. If an earlier failure booted
the SD-card OS, cleanup uses graceful shutdown; unknown state refuses a forced
cut. Tests do not save U-Boot environment or write the SD card. `--authorize-hardware`
is an explicit execution gate, not a dry-run flag.

## Prepare a fixture

For hypervisor-only changes, reuse an existing frozen quick bundle. This avoids
changing Linux, BusyBox, probe or configuration while comparing tvisor behavior.
Use a fresh unique output directory for every preparation and attempt:

```sh
fixture=/absolute/path/to/verified-quick-bundle
run="$PWD/guest/build/acceptance-$(date -u +%Y%m%dT%H%M%SZ)-mapping"
python3 scripts/checks/phase115_prepare.py --fixture "$fixture" \
  --output "$run" --phase 11.6 --kind mapping --dry-run
python3 scripts/checks/phase115_prepare.py --fixture "$fixture" \
  --output "$run" --phase 11.6 --kind mapping
```

Choose the options from the test table. The helper verifies frozen Image/config/
probe hashes, builds current tvisor, copies its ELF immediately, checks staging
bounds and linked entry/diagnostic/TLBI properties, and snapshots source hashes
including participating untracked files. It neither rebuilds Linux nor touches
hardware/TFTP. The source-manifest capture currently covers flat source trees;
update its enumeration if source modules move into subdirectories.

When no fixture exists, create one explicitly with
[`scripts/stability/prepare.py`](../../scripts/stability/prepare.py):

```sh
repo="$PWD"
board_id=lab-rpi4
fixture="$repo/guest/build/fixture-$(date -u +%Y%m%dT%H%M%SZ)"
python3 scripts/stability/prepare.py --repo "$repo" --mode quick \
  --board-id "$board_id" --output "$fixture" --dry-run
python3 scripts/stability/prepare.py --repo "$repo" --mode quick \
  --board-id "$board_id" --output "$fixture"
```

This path needs the [guest build prerequisites](../setup/build.md#linux-guest),
existing Linux sources/normal Image and configured static BusyBox. It builds a
static probe and isolated diagnostic kernel; the normal guest is preserved.
`--baseline-image /absolute/path/Image` selects another intentional baseline.
The default LLVM suffix is `-22`, and jobs default to at most four. Review
`kernel-config.diff`, embedded probe verification, staging bounds and logs.
Changing guest configuration/probe creates a new baseline, not evidence about
a hypervisor-only regression. A build failure is not a hardware failure.

## Publish and execute

Inspect `manifest.json`, build/check logs, hashes and source snapshots before
publication. Publish only its three image artifacts, retaining unique names:

```sh
python3 - "$run" <<'PY'
import json, pathlib, subprocess, sys
directory = pathlib.Path(sys.argv[1])
for item in json.loads((directory / 'manifest.json').read_text())['files']:
    name = pathlib.Path(item['path']).name
    subprocess.run(['sudo', 'install', '-m', '0644',
                    str(directory / name), item['path']], check=True)
PY
python3 scripts/checks/phase116_acceptance.py "$run" \
  --mode mapping --authorize-hardware
```

Match runner/mode to the table, particularly the separate group-timeout runner.
Run mapping, timeout and concurrency pilots plus quick regression before
overnight. A result directory containing status/events is not reusable; failed
and interrupted attempts remain separate. Do not retry away an actual failure.

For an unattended overnight run, prepare an overnight diagnostics bundle, publish
it, and use a unique bounded service. Set `repo` and `run` to absolute paths:

```sh
repo="$PWD"
unit="tvisor-overnight-$(date -u +%Y%m%dT%H%M%SZ)"
sudo systemd-run --unit="$unit" --uid="$(id -un)" \
  --property=RuntimeMaxSec=9h --property=TimeoutStopSec=150 \
  --property="StandardOutput=append:$run/runner.log" \
  --property="StandardError=append:$run/runner.log" \
  --working-directory="$repo" \
  /usr/bin/python3 -u "$repo/scripts/checks/phase116_acceptance.py" "$run" \
  --mode overnight --authorize-hardware
systemctl show "$unit.service" -p ActiveState -p SubState -p User -p RuntimeMaxUSec
```

Verify initial boot/console progress and, after harness changes, smoke and first
progressing soak checks. The host and rpictl daemon must stay running. Ordinary
service termination runs cleanup; host power loss or SIGKILL cannot guarantee it.

```sh
cat "$run/report.md"
tail -n 20 "$run/events.jsonl"
systemctl status "$unit.service"
# If stopping the run, inspect cleanup and actual power afterward:
sudo systemctl stop "$unit.service"
```

For guest-only stability coverage, `scripts/stability/run.py` accepts `quick`
or `overnight` with `--authorize-hardware`; it does not add the physical CPU
acceptance checks. Its optional `--soak-seconds` supports 120–25200 seconds.
Report custom durations explicitly; a shortened run is not a full overnight pass.

## Acceptance criteria

Quick regression requires two baseline guest boots, one diagnostic smoke/soak
boot and one post-soak diagnostic reboot: four total, including 35-second smoke,
600-second soak and five-second post-reboot probe. Overnight requires five
baseline boots, the diagnostic smoke/21,600-second soak and five diagnostic
reboots: eleven total. Kernel diagnostic status and tvisor `diagnostics` are
independent settings; a quiet tvisor can still run the diagnostic guest probe.

The probe repeats 1 ms/10 ms/100 ms/1 s sleeps, checks monotonic time and early
wakes, alternates idle/load every 30 seconds and verifies patterns over 32 MiB.
Host checks independently require UART responses and increasing timer IRQ counts
each minute, starting 45 seconds into the soak. Five-second probes are idle-only.

Require fresh tagged command completion, the full-duration `STAB PASS`, increasing
wakeups/IRQ samples and memory progress under load. Fail on abort/reset/panic/oops,
memory verification errors, early/backward time, >2-second wakeup lateness,
90 seconds without heartbeat, missing console response, non-increasing IRQ count,
truncated evidence or guest/host drift >max(5 seconds, 2%). Report every wakeup
over 500 ms late even below the hard failure limit. These are liveness checks,
not real-time latency guarantees.

Diagnostics-enabled physical CPU boots additionally require unique affinities,
contexts, stacks and GIC targets; acquired local checkpoints with no failures;
bidirectional management and fresh parked-worker pings. The actual retaining
mask is `0xf`, parked subset `0xe` on the four-core profile. `online_mask` in
existing output includes initialized retaining Parked CPUs; it is not a count
of CPUs currently in the Online lifecycle state. Quiet output cannot prove
these UART-based checks and must not be reported as doing so.

Mapping acceptance requires exact manifest epochs/signatures and safe old-page
reuse after all owners synchronize. Timeout acceptance delays CPU2 four seconds
against a two-second deadline, observes its real late response, retains mappings/
backing and blocks Linux. Startup timeout likewise distinguishes late lifecycle
arrival from irrevocable boot-policy failure. Concurrency requires disjoint
live allocations, allocator/heap baseline restoration and management progress
alongside Linux. Guest DT CPU count remains one throughout.

## Evidence and comparison

| Artifact | Purpose |
|---|---|
| `manifest.json`, frozen configs and probe hashes | Exact images, build, board and fixture provenance |
| Source snapshots/manifest, status and patch | Actual tested tree, including changes beyond the recorded commit |
| `status.json`, `report.md` | Final outcome, boots, commands, soak and error |
| `events.jsonl` | Progress, timer/console checks, final result and `power_off_verified` |
| `*.uart.log` | Raw per-boot behavior; assess only the fresh invocation segment |
| Build, linked-check and kernel logs | Local prerequisites and guest-config differences |

UART preambles may contain historical ring-buffer data. Whole-file marker counts
can falsely pass a test; assess after the current unique transfer/application
marker. A prompt timeout is not proof of either success or failure: require fresh
execution and unique command-result markers. RUNNING, a smoke pass or elapsed
wall time alone does not prove a completed soak.

Require verified final power-off and independent relay-state confirmation.
Missing completion/cleanup after host interruption is incomplete, never PASS.
Label a harness failure before image execution separately from hypervisor/guest
failure. Fix the harness, validate it, and start a new attempt with new evidence.
Keep frozen fixtures, failed attempts and existing evidence immutable.

The retained comparison bundle, when present locally, is
`docs/evidence/diagnostics_regression_20261102_01/` (ignored by Git). Its complete
physical CPU overnight run records eleven boots, 379 commands, 360 console/IRQ
checks and a 21,600.334-second soak with maximum lateness 50.007 ms and no
>500 ms wakes; quiet quick records a 600.461-second soak. Mapping, late-ack and
concurrent workloads have separate bundles. Match actual source/configuration
and artifact hashes, not the base commit alone. These are retained observations,
not a fresh validation of subsequent changes or a prerequisite download.

Prefer overnight after translation, world-switch, timer/GIC/IRQ or concurrency
changes. Single-vCPU Linux stability does not validate guest multiprocessing,
general device assignment, teardown or stage-2 isolation comprehensively.
