#!/usr/bin/env python3
"""Check the linked scratch maintenance helper and reject broken sequences."""
import re
import subprocess
import sys


EXPECTED = [
    ('dc', 'cvac, x0'),
    ('dsb', 'ish'),
    ('tlbi', 'alle2is'),
    ('dsb', 'ish'),
    ('isb', ''),
    ('ret', ''),
]


def check(sequence):
    if sequence != EXPECTED:
        raise ValueError('Incorrect EL2 descriptor publication/TLBI completion: ' + repr(sequence))


def main():
    output = subprocess.check_output(
        ['llvm-objdump', '-d', '--no-show-raw-insn', '--disassemble-symbols=__el2_scratch_sync', sys.argv[1]],
        text=True,
    )
    sequence = []
    for line in output.splitlines():
        match = re.match(r'\s*[0-9a-f]+:\s+(\S+)(?:\s+(.*))?$', line)
        if match:
            sequence.append((match[1].lower(), (match[2] or '').strip().lower()))
    check(sequence)
    mutations = [EXPECTED[:i] + EXPECTED[i + 1:] for i in range(len(EXPECTED))]
    mutations += [EXPECTED[:2] + [('tlbi', 'alle2')] + EXPECTED[3:]]
    mutations += [EXPECTED[:1] + [EXPECTED[2], EXPECTED[1]] + EXPECTED[3:]]
    mutations += [EXPECTED[:3] + [('dsb', 'ishst')] + EXPECTED[4:]]
    for sequence in mutations:
        try:
            check(sequence)
        except ValueError:
            continue
        raise AssertionError('Broken maintenance sequence accepted')
    print(f'PASS: linked EL2 scratch publication/TLBI helper; {len(mutations)} negative mutations rejected')


if __name__ == '__main__':
    main()
