#!/usr/bin/env python3
"""Bounded quick/overnight lab-rpi4 regression test through the existing rpictl daemon IPC.

The IPC framing is the same as rpictl-mcp's SocketClient (RPC JSON + newline).
Never opens the UART directly. No daemon/configuration changes are made.
"""
import argparse
import datetime as dt
import hashlib
import json
from pathlib import Path
import re
import signal
import socket
import time
import uuid


def utc():
    return dt.datetime.now(dt.timezone.utc).isoformat()


class Run:
    def __init__(self, directory, soak, mode="overnight"):
        self.directory = Path(directory).resolve()
        if (self.directory / 'status.json').exists() or (self.directory / 'events.jsonl').exists():
            raise RuntimeError('Run directory already contains results; prepare a new run')
        self.manifest = json.loads((self.directory / 'manifest.json').read_text())
        if self.manifest.get('mode') != mode:
            raise RuntimeError('Requested mode does not match prepared manifest')
        self.mode = mode
        self.before_boots, self.after_boots = (2, 1) if mode == 'quick' else (5, 5)
        self.expected_boots = self.before_boots + self.after_boots + 1
        self.board_id = self.manifest['board_id']
        self.socket_path = self.manifest['rpictl_socket']
        self.soak = soak
        self.cursor = None
        self.generation = None
        self.boots = []
        self.commands = 0
        self.host_checks = 0
        self.probe_lines = []
        self.probe_pending = ""
        self.latest_heartbeat = None
        self.pass_count = 0
        self.fault = None
        self.current_log = None
        self.events = open(self.directory / 'events.jsonl', 'a', buffering=1)
        self.started = utc()
        self.phase = "baseline-boots"

    def event(self, kind, **values):
        row = dict(time=utc(), kind=kind, **values)
        self.events.write(json.dumps(row) + '\n')
        print(json.dumps(row), flush=True)

    def rpc(self, method, optional=False, **params):
        params = dict(board_id=self.board_id, **params)
        with socket.socket(socket.AF_UNIX) as s:
            s.settimeout(120)
            s.connect(self.socket_path)
            s.sendall(('RPC ' + json.dumps(dict(method=method, params=params)) + '\n').encode())
            data = json.loads(s.makefile('rb').readline(4 * 1024 * 1024))
        if not data.get('ok'):
            if optional:
                self.event('rpc_notice', method=method, error=data.get('error'))
                return None
            raise RuntimeError(f'{method}: {data.get("error")}')
        return data['result']

    def capture(self, check=True):
        row = self.rpc('capture_serial', cursor=self.cursor, max_bytes=65536)
        if row['truncated']:
            raise RuntimeError('Serial capture truncated; evidence is incomplete')
        if self.generation is not None and row['console_generation'] != self.generation:
            raise RuntimeError('Unexpected serial generation change')
        self.generation = row['console_generation']
        self.cursor = row['next_cursor']
        text = row['text'].replace('\r', '')
        if self.current_log:
            self.current_log.write(text)
            self.current_log.flush()
        if not check:
            self.probe_pending = ''
            return text
        self.probe_pending += text
        lines = self.probe_pending.split('\n')
        self.probe_pending = lines.pop()
        for line in lines:
            line = line.removeprefix('root@js-virt:guest# ')
            if line.startswith('STAB '):
                self.probe_lines.append(line)
                self.event('probe', line=line)
                if line.startswith('STAB START '):
                    self.latest_heartbeat = None
                h = re.match(r'STAB HEARTBEAT elapsed=([\d.]+) wakes=(\d+)', line)
                if h:
                    self.latest_heartbeat = (float(h[1]), int(h[2]), time.monotonic())
                if line.startswith('STAB PASS '):
                    self.pass_count += 1
                if check and line.startswith('STAB FAIL '):
                    raise RuntimeError(line)
        if check and re.search(r'Kernel panic|PANIC:|Synchronous Abort|Unexpected IRQ|Unexpected exception:|Unexpected VM exit|queue_timer_ppi fails|STAB FAIL|STAB_INIT_.*_FAIL|Resetting CPU|BUG:|Oops:', text):
            raise RuntimeError('Fatal/error marker in UART log')
        return text

    def wait_text(self, pattern, timeout):
        deadline = time.monotonic() + timeout
        text = ''
        while time.monotonic() < deadline:
            text += self.capture()
            if re.search(pattern, text, re.M):
                return text
            time.sleep(0.2)
        raise TimeoutError(f'UART did not show {pattern!r} within {timeout}s')

    def command(self, command, timeout=15):
        # Only complete output lines count; echoed input is not success evidence.
        marker = 'STAB_CMD_' + uuid.uuid4().hex
        wrapped = command + "; printf '\\n" + marker + ":%s\\n' \"$?\""
        self.rpc('run_linux_command', optional=True, command=wrapped,
                 timeout_ms=1000, expected_patterns=None, allow_reboot=False)
        text = self.wait_text('^' + marker + r':\d+$', timeout)
        code = int(re.search('^' + marker + r':(\d+)$', text, re.M)[1])
        if code:
            raise RuntimeError(f'Guest command exit {code}: {command}')
        self.commands += 1
        return text

    def verify_artifacts(self):
        if hashlib.sha256(Path(__file__).read_bytes()).hexdigest() != self.manifest['runner_sha256']:
            raise RuntimeError('Runner changed since preparation; prepare a fresh bundle')
        for f in self.manifest['files']:
            actual = hashlib.sha256(Path(f['path']).read_bytes()).hexdigest()
            if actual != f['sha256']:
                raise RuntimeError('Published artifact changed: ' + f['path'])

    def off(self):
        state = self.rpc('get_power_state')
        if state['state'] != 'off':
            self.rpc('power_off', mode='force', confirmation=True,
                     reason='Authorized tvisor regression test: RAM-only guest, power-cycle/cleanup')
        if self.rpc('get_power_state')['state'] != 'off':
            raise RuntimeError('Relay did not verify off')

    def boot(self, name, image, diagnostic):
        self.verify_artifacts()
        self.event('boot_start', name=name, image=image)
        self.off()
        result = self.rpc('power_on_uboot', timeout_ms=45000)
        if not result['success']:
            raise RuntimeError('Failed to enter U-Boot')
        if self.current_log:
            self.current_log.close()
        self.current_log = open(self.directory / (name + '.uart.log'), 'w', buffering=1)
        self.cursor = None
        self.generation = None
        self.capture(check=False)
        board = self.rpc('run_uboot_command', command='bdinfo; printenv fdt_addr',
                         timeout_ms=5000, expected_patterns=None, allow_reboot=False)['output']
        self.current_log.write(board)
        if self.manifest['expected_mac'] not in board or ('fdt_addr=' + self.manifest['expected_fdt']) not in board:
            raise RuntimeError('Board/DTB layout differs from validated boot layout')
        for address, filename in [('0x04000000', self.manifest['tvisor']), ('0x30100000', image)]:
            output = self.rpc('run_uboot_command', command=f'setenv autostart no; setenv serverip 192.168.28.1; tftpboot {address} {filename}',
                              timeout_ms=20000, expected_patterns=None, allow_reboot=False)['output']
            expected = next(f['size'] for f in self.manifest['files'] if Path(f['path']).name == filename)
            if f'Bytes transferred = {expected} ' not in output:
                raise RuntimeError('TFTP did not transfer expected bytes: ' + filename)
        self.rpc('run_uboot_command', command='setenv guest_size ${filesize}',
                 timeout_ms=3000, expected_patterns=None, allow_reboot=False)
        self.capture(check=False)
        self.rpc('run_uboot_command', optional=True,
                 command='setenv autostart yes; bootelf 0x04000000 fdt=${fdt_addr} image=0x30100000,${guest_size}',
                 timeout_ms=1000, expected_patterns=None, allow_reboot=False)
        self.wait_text('^STABILITY_GUEST_READY$' if diagnostic else '^BusyBox initramfs ready$', 60)
        self.command('echo STAB_SHELL_OK; uname -a')
        self.boots.append(dict(name=name, image=image, passed=True, time=utc()))
        self.event('boot_pass', name=name)
        self.status('RUNNING')

    def status(self, result, error=None):
        row = dict(status=result, mode=self.mode, expected_boots=self.expected_boots, phase=self.phase, started=self.started, updated=utc(),
                   revision=self.manifest['revision'], soak_seconds=self.soak,
                   boots=self.boots, guest_commands=self.commands,
                   host_console_checks=self.host_checks,
                   probe_output=self.probe_lines, error=error)
        temporary = self.directory / 'status.tmp'
        temporary.write_text(json.dumps(row, indent=2) + '\n')
        temporary.replace(self.directory / 'status.json')
        report = [f'# Phase 10 stability run: {result}', '',
                  f'- Revision: `{self.manifest["revision"]}`',
                  f'- Phase: {self.phase}', f'- Started (UTC): {self.started}',
                  f'- Updated (UTC): {row["updated"]}',
                  f'- Completed boots: {len(self.boots)} / {self.expected_boots} (including diagnostic soak boot)',
                  f'- Console commands passed: {self.commands}',
                  f'- Soak console/IRQ checks passed: {self.host_checks}',
                  f'- Requested soak: {self.soak} seconds',
                  f'- Error: {error or "none reported"}', '',
                  'Latest probe output:', '```',
                  *self.probe_lines[-4:], '```', '',
                  'A PASS is evidence for morning review, not an automatic SMP readiness decision.',
                  'Check events.jsonl for final power_off_verified and inspect all UART logs.']
        (self.directory / 'report.md').write_text('\n'.join(report) + '\n')

    def run(self):
        self.status('RUNNING')
        for i in range(self.before_boots):
            self.boot(f'baseline-{i+1:02}', self.manifest['baseline_image'], False)
        self.phase = 'diagnostic-smoke'
        self.boot('diagnostic-soak', self.manifest['diagnostic_image'], True)
        text = self.command('cat /proc/interrupts; cat /proc/uptime; /stability_probe 35', timeout=45)
        if not re.search(r'^STAB PASS ', text, re.M):
            raise RuntimeError('Timer/memory smoke probe did not pass')
        # Background probe retains UART shell access for independent console checks.
        self.phase = 'timer-memory-soak'
        expected_pass = self.pass_count + 1
        self.command(f'/stability_probe {self.soak} > /dev/console 2>&1 & echo STAB_PROBE_LAUNCHED')
        started = time.monotonic()
        deadline = started + self.soak + 180
        next_console = started + 45
        last_probe = started
        last_guest_elapsed = 0.0
        last_wakes = 0
        previous_timer = None
        soak_pass = False
        while time.monotonic() < deadline:
            text = self.capture()
            heartbeat = self.latest_heartbeat
            if heartbeat and heartbeat[2] > last_probe:
                elapsed, wakes, observed = heartbeat
                if elapsed <= last_guest_elapsed or wakes <= last_wakes:
                    raise RuntimeError('Guest heartbeat stopped progressing')
                if abs((observed - started) - elapsed) > max(5, elapsed * 0.02):
                    raise RuntimeError('Guest monotonic clock differs from host by >2%/5s')
                last_guest_elapsed, last_wakes, last_probe = elapsed, wakes, observed
            if self.pass_count >= expected_pass:
                soak_pass = True
                break
            if time.monotonic() - last_probe > 90:
                raise TimeoutError('No progressing guest probe heartbeat for 90 seconds')
            if time.monotonic() >= next_console:
                output = self.command('echo STAB_UART_OK; cat /proc/interrupts; cat /proc/uptime')
                # Timer IRQ count in the single CPU's first /proc/interrupts column.
                timer = re.search(r'^\s*\d+:\s+(\d+)\s+.*arch_timer', output, re.M)
                if not timer:
                    raise RuntimeError('Missing arch_timer IRQ counter')
                count = int(timer[1])
                if previous_timer is not None and count <= previous_timer:
                    raise RuntimeError('Virtual timer interrupt count stopped progressing')
                previous_timer = count
                self.host_checks += 1
                self.event('console_pass', timer_interrupts=count, check=self.host_checks)
                if self.pass_count >= expected_pass:
                    soak_pass = True
                    break
                next_console += 60
                self.status('RUNNING')
            time.sleep(1)
        if not soak_pass or last_guest_elapsed < self.soak - 90:
            raise RuntimeError('Full-duration soak did not complete')
        self.command('echo STAB_POST_SOAK_OK; cat /proc/interrupts; cat /proc/uptime')
        self.event('soak_pass', elapsed_host=time.monotonic() - started)
        self.phase = 'post-soak-boots'
        for i in range(self.after_boots):
            self.boot(f'after-soak-{i+1:02}', self.manifest['diagnostic_image'], True)
            text = self.command('/stability_probe 5', timeout=15)
            if not re.search(r'^STAB PASS ', text, re.M):
                raise RuntimeError('Post-soak timer test did not pass')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('directory')
    parser.add_argument('--mode', choices=['quick', 'overnight'], required=True)
    parser.add_argument('--soak-seconds', type=int)
    parser.add_argument('--authorize-hardware', action='store_true', help='Confirm the user authorized this board test, including power cycles and final power off')
    args = parser.parse_args()
    if not args.authorize_hardware:
        parser.error('Hardware run requires user authorization and --authorize-hardware')
    if args.soak_seconds is None:
        args.soak_seconds = 600 if args.mode == 'quick' else 21600
    if not 120 <= args.soak_seconds <= 25200:
        parser.error('soak must be between 120 seconds and seven hours')
    run = Run(args.directory, args.soak_seconds, args.mode)
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(InterruptedError('Test service stopped')))
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
    return 0 if outcome == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
