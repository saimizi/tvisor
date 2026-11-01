#!/usr/bin/env python3
"""Bounded Phase 11.5 acceptance using prepared, uniquely published artifacts.

Uses the existing daemon IPC exclusively. No direct UART access, SD writes,
firmware environment saves, retries, or mutation of an earlier result bundle.
"""
import argparse
from pathlib import Path
import re
import signal
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'stability'))
from run import Run


class ExpectedTimeout(Exception):
    pass


class Acceptance(Run):
    def __init__(self, directory, mode):
        super().__init__(directory, 600 if mode == 'quick' else 0, 'quick')
        self.acceptance_mode = mode
        if mode != 'quick':
            self.expected_boots = 0 if mode == 'timeout' else 1
            self.phase = 'phase11.5-' + mode

    def rpc(self, method, **params):
        transfer = method == 'run_uboot_command' and '; tftpboot ' in params.get('command', '')
        if transfer:
            params['command'] = params['command'].replace('; tftpboot ', '; if ping 192.168.28.1; then tftpboot ', 1) + '; fi'
        result = super().rpc(method, **params)
        if method == 'run_uboot_command' and params.get('command') == 'bdinfo; printenv fdt_addr':
            for expected in [self.manifest['expected_mac'], 'fdt_addr=' + self.manifest['expected_fdt'], '37f4d000', '0x38000000', '0xbc000000']:
                if expected not in result['output'].lower():
                    raise RuntimeError('Fresh board layout differs from verified profile: ' + expected)
            self.event('board_layout_verified')
        if transfer:
            if not result or 'host 192.168.28.1 is alive' not in result['output']:
                raise RuntimeError('Fresh network probe failed; transfer not accepted')
            self.event('network_probe_verified')
        return result

    def boot(self, name, image, diagnostic):
        super().boot(name, image, diagnostic)
        if self.acceptance_mode == "quick":
            self.assess_resources()

    def fresh_log(self):
        text = Path(self.current_log.name).read_text()
        # The daemon's recent buffer can include previous boots. Assess only
        # the current tvisor invocation, following its unique transfer name.
        text = text[text.rindex("Filename '" + self.manifest['tvisor'] + "'."):]
        return text

    def wait_text(self, pattern, timeout):
        if self.acceptance_mode == 'timeout' and pattern == '^BusyBox initramfs ready$':
            super().wait_text('^Phase 11.3 timeout-test PASS:', 20)
            text = self.fresh_log()
            if 'Phase 11.5 timeout-test CPU2 group_preserved=true' not in text:
                raise RuntimeError('Missing partial-group preservation')
            if 'entering one-vCPU Linux guest' in text or 'Linux version ' in text:
                raise RuntimeError('Guest ran after startup timeout')
            raise ExpectedTimeout()
        return super().wait_text(pattern, timeout)

    def assess_resources(self):
        text = self.fresh_log()
        prepared = re.findall(r'Phase 11.1 CPU([0-3]) prepared: affinity=(0x[0-9a-f]+) context=(0x[0-9a-f]+) stack=\[(0x[0-9a-f]+), (0x[0-9a-f]+)\)', text)
        cpus = {int(row[0]): tuple(int(x, 16) for x in row[1:]) for row in prepared}
        if set(cpus) != set(range(4)):
            raise RuntimeError('Missing prepared resources')
        if len({r[0] for r in cpus.values()}) != 4 or len({r[1] for r in cpus.values()}) != 4:
            raise RuntimeError('Duplicate CPU identity/context')
        for cpu, (_, context, bottom, top) in cpus.items():
            if top - bottom != 65536 or any(bottom < r[3] and r[2] < top for other, r in cpus.items() if other != cpu):
                raise RuntimeError('Invalid/overlapping stacks')
            if cpu:
                checkpoint = re.search(rf'Phase 11.3 CPU{cpu} checkpoint generation=1 EL=0x8 MPIDR=(0x[0-9a-f]+) SP=(0x[0-9a-f]+) TPIDR=(0x[0-9a-f]+)', text)
                if not checkpoint or int(checkpoint[3], 16) != context or not bottom <= int(checkpoint[2], 16) < top:
                    raise RuntimeError('Checkpoint resource mismatch')
                if int(checkpoint[1], 16) & 0xff00ffffff != cpus[cpu][0]:
                    raise RuntimeError('Checkpoint affinity mismatch')
                if f'CPU{cpu} Online management bidirectional PASS' not in text:
                    raise RuntimeError('Missing Online worker')
        targets = dict(re.findall(r'CPU([0-3]) GIC target=(0x[0-9a-f]+)', text))
        if len(targets) != 4 or len(set(targets.values())) != 4:
            raise RuntimeError('GIC target uniqueness')
        if 'all-core management PASS required_mask=0xe HCR=0x80000010' not in text or 'local validation failures=0x' not in text:
            raise RuntimeError('Missing all-core gate')
        if any(int(value, 16) for value in re.findall(r'local validation failures=(0x[0-9a-f]+)', text)):
            raise RuntimeError('Local validation failed')
        self.event('all_core_resources_pass', cpus=cpus, targets=targets)

    def run(self):
        if self.acceptance_mode == 'quick':
            return super().run()
        self.status('RUNNING')
        diagnostic = self.acceptance_mode == 'stress'
        try:
            self.boot('timeout' if self.acceptance_mode == 'timeout' else 'acceptance', self.manifest['diagnostic_image' if diagnostic else 'baseline_image'], diagnostic)
        except ExpectedTimeout:
            self.event('middle_cpu_timeout_pass')
            return
        if self.acceptance_mode == 'timeout':
            raise RuntimeError('Fault image returned unexpectedly')
        self.assess_resources()
        if diagnostic:
            if 'STRESS phase=0 mask=0xf allocator baseline restored' not in self.fresh_log():
                raise RuntimeError('Missing boot stress result')
            output = self.command('/stability_probe 120', timeout=140)
            if not re.search(r'^STAB PASS ', output, re.M):
                raise RuntimeError('Guest probe failed')
            text = self.fresh_log()
            if 'STRESS FAIL' in text or 'STRESS phase=1 mask=0xe allocator baseline restored' not in text:
                raise RuntimeError('Guest stress missing/failed')
            for cpu in range(1, 4):
                if f'STRESS phase=1 CPU{cpu} rounds=640 pages=5120 heap=2560 pings=480 PASS' not in text:
                    raise RuntimeError('Missing worker completion')
        for _ in range(3):
            self.command('echo ALL_CORE_GUEST_OK; uname -a')
        self.event('all_core_acceptance_pass')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory')
    parser.add_argument('--mode', choices=['bringup', 'stress', 'timeout', 'quick'], required=True)
    parser.add_argument('--authorize-hardware', action='store_true')
    args = parser.parse_args()
    if not args.authorize_hardware:
        parser.error('An authorized hardware run requires --authorize-hardware')
    run = Acceptance(args.directory, args.mode)
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(InterruptedError('Service stopped')))
    outcome, error = 'PASS', None
    try:
        run.run()
    except BaseException as exc:
        outcome, error = 'FAIL', str(exc)
        run.event('failure', error=error)
    finally:
        try:
            run.capture(check=False)
        except BaseException as exc:
            run.event('capture_cleanup_notice', error=str(exc))
        try:
            run.off()
            run.event('power_off_verified')
        except BaseException as exc:
            outcome, error = 'FAIL', f'{error or ""}; cleanup: {exc}'
        run.status(outcome, error)
        run.event('finished', outcome=outcome)
    return int(outcome != 'PASS')


if __name__ == '__main__':
    raise SystemExit(main())
