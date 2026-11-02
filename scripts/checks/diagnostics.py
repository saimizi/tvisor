#!/usr/bin/env python3
"""Check build-time diagnostics and scratch acceptance isolation in a linked ELF."""
import argparse
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('elf', type=Path)
    parser.add_argument('--enabled', action='store_true')
    parser.add_argument('--scratch', action='store_true')
    args = parser.parse_args()
    if args.scratch and not args.enabled:
        parser.error('Scratch acceptance must enable diagnostics')
    data = args.elf.read_bytes()
    for marker in [b'Phase 11.1 CPU', b'Phase 11.5 all-core management PASS', b'Phase 11.6 physical SMP gate']:
        assert (marker in data) == args.enabled, (marker, args.enabled)
    for marker in [b'Phase 11.6 scratch VA=', b'Phase 11.6 remap PASS epoch=']:
        assert (marker in data) == args.scratch, (marker, args.scratch)
    if not args.enabled:
        for marker in [b'Found RAM region:', b'Phase 7 checkpoint', b'Phase 10: loading Linux Image']:
            assert marker not in data, marker
    symbols = subprocess.check_output(['llvm-nm', '--defined-only', str(args.elf)], text=True)
    assert ('__el2_scratch_sync' in symbols) == args.scratch
    # Quiet builds still retain actionable failure reporting.
    assert b'management activation failed' in data and b'PANIC:' in data
    print(f'PASS: diagnostics={args.enabled} scratch_acceptance={args.scratch}; failure diagnostics retained')


if __name__ == '__main__':
    main()
