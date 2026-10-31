#!/usr/bin/env python3
"""Verify the entry checker rejects specific corruptions of a linked ELF copy."""
import argparse
import contextlib
import io
from pathlib import Path
import re
import struct
import subprocess
import tempfile
import secondary_entry


def check(elf):
    original = Path(elf).read_bytes()
    assert original[:6] == b'\x7fELF\x02\x01', 'ELF64 little-endian required'
    symbols = {}
    for line in subprocess.check_output(['llvm-nm', '-S', elf], text=True).splitlines():
        parts = line.split()
        if len(parts) in (3, 4):
            symbols[parts[-1]] = int(parts[0], 16)
    phoff = struct.unpack_from('<Q', original, 32)[0]
    phsize, phnum = struct.unpack_from('<HH', original, 54)

    def offset(address):
        for i in range(phnum):
            kind, _, file, va, _, size, _, _ = struct.unpack_from('<IIQQQQQQ', original, phoff + i * phsize)
            if kind == 1 and va <= address and address + 4 <= va + size:
                return file + address - va
        raise AssertionError('instruction not in a file-backed LOAD segment')

    start = symbols['__secondary_entry']
    listing = subprocess.check_output(['llvm-objdump', '-d', '--no-show-raw-insn',
        f'--start-address={start}', f"--stop-address={symbols['__secondary_rejected']}", elf], text=True)

    def find(needle):
        return next(int(re.match(r'\s*([0-9a-f]+):', line)[1], 16)
                    for line in listing.splitlines() if needle in line)

    mask = find('movk\tx10, #255, lsl #32')
    acquire = find('ldar\tx10, [x9]')
    # Replacement opcodes are A64 STR XZR,[SP], BL, LDR X10,[X9], NOP and STR W1,[X0].
    call_pc = start + 4
    call = 0x94000000 | (((symbols['secondary_main'] - call_pc) // 4) & 0x3ffffff)
    store = symbols['__secondary_store_cell'] + 8  # ISB, MRS, STR
    mutations = [
        ('stack store before stack validation', start + 4, 0xf90003ff),
        ('Rust call before acquire', call_pc, call),
        ('wrong full-affinity mask', mask, struct.unpack_from('<I', original, offset(mask))[0] ^ (1 << 5)),
        ('ordinary load instead of LDAR', acquire, 0xf940012a),
        ('missing cache clean', symbols['__secondary_clean_line'], 0xd503201f),
        ('32-bit release store', store, 0xb9000001),
    ]
    with contextlib.redirect_stdout(io.StringIO()):
        secondary_entry.check(elf)
    with tempfile.TemporaryDirectory(prefix='tvisor-entry-negative-') as directory:
        for name, address, instruction in mutations:
            data = bytearray(original)
            struct.pack_into('<I', data, offset(address), instruction)
            candidate = Path(directory) / 'mutated.elf'
            candidate.write_bytes(data)
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    secondary_entry.check(str(candidate))
            except (AssertionError, StopIteration, KeyError):
                print('PASS: rejected ' + name)
            else:
                raise AssertionError('checker accepted ' + name)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('elf')
    check(parser.parse_args().elf)
