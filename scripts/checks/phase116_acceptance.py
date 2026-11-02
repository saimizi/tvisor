#!/usr/bin/env python3
"""Phase 11.6 mapping, failure, concurrency and final physical-SMP regression.

All board access uses the existing daemon IPC. Cold boots and probe thresholds
come from the standard stability runner. SMP modes require physical gate checks;
quiet mode verifies the ordinary image and absence of acceptance output.
"""
import argparse
from pathlib import Path
import re
import signal
from phase115_acceptance import Acceptance
from run import Run


class ExpectedMappingTimeout(Exception):
    pass


class MappingAcceptance(Acceptance):
    def __init__(self, directory, mode):
        if mode == 'quiet':
            # Quiet-image regression checks guest execution and absence of
            # acceptance output; it does not claim UART-based SMP evidence.
            Run.__init__(self, directory, 600, 'quick')
            self.acceptance_mode = 'quiet'
        elif mode == 'overnight':
            Run.__init__(self, directory, 21600, 'overnight')
            if self.manifest.get('diagnostics_enabled') is False:
                raise RuntimeError('Physical SMP acceptance requires a bundle prepared with --diagnostics')
            self.acceptance_mode = 'quick'  # inherited per-boot resource validation
        else:
            super().__init__(directory, 'stress' if mode == 'stress' else 'bringup')
        self.test_mode = mode
        if mode in ('quick', 'quiet'):
            self.before_boots, self.after_boots, self.expected_boots = 2, 1, 4
            self.soak = 600
        elif mode not in ('overnight',):
            self.expected_boots = 0 if mode == 'mapping-timeout' else 1
        self.phase = 'phase11.6-' + mode

    def wait_text(self, pattern, timeout):
        if self.test_mode == 'mapping-timeout' and pattern == '^BusyBox initramfs ready$':
            Run.wait_text(self, '^Phase 11.6 timeout acceptance PASS ', 20)
            text = self.fresh_log()
            for field in ['CPU2_late_rejected=true', 'old_mapping_retained=true', 'backing_retained=true', 'guest_blocked=true']:
                if field not in text:
                    raise RuntimeError('Missing mapping timeout guarantee: ' + field)
            if 'Linux version ' in text or 'entering one-vCPU Linux guest' in text:
                raise RuntimeError('Guest entered after mapping timeout')
            raise ExpectedMappingTimeout()
        return Run.wait_text(self, pattern, timeout)

    def boot(self, name, image, diagnostic):
        super().boot(name, image, diagnostic)
        if self.test_mode == 'quiet':
            text = self.fresh_log()
            if 'tvisor: entering one-vCPU Linux guest' not in text:
                raise RuntimeError('Missing ordinary guest entry')
            for marker in ['Phase 11.6 remap PASS', 'Phase 11.1 CPU', 'Phase 11.5 all-core management PASS', 'Phase 11.6 physical SMP gate', 'Found RAM region:']:
                if marker in text:
                    raise RuntimeError('Quiet build emitted acceptance diagnostics: ' + marker)
            self.event('quiet_build_boot_pass')
            return
        self.assess_resources()
        text = self.fresh_log()
        if 'Phase 11.6 physical SMP gate online_mask=0xf parked_mask=0xe ' not in text:
            raise RuntimeError('Missing actual Online/Parked SMP gate')
        for cpu in range(1, 4):
            if f'Phase 11.6 CPU{cpu} Parked responsive; retained in mapping membership' not in text:
                raise RuntimeError('Missing responsive parked member')
        # Older frozen bundles performed two startup remaps in normal images.
        expected = self.manifest.get('expected_remaps', 48 if self.test_mode == 'mapping' else 2)
        epochs = [int(value) for value in re.findall(r'^Phase 11.6 remap PASS epoch=(\d+) members=0xf .* old_reusable=true$', text, re.M)]
        if epochs != list(range(1, expected + 1)):
            raise RuntimeError('Incomplete scratch remap epochs: ' + str(epochs))
        if self.test_mode == 'mapping' and 'Phase 11.6 scratch acceptance PASS updates=48 online_and_parked=true old_reuse_checked=true' not in text:
            raise RuntimeError('Missing mapping stress acceptance')
        self.event('physical_smp_mapping_gate_pass', remaps=expected, retained_members=15, parked_workers=14)

    def run(self):
        if self.test_mode in ('quick', 'quiet', 'overnight'):
            return Run.run(self)
        if self.test_mode == 'stress':
            return super().run()
        self.status('RUNNING')
        diagnostic = self.test_mode == 'mapping'
        try:
            self.boot('acceptance', self.manifest['diagnostic_image' if diagnostic else 'baseline_image'], diagnostic)
        except ExpectedMappingTimeout:
            self.event('mapping_timeout_pass')
            return
        if self.test_mode == 'mapping-timeout':
            raise RuntimeError('Fault image returned unexpectedly')
        if diagnostic:
            text = self.command('/stability_probe 35', timeout=45)
            if not re.search(r'^STAB PASS ', text, re.M):
                raise RuntimeError('Post-remap Linux timer/memory probe failed')
        for _ in range(3):
            self.command('echo PHASE116_CONSOLE_OK; uname -a')
        self.event('mapping_acceptance_pass')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory')
    parser.add_argument('--mode', choices=['bringup', 'mapping', 'mapping-timeout', 'stress', 'quiet', 'quick', 'overnight'], required=True)
    parser.add_argument('--authorize-hardware', action='store_true')
    args = parser.parse_args()
    if not args.authorize_hardware:
        parser.error('An authorized hardware run requires --authorize-hardware')
    run = MappingAcceptance(args.directory, args.mode)
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
