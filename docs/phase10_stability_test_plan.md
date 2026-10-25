# Tvisor single-vCPU hardware regression tests

Use these tests on `lab-rpi4` after tvisor changes, keeping the established
single-vCPU Linux platform as the comparison point. A pass supplies evidence for
the Phase 10 completion gate; it does not validate physical or guest SMP.

## Modes and coverage

| Mode | Normal guest cold boots | Diagnostic soak | Final diagnostic reboots |
|---|---|---|---|
| `quick` | 2 | 10 minutes | 1 |
| `overnight` | 5 | 6 hours | 5 |

Both modes also boot the diagnostic guest and run a 35-second smoke probe
before the soak. Each final reboot includes a five-second timer probe. Thus
quick mode requires four total boots and overnight requires eleven.
Allow roughly 15 minutes for quick hardware execution plus image preparation.

`scripts/stability/probe.c` requests repeating 1 ms, 10 ms, 100 ms and 1 s
nanosleeps; verifies monotonic time and no early wakeups; alternates 30-second
idle/load phases; and uses another guest process to write/verify changing
patterns across 32 MiB of RAM. Heartbeats report wakeup counts, maximum lateness
and memory passes every 30 seconds. The host independently checks UART responses
and increasing architectural timer IRQ counts every minute, starting 45 seconds
into the soak during a load phase.

The preparation helper builds current tvisor, runs formatting/native library
tests, compiles the static ARM64 probe and builds an isolated diagnostic kernel.
It preserves the normal guest image, verifies the embedded probe and staging
bounds, and writes a frozen manifest with source and configuration evidence.
It does not publish files or control hardware.

Example setup, from the repository root:

```sh
repo="$PWD"
mode=quick  # or overnight
run="$repo/guest/build/regression-$(date -u +%Y%m%dT%H%M%SZ)-$mode"
```

## Prepare and execute

Set `repo` to the absolute path of the current tvisor checkout.
Choose a fresh absolute `run` directory under `guest/build/` (timestamp plus
unique suffix), and `mode=quick` or `mode=overnight`. Examples below are shell
commands; quote paths, and do not interpolate untrusted strings into commands.

### Prerequisites

- ARM64 Linux host; Rust bare-metal target `aarch64-unknown-none` and cached
  locked Cargo dependencies. Host library tests use the actual rustc host triple,
  not the old x86_64 `cargo test-host` alias.
- Clang/LLVM/LLD 22, GNU `aarch64-linux-gnu-gcc` with static libc, make and the
  normal guest build prerequisites. Normal guest example: `make -C guest all
  LLVM=-22 JOBS=4`. Do not install system packages unless that setup is authorized.
- Existing sources `guest/linux-6.18.2/`, normal guest build configuration and
  `guest/build/arm64/arch/arm64/boot/Image`, static BusyBox under
  `guest/build/busybox-arm64/`, and its generated `.config`.
- Existing verified rpictl daemon at `/tmp/rpictl-mcp.sock`, configured board
  `lab-rpi4` and UART `/dev/ttyUSB0`. Use rpictl tools to inspect power/ownership;
  never open the UART in a second process or rewrite the global board config.
- LAN `eth0=192.168.28.1/24`, working read-only TFTP at `/srv/tftp`. This setup
  uses dnsmasq in `eth0-gateway-dhcp.service`. Inspect rather than recreating the
  network. Verify no conflicting board operation or regression service exists.

Prepare as the normal development user, not root:

```sh
python3 "$repo/scripts/stability/prepare.py" --repo "$repo" --mode "$mode" --output "$run" --dry-run
python3 "$repo/scripts/stability/prepare.py" --repo "$repo" --mode "$mode" --output "$run"
```

`--baseline-image /absolute/path/Image` can select an explicitly chosen frozen
fixture. The helper defaults to `LLVM=-22`, at most four build jobs, and fails
rather than overwriting a prior run. A diagnostic kernel is built in the new run
directory; only PROC_FS, SYSFS and the diagnostic initramfs differ intentionally
from the normal guest config. Kconfig dependencies are recorded in
`kernel-config.diff`. Inspect this diff, source status, ELF bounds, build/test
logs, and manifest before publication. Build failures leave logs in that new
directory and must not be described as hardware failures.

Publish only the three files named in `manifest.json`. Pass filenames as actual
subprocess arguments, not shell-generated JSON strings. For example:

```sh
python3 - "$run" <<'PY'
import json, pathlib, subprocess, sys
run = pathlib.Path(sys.argv[1])
for item in json.loads((run / 'manifest.json').read_text())['files']:
    name = pathlib.Path(item['path']).name
    subprocess.run(['sudo', 'install', '-m', '0644', str(run / name), item['path']], check=True)
PY
```

Use the environment's escalation mechanism for writes outside the workspace.
Publication is required for the authorized test; it is not a reason to request
another conversational confirmation when the user already asked for that test.

### Run and detach

Foreground (quick tests can take longer overall because of kernel preparation):

```sh
python3 "$repo/scripts/stability/run.py" "$run" --mode "$mode" --authorize-hardware
```

`--authorize-hardware` represents the user's actual authorization; do not supply
it for a plan/results-only request. The modes use 600 and 21600 soak seconds.
An explicitly requested custom duration can use `--soak-seconds` (120–25200);
report the deviation and never call a shortened run a full overnight pass.

For unattended tests, use a unique systemd unit, bounded to nine hours, running
as the development user. A system service survives the conversation/log-out
without changing user linger settings. Example with pre-reviewed variables:

```sh
sudo systemd-run --unit="$unit" --uid="$(id -un)" \
  --property=RuntimeMaxSec=9h --property=TimeoutStopSec=150 \
  --property="StandardOutput=append:$run/runner.log" \
  --property="StandardError=append:$run/runner.log" \
  --working-directory="$repo" \
  /usr/bin/python3 -u "$repo/scripts/stability/run.py" "$run" \
  --mode "$mode" --authorize-hardware
systemctl show "$unit.service" -p ActiveState -p SubState -p User -p RuntimeMaxUSec
```

Verify at least initial boot/console progress and, for a changed harness, the
smoke probe and first progressing soak checks before leaving it unattended.
The host and rpictl daemon must stay running. Stopping the service triggers
cleanup; a host power loss/SIGKILL cannot guarantee board cleanup. Do not launch
another service against the same board.

### Inspect or stop

```sh
cat "$run/report.md"
tail -n 20 "$run/events.jsonl"
systemctl status "$unit.service"
# Stop early only when requested/needed; verify cleanup afterward:
sudo systemctl stop "$unit.service"
```

No `status.json`/`events.jsonl` directory is reusable: the runner rejects it.
On failure it preserves evidence and powers off, without retrying a failed boot
or restarting the guest soak. Verify `power_off_verified`; if absent, inspect
actual relay state through rpictl and complete authorized cleanup.

## Evidence and baseline comparison

### Output bundle

- `manifest.json`: mode, exact tvisor revision, board profile, published binary
  sizes/hashes, probe/runner hashes and kernel configuration hashes.
- `source-status.txt`, `source-changes.patch`: working-tree provenance; a commit
  identifier alone does not describe dirty builds.
- `status.json`, `report.md`: running/final verdict, expected/completed boots,
  commands, host checks, probe results, and failure detail.
- `events.jsonl`: host timestamps, boots, probe messages, timer IRQ progress,
  final result and `power_off_verified`.
- `*.uart.log`: per-boot serial evidence. Initial ring-buffer snapshots may
  contain older output. Use the subsequent bootelf invocation and uniquely
  tagged output lines as fresh evidence, not an old prompt or echoed command.
- `build.log`, `host-tests.log`, `kernel-build.log`, `kernel-config.diff`, and
  frozen normal/diagnostic kernel and BusyBox configurations.

### Acceptance

Both modes use the same probe/thresholds, not a weaker quick-test oracle:

- Required boots: quick 4 total (2 normal + 1 soak + 1 final); overnight 11
  (5 normal + 1 soak + 5 final). Every boot must execute a fresh tagged command.
- Full selected soak must print a new `STAB PASS` at the expected duration.
  The 35-second smoke pass is not the soak pass. Require increasing wakeups,
  timer IRQ counts, memory worker progress during load, and independent UART
  responses. The 5-second post-reboot probe is intentionally idle-only.
- Fail on guest/kernel/hypervisor abort, reset, panic/oops, memory-worker failure,
  early wakeup/backwards monotonic time, >2 s wakeup lateness, 90 s without
  heartbeat progress, missing console response, non-increasing timer IRQ count,
  truncated/missing serial evidence, or guest/host drift >max(5 s, 2%).
- Explicitly report any wakeups >500 ms late even if the runner's hard verdict
  is PASS. Review these as warnings before accepting a regression baseline.
- Inspect full lines, handling fragmented serial reads and a shell prompt
  prefixed to `STAB` output. Ignore historical snapshots for counters/verdicts.
- rpictl can report a prompt timeout after successfully sending bootelf or a
  command because this guest has a minimal prompt. A logged RPC timeout is not
  itself a pass: require the new boot/unique completion output in captured UART.
- Verify final power-off independently from a stale serial state. Missing final
  result/cleanup is incomplete, including a host/service restart. Never promote
  an incomplete run to PASS based on earlier boot messages.

### Established overnight baseline

The successful run of 2026-10-24/25 used tvisor
`bd639dc567c3802ccff95d891900be5e3e9baf5d`, Linux 6.18.2, Clang/LLD 22.1.8,
one vCPU and 512 MiB guest RAM. It completed at approximately 05:33 JST:

- 11 successful boots, 379 completed console commands, 360 soak console/IRQ checks;
- 21600.615 s probe runtime, 76832 timed wakeups;
- 288321 complete write/verify passes over 32 MiB during alternating load phases;
- maximum wakeup lateness 52.001 ms, zero wakeups >500 ms;
- timer IRQ count progressed from 20807 to 5405206 across host samples;
- no fatal/reset/timer-injection error markers; verified power-off.

Original evidence, when retained in the checkout:
`guest/build/stability-run-2026-10-24/`. Read its manifest and diagnostic-soak log
for exact binary hashes; do not recreate or overwrite that directory. Pilot
attempts were retained separately: shell `&;` syntax and prompt-prefix parser
bugs were fixed before the successful six-hour run. The repository runner includes
those fixes and parameterizes mode/result directories.

Compare like with like: record changed tvisor, guest config/image, probe and
thresholds. A new fixture/probe is a new test baseline, not proof of a hypervisor
regression. Timing is a liveness measure on a periodic 250 Hz kernel, not a
real-time latency guarantee; 52 ms is an observation, not a universal limit.

### Interpretation

A quick pass is useful after routine changes. Prefer overnight after changes to
translation, vCPU context, timers, GIC/IRQ handling, or concurrency. These tests
cover one vCPU; physical/guest SMP correctness, stage-2 fault isolation, teardown,
multiple VMs, and complete device-register behavior need separate tests. Report
those limits instead of declaring Phase 11/12 complete from this suite alone.
