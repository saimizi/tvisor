#!/usr/bin/env python3
"""Build a Phase 11.5 image and reuse verified immutable Linux fixtures.

No hardware access, TFTP publication, Linux rebuild, or previous bundle edits.
The selected Cargo build runs immediately before copying its ELF, preventing
accidental reuse of another acceptance feature from Cargo's shared target path.
"""
import argparse
import datetime as dt
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import uuid


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', type=Path, required=True, help='Existing frozen quick regression bundle')
    parser.add_argument('--output', type=Path, required=True, help='New result directory')
    parser.add_argument('--kind', choices=['normal', 'stress', 'timeout'], required=True)
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    fixture, output = args.fixture.resolve(), args.output.resolve()
    original = json.loads((fixture / 'manifest.json').read_text())
    runner = repo / 'scripts/stability/run.py'
    if original['mode'] != 'quick' or sha(runner) != original['runner_sha256']:
        raise RuntimeError('Fixture must use the current quick regression runner')
    for key in ['baseline_image', 'diagnostic_image']:
        path = fixture / original[key]
        expected = next(item for item in original['files'] if Path(item['path']).name == path.name)
        if path.stat().st_size != expected['size'] or sha(path) != expected['sha256']:
            raise RuntimeError('Frozen fixture changed: ' + str(path))
    for name, expected in original['config_sha256'].items():
        if sha(fixture / name) != expected:
            raise RuntimeError('Frozen configuration changed: ' + name)
    if sha(repo / 'scripts/stability/probe.c') != original['probe_source_sha256']:
        raise RuntimeError('Current probe source differs from the frozen fixture provenance')
    if sha(fixture / 'stability_probe') != original['probe_sha256']:
        raise RuntimeError('Frozen probe changed')
    if output.exists():
        raise RuntimeError('Output must be a new directory')
    feature = {'normal': None, 'stress': 'all-core-stress-test', 'timeout': 'all-core-timeout-test'}[args.kind]
    profile = 'release' if args.kind == 'stress' else 'debug'
    build = ['cargo', 'build', '--locked', '--offline', '--target', 'aarch64-unknown-none']
    if feature:
        build += ['--features', feature]
    if profile == 'release':
        build += ['--release']
    if args.dry_run:
        print(json.dumps(dict(build=build, fixture=str(fixture), output=str(output), hardware_actions=False), indent=2))
        return
    output.mkdir(parents=True)

    def command(arguments, logfile):
        with (output / logfile).open('w') as stream:
            subprocess.run(arguments, cwd=repo, stdout=stream, stderr=subprocess.STDOUT, check=True)

    command(['cargo', 'fmt', '--check'], 'format.log')
    command(build, 'build.log')
    elf = repo / 'target/aarch64-unknown-none' / profile / 'tvisor'
    # Check the same staging bounds as the original fixture preparation.
    symbols = {}
    for line in subprocess.check_output(['llvm-nm', '-n', str(elf)], text=True).splitlines():
        fields = line.split()
        if len(fields) == 3:
            symbols[fields[2]] = int(fields[0], 16)
    if symbols['__image_start'] != 0x04000000 or symbols['__image_end'] > 0x2eff1000 or 0x04000000 + elf.stat().st_size > 0x2eff1000:
        raise RuntimeError('ELF exceeds the verified staging/runtime window')
    stamp = dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8]
    manifest = dict(original)
    manifest.update(files=[], created=dt.datetime.now(dt.timezone.utc).isoformat(),
                    revision=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
                    dirty=bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=repo)),
                    purpose='Phase 11.5 ' + args.kind, build_command=build, build_profile=profile,
                    fixture_reuse=dict(directory=str(fixture), original_manifest_sha256=sha(fixture / 'manifest.json'), linux_rebuilt=False))
    for key, source, prefix in [('tvisor', elf, 'tvisor'), ('baseline_image', fixture / original['baseline_image'], 'Image-baseline'), ('diagnostic_image', fixture / original['diagnostic_image'], 'Image-diagnostic')]:
        if key != 'tvisor' and 0x30100000 + source.stat().st_size >= 0x36b2b000:
            raise RuntimeError('Guest fixture exceeds verified staging window')
        name = prefix + '-115-' + args.kind + '-' + stamp
        destination = output / name
        shutil.copyfile(source, destination)
        manifest[key] = name
        manifest['files'].append(dict(path='/srv/tftp/' + name, sha256=sha(destination), size=destination.stat().st_size))
    for name in ['baseline-kernel.config', 'diagnostic-kernel.config', 'busybox.config', 'kernel-config.diff', 'stability_probe', 'init', 'initramfs.list']:
        shutil.copyfile(fixture / name, output / name)
    shutil.copyfile(fixture / 'manifest.json', output / 'fixture-original-manifest.json')
    sources = [*repo.glob('src/*.rs'), *repo.glob('src/*.S'), *repo.glob('tvisor_util/*.rs'), *repo.glob('scripts/checks/*.py'), *repo.glob('scripts/stability/*.py'), *repo.glob('scripts/stability/*.c')]
    sources += [repo / path for path in ['Cargo.toml', 'Cargo.lock', '.cargo/config.toml', 'scripts/rpi.ld']]
    hashes = {}
    for path in sources:
        relative = path.relative_to(repo)
        destination = output / 'source' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
        hashes[str(relative)] = sha(destination)
    (output / 'source-manifest.json').write_text(json.dumps(hashes, indent=2) + '\n')
    (output / 'source-changes.patch').write_bytes(subprocess.check_output(['git', 'diff', 'HEAD'], cwd=repo))
    (output / 'source-status.txt').write_bytes(subprocess.check_output(['git', 'status', '--short'], cwd=repo))
    for checker in ['secondary_entry', 'secondary_entry_negative']:
        command(['python3', str(repo / 'scripts/checks' / (checker + '.py')), str(output / manifest['tvisor'])], checker + '.log')
    command(['llvm-readelf', '-l', str(output / manifest['tvisor'])], 'elf-layout.txt')
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print('Prepared ' + str(output) + '; publish only the three manifest artifacts before an authorized run.')


if __name__ == '__main__':
    main()
