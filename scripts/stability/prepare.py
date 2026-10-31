#!/usr/bin/env python3
"""Prepare a frozen tvisor regression bundle; never controls hardware or publishes files."""
import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import uuid


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def embedded_probe(image_data):
    data = Path(image_data).read_bytes()
    if data.startswith(b'\x1f\x8b'):
        data = gzip.decompress(data)
    offset = 0
    while data[offset:offset + 6] in (b'070701', b'070702'):
        header = data[offset:offset + 110]
        size, namesize = int(header[54:62], 16), int(header[94:102], 16)
        name = data[offset + 110:offset + 110 + namesize - 1].decode()
        start = (offset + 110 + namesize + 3) & ~3
        if name.lstrip('./') == 'stability_probe':
            return data[start:start + size]
        offset = (start + size + 3) & ~3
    raise RuntimeError('Cannot find probe in diagnostic initramfs (supports raw/gzip newc)')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--repo', type=Path, required=True)
    p.add_argument('--mode', choices=['quick', 'overnight'], required=True)
    p.add_argument('--output', type=Path)
    p.add_argument('--baseline-image', type=Path)
    p.add_argument('--llvm', default='-22')
    p.add_argument('--jobs', type=int, default=min(4, os.cpu_count() or 1))
    p.add_argument('--dry-run', action='store_true', help='Show prerequisites and intended operations without writing or building')
    args = p.parse_args()
    repo = args.repo.resolve()
    skill = Path(__file__).resolve().parent
    guest = repo / 'guest'
    kernel = guest / 'linux-6.18.2'
    baseline = (args.baseline_image or guest / 'build/arm64/arch/arm64/boot/Image').resolve()
    inputs = [repo / 'Cargo.toml', kernel / 'Makefile', guest / 'build/arm64/.config',
              baseline, guest / 'build/busybox-arm64/busybox', guest / 'build/busybox-arm64/.config', skill / 'probe.c']
    for path in inputs:
        if not path.is_file():
            p.error(f'Missing prerequisite: {path}; build the normal guest first')
        if ' ' in str(path) or '\n' in str(path):
            p.error('The kernel initramfs file-list format requires paths without whitespace')
    for command in ['cargo', 'rustc', 'make', 'aarch64-linux-gnu-gcc', 'nm']:
        if not shutil.which(command):
            p.error(f'Missing tool: {command}')
    stamp = dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8]
    output = (args.output or guest / 'build' / ('regression-' + stamp)).resolve()
    if output.exists():
        p.error('Output already exists; choose a new directory to preserve prior results')
    if any(c.isspace() for c in str(output)):
        p.error('Output path must not contain whitespace')
    if args.jobs < 1:
        p.error('--jobs must be positive')
    if args.dry_run:
        print(json.dumps(dict(repo=str(repo), output=str(output), mode=args.mode,
              operations=['build current tvisor', 'format check and native library tests',
                          'compile static ARM64 probe', 'build isolated diagnostic kernel',
                          'verify embedded probe', 'freeze artifacts and hashes'],
              baseline=str(baseline), llvm=args.llvm, hardware_actions=False), indent=2))
        return
    output.mkdir(parents=True)
    def run(cmd, log='build.log'):
        with open(output / log, 'a') as stream:
            stream.write('\nCOMMAND: ' + repr([str(a) for a in cmd]) + '\n')
            stream.flush()
            subprocess.run([str(a) for a in cmd], cwd=repo, stdout=stream,
                           stderr=subprocess.STDOUT, check=True)
    (output / 'source-status.txt').write_bytes(subprocess.check_output(['git', 'status', '--short'], cwd=repo))
    (output / 'source-changes.patch').write_bytes(subprocess.check_output(['git', 'diff', 'HEAD'], cwd=repo))
    run(['cargo', 'build', '--locked', '--offline', '--target', 'aarch64-unknown-none'])
    run(['cargo', 'fmt', '--check'])
    host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
    run(['cargo', 'test', '--locked', '--offline', '--lib', '--target', host], 'host-tests.log')
    probe = output / 'stability_probe'
    run(['aarch64-linux-gnu-gcc', '-static', '-O2', '-Wall', '-Wextra', '-Werror', skill / 'probe.c', '-o', probe])
    busybox = guest / 'build/busybox-arm64/busybox'
    init = output / 'init'
    init.write_text('''#!/bin/sh
/bin/busybox --install -s /bin
mount -t proc proc /proc || echo STAB_INIT_PROC_FAIL
mount -t sysfs sysfs /sys || echo STAB_INIT_SYS_FAIL
mount -t devtmpfs devtmpfs /dev || echo STAB_INIT_DEV_FAIL
export PS1='root@js-virt:guest# '
echo STABILITY_GUEST_READY
exec /bin/sh
''')
    listing = output / 'initramfs.list'
    listing.write_text(f'''dir /bin 0755 0 0
file /bin/busybox {busybox} 0755 0 0
slink /bin/sh busybox 0777 0 0
file /init {init} 0755 0 0
file /stability_probe {probe} 0755 0 0
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
''')
    build = output / 'kernel-build'
    build.mkdir()
    config = build / '.config'
    shutil.copy2(guest / 'build/arm64/.config', config)
    run([kernel / 'scripts/config', '--file', config, '--enable', 'PROC_FS', '--enable', 'SYSFS', '--set-str', 'INITRAMFS_SOURCE', listing])
    common = ['make', '-C', kernel, f'O={build}', 'ARCH=arm64', f'LLVM={args.llvm}']
    run(common + ['olddefconfig'], 'kernel-build.log')
    run(common + [f'-j{args.jobs}', 'Image'], 'kernel-build.log')
    if embedded_probe(build / 'usr/initramfs_inc_data') != probe.read_bytes():
        raise RuntimeError('Embedded probe differs from the prepared binary')
    tvisor = repo / 'target/aarch64-unknown-none/debug/tvisor'
    symbols = subprocess.check_output(['nm', '-n', tvisor], text=True)
    addresses = {row.split()[-1]: int(row.split()[0], 16) for row in symbols.splitlines() if len(row.split()) == 3}
    if addresses['__image_start'] != 0x04000000 or addresses['__image_end'] > 0x2eff1000 or 0x04000000 + tvisor.stat().st_size > 0x2eff1000:
        raise RuntimeError('Tvisor no longer fits the validated staging/runtime window; review board layout')
    names = {'tvisor': 'tvisor-' + stamp, 'baseline_image': 'Image-baseline-' + stamp,
             'diagnostic_image': 'Image-diagnostic-' + stamp}
    files = []
    for key, source in [('tvisor', tvisor), ('baseline_image', baseline), ('diagnostic_image', build / 'arch/arm64/boot/Image')]:
        if key != 'tvisor' and 0x30100000 + source.stat().st_size >= 0x36b2b000:
            raise RuntimeError('Guest staging image overlaps validated U-Boot reservation')
        dest = output / names[key]
        shutil.copy2(source, dest)
        files.append(dict(path='/srv/tftp/' + dest.name, sha256=sha(dest), size=dest.stat().st_size))
    for source, name in [(guest / 'build/arm64/.config', 'baseline-kernel.config'),
                         (config, 'diagnostic-kernel.config'), (guest / 'build/busybox-arm64/.config', 'busybox.config')]:
        shutil.copy2(source, output / name)
    with open(output / 'kernel-config.diff', 'w') as stream:
        subprocess.run([kernel / 'scripts/diffconfig', output / 'baseline-kernel.config', output / 'diagnostic-kernel.config'], stdout=stream, check=True)
    manifest = dict(names, mode=args.mode, board_id='lab-rpi4', rpictl_socket='/tmp/rpictl-mcp.sock',
                    expected_mac='dc:a6:32:91:38:be', expected_fdt='2eff1f00', files=files,
                    revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
                    created=dt.datetime.now(dt.timezone.utc).isoformat(), probe_sha256=sha(probe),
                    probe_source_sha256=sha(skill / 'probe.c'), runner_sha256=sha(skill / 'run.py'),
                    config_sha256={name: sha(output / name) for name in ['baseline-kernel.config', 'diagnostic-kernel.config', 'busybox.config']})
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(f'Prepared {args.mode} bundle: {output}')
    print('Publish the three files listed in manifest.json to /srv/tftp after reviewing the bundle.')


if __name__ == '__main__':
    main()
