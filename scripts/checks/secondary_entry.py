#!/usr/bin/env python3
"""Check structural safety properties of the linked secondary trampoline.

Requires LLVM nm/objdump. Does not prove mappings, coherency or CPU behavior.
"""
import argparse
import re
import subprocess


def check(elf):
    def run(*args):
        return subprocess.check_output(args, text=True)

    symbols = {}
    sizes = {}
    for line in run('llvm-nm', '-S', elf).splitlines():
        parts = line.split()
        if len(parts) in (3, 4):
            symbols[parts[-1]] = int(parts[0], 16)
            if len(parts) == 4: sizes[parts[-1]] = int(parts[1], 16)
    start = symbols['__secondary_entry']
    stack = symbols['__secondary_stack_ready']
    boundary = symbols['__secondary_rust_boundary']
    reject = symbols['__secondary_rejected']
    assert start < stack < boundary < reject
    assert start % 64 == 0
    assert symbols['__text_start'] <= start < reject + 8 <= symbols['__text_end']
    assert symbols['__vectors_start'] <= symbols['__el2_vectors'] < symbols['__vectors_end']
    assert symbols['__el2_vectors'] % 2048 == 0
    assert 'SECONDARY_RECORD' not in symbols
    for name, expected_size in [('PCPU_SLOTS', 4 * 384), ('HOST_EL2_CONFIG', 128)]:
        assert sizes[name] == expected_size
        assert symbols['__writable_start'] <= symbols[name]
        assert symbols[name] + sizes[name] <= symbols['__writable_end']
        assert symbols[name] % 64 == 0
    lookup = symbols['__secondary_lookup']
    text = run('llvm-objdump', '-d', '--no-show-raw-insn',
               f'--start-address={start}', f'--stop-address={reject + 8}', elf)
    instructions = []
    for line in text.splitlines():
        m = re.match(r'^\s*([0-9a-f]+):\s+(\S+)\s*(.*?)\s*$', line)
        if m:
            instructions.append((int(m[1], 16), m[2], m[3]))
    assert len(instructions) * 4 == reject + 8 - start, 'undecoded data or gap'
    calls = [(a, op, args) for a, op, args in instructions if op in ('bl', 'blr')]
    assert len(calls) == 1 and calls[0][0] >= boundary
    assert '<secondary_main>' in calls[0][2]
    assert instructions[-2][1] == 'wfe' and instructions[-1][1] == 'b'
    assert int(instructions[-1][2].split()[0], 16) == reject
    before = [(a, op, args) for a, op, args in instructions if a < boundary]
    # No stack memory access, store, call, indirect branch or literal-pool read.
    allowed = {'msr', 'mrs', 'mov', 'movk', 'movz', 'ubfx', 'cmp', 'tst',
               'tbz', 'tbnz', 'b.ne', 'b.hs', 'b.ls', 'cbz', 'adr', 'adrp',
               'add', 'and', 'ldr', 'subs', 'dsb', 'tlbi', 'ic', 'isb', 'ldar', 'nop', 'cbnz', 'b.lo'}
    record_loads = {'x10, [x15, #48]', 'x10, [x15, #24]',
                    'x9, [x19, #16]', 'x20, [x19, #64]',
                    'x9, [x19, #40]', 'x10, [x19, #32]',
                    'x10, [x17, #16]', 'x9, [x17]', 'x9, [x17, #8]',
                    'x9, [x17, #16]', 'x9, [x17, #24]'}
    assert instructions[0][1:] == ('msr', 'DAIFSet, #15')
    assert before[1][1:] == ('mrs', 'x9, CurrentEL')
    assert before[2][1:] == ('cmp', 'x9, #8')
    for address, op, args in before:
        assert op in allowed, (op, args)
        if op == 'ldr':
            assert args in record_loads, args
        if op == 'ldar':
            assert args == 'x10, [x9]'
        if op in ('b.ne', 'b.hs', 'b.ls', 'tbz', 'tbnz', 'cbz', 'cbnz', 'b.lo'):
            target = re.findall(r'0x[0-9a-f]+', args)[0]
            destination = int(target, 16)
            if lookup <= address < lookup + 52:
                assert destination in (reject, lookup, lookup + 36), args
            else:
                assert destination == reject, args
    def index(op, operand):
        return next(i for i, (_, o, args) in enumerate(before) if o == op and operand in args)
    assert index('mrs', 'CurrentEL') < index('mrs', 'SCTLR_EL2') < index('mrs', 'MIDR_EL1')
    assert index('mrs', 'MIDR_EL1') < index('mrs', 'S3_1_C15_C2_1') < index('ldr', 'x20')
    sp = index('mov', 'sp, x9')
    assert before[sp][0] + 4 == stack
    assert index('tst', 'x12, #0x3f') < sp  # context alignment
    assert index('msr', 'TTBR0_EL2') < index('tlbi', 'alle2') < index('msr', 'SCTLR_EL2') < index('ldar', '[x9]')
    operations = [(op, args.split(' //')[0].rstrip()) for _, op, args in before]
    mask = [('mov', 'x10, #65535'), ('movk', 'x10, #255, lsl #16'), ('movk', 'x10, #255, lsl #32')]
    assert any(operations[i:i + 3] == mask for i in range(len(operations) - 2)), 'full affinity mask'
    assert before[index('msr', 'SPSel') + 1][1] == 'isb'
    # Resolve both relaxed ADR and unrelaxed ADRP/ADD address construction.
    for reg, symbol in [('x15', 'PCPU_SLOTS'), ('x17', 'HOST_EL2_CONFIG'), ('x9', '__el2_vectors')]:
        addresses = []
        for i, (address, op, args) in enumerate(before):
            if op == 'adr' and args.startswith(reg + ', #'):
                addresses.append(address + int(args.split('#')[1], 0))
            if op == 'adrp' and args.startswith(reg + ', '):
                page = int(args.split(', ')[1].split()[0], 0)
                _, next_op, next_args = before[i + 1]
                m = re.fullmatch(reg + ', ' + reg + r', #(\d+)', next_args)
                assert next_op == 'add' and m, 'unexpected ADRP consumer'
                addresses.append(page + int(m[1]))
        assert symbols[symbol] in addresses, f'{symbol}: incorrect address materialization'
    lookup_ops = [(op, args.split(' //')[0].rstrip()) for address, op, args in before if lookup <= address < lookup + 52]
    normalized_lookup = [(op, args.split(' <')[0]) for op, args in lookup_ops]
    assert normalized_lookup == [
        ('ldr','x10, [x15, #48]'), ('cmp','x10, #1'), ('b.ne',hex(lookup + 36)),
        ('ldr','x10, [x15, #24]'), ('cmp','x10, x14'), ('b.ne',hex(lookup + 36)),
        ('cbnz','x19, ' + hex(reject)), ('mov','x19, x15'), ('mov','x18, x16'),
        ('add','x15, x15, #384'), ('add','x16, x16, #1'), ('cmp','x16, #4'), ('b.lo',hex(lookup))
    ], 'unbounded or incorrect full-affinity slot scan'
    assert ('and','x14, x9, x10') in operations
    assert ('mov','x16, #0') in operations and ('mov','x19, xzr') in operations
    assert ('cmp','x9, x18') in operations and ('mov','x12, x19') in operations
    required = [('tst', 'x9, #0x1'), ('tbnz', 'w9, #2'), ('tbnz', 'w9, #25'),
                ('ubfx', 'x10, x9, #24, #8'), ('cmp', 'x10, #65'),
                ('ubfx', 'x10, x9, #4, #12'), ('mov', 'x11, #3336'), ('cmp', 'x10, x11'),
                ('tbz', 'w9, #6')]
    for op, prefix in required:
        assert any(o == op and args.startswith(prefix) for _, o, args in before), (op, prefix)
    assert [(op, args) for _, op, args in before[-4:-1]] == [
        ('add', 'x9, x19, #64'), ('ldar', 'x10, [x9]'), ('cmp', 'x10, x20')]
    ttbr = index('msr', 'TTBR0_EL2')
    assert [(op, args) for _, op, args in before[ttbr:ttbr + 10]] == [
        ('msr', 'TTBR0_EL2, x9'), ('isb', ''), ('dsb', 'sy'), ('tlbi', 'alle2'),
        ('ic', 'iallu'), ('dsb', 'sy'), ('isb', ''), ('ldr', 'x9, [x17, #24]'),
        ('msr', 'SCTLR_EL2, x9'), ('isb', '')]
    boundary_ops = [(op, args) for address, op, args in instructions if boundary <= address < reject]
    assert boundary_ops[:6] == [('mov', 'x0, x19'), ('mov', 'x1, x21'), ('mov', 'x2, x22'), ('mov', 'x3, x23'), ('mov', 'x4, x24'), ('mov', 'x5, x20')]
    # Both CPUs must install the same explicit host baseline before Rust.
    primary = run('llvm-objdump', '-d', '--no-show-raw-insn',
                  f"--start-address={symbols['__enter_private_el2']}",
                  f"--stop-address={symbols['__enter_private_el2'] + sizes['__enter_private_el2']}", elf)
    primary_ops = []
    for line in primary.splitlines():
        m = re.match(r'^\s*([0-9a-f]+):\s+(\S+)\s*(.*?)\s*$', line)
        if m: primary_ops.append((m[2], m[3]))
    common = [('mov','x9, #2147483648'),('msr','HCR_EL2, x9'),
              ('mov','x9, #13311'),('msr','CPTR_EL2, x9'),
              ('mov','x9, #0'),('msr','HSTR_EL2, x9'),
              ('mov','x9, #0'),('msr','VTTBR_EL2, x9'),
              ('mov','x9, #0'),('msr','CNTVOFF_EL2, x9'),
              ('mov','x9, #3'),('msr','CNTHCTL_EL2, x9'),
              ('mov','x9, #0'),('msr','CNTHP_CTL_EL2, x9'),
              ('msr','CNTP_CTL_EL0, x9'),('msr','CNTV_CTL_EL0, x9'),('isb','')]
    for ops in [operations, primary_ops]:
        assert any(ops[j:j + len(common)] == common for j in range(len(ops))), 'host control policy drift'
    assert before[-1][1] == 'b.ne'   # generation check precedes Rust boundary
    publication = {
        '__secondary_clean_line': [('dc', 'cvac, x0'), ('ret', '')],
        '__secondary_publication_barrier': [('dsb', 'sy'), ('ret', '')],
        '__secondary_read_cell': [('ldr', 'x0, [x0]'), ('ret', '')],
        '__secondary_store_cell': [('isb', ''), ('mrs', 'x2, CNTPCT_EL0'), ('str', 'x1, [x0]'), ('mov', 'x0, x2'), ('ret', '')],
        '__secondary_send_event': [('sev', ''), ('ret', '')],
    }
    for name, expected in publication.items():
        address = symbols[name]
        length = len(expected) * 4
        assert sizes[name] == length and symbols['__text_start'] <= address < address + length <= symbols['__text_end']
        decoded = run('llvm-objdump', '-d', '--no-show-raw-insn', f'--start-address={address}', f'--stop-address={address + length}', elf)
        actual = []
        for line in decoded.splitlines():
            m = re.match(r'^\s*([0-9a-f]+):\s+(\S+)\s*(.*?)\s*$', line)
            if m: actual.append((m[2], m[3]))
        assert actual == expected, (name, actual)
    print(text)
    print('PASS: linked entry structure, bounded slot lookup/rejection, memory operands, ABI regions, boundary ordering and publication primitives')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('elf')
    check(parser.parse_args().elf)
