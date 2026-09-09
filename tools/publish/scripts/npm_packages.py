#!/usr/bin/env python3
"""Assemble npm packages from checked native release inputs; never publish."""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
from native_artifacts import ROOT, TARGETS, digest, read_checked, unpack_checked, version

def npm_command() -> list[str]:
    node, npm = shutil.which('node'), shutil.which('npm')
    if not node or not npm:
        raise ValueError('Node 24 and npm 11.5.1 or newer are required for packaging')
    path = Path(npm).resolve()
    candidates = [path] if path.suffix in ('.js', '.cjs') else []
    candidates += [path.parent / 'node_modules/npm/bin/npm-cli.js', Path(node).parent / 'node_modules/npm/bin/npm-cli.js']
    for candidate in candidates:
        if candidate.is_file():
            return [node, str(candidate)]
    raise ValueError('Cannot locate npm-cli.js beside Node/npm; no shell fallback is used')

def key(manifest: dict) -> str:
    return f"{manifest['os']}-{manifest['arch']}" + ('-gnu' if manifest['os'] == 'linux' else '')

def write_json(path: Path, value) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n', encoding='utf-8')

def dist_tag(release_version: str, channel: str) -> str:
    if channel == 'stable': return 'latest'
    if re.fullmatch(r'\d+\.\d+\.\d+-alpha\.(0|[1-9][0-9]*)', release_version): return 'alpha'
    return 'next'

def assemble(directory: Path, output: Path, channel: str) -> Path:
    complete = json.loads(read_checked(directory / 'usagestat-artifacts.json'))
    settings = json.loads((ROOT / 'npm/distribution.json').read_text())
    if complete['schemaVersion'] != 1 or complete['version'] != version():
        raise ValueError('Native artifact schema/version does not match the source checkout')
    if channel == 'prerelease' and '-' not in complete['version']:
        raise ValueError('A prerelease publication requires a prerelease version')
    if channel == 'stable' and '-' in complete['version']:
        raise ValueError('Stable publication cannot contain a prerelease version')
    manifests = [manifest for manifest in complete['targets'] if channel != 'stable' or manifest['minimumSystemQualified']]
    if not manifests or len({m['target'] for m in manifests}) != len(manifests):
        raise ValueError('Native inputs are empty or contain duplicate targets')
    for manifest in manifests:
        metadata = TARGETS[manifest['target']]
        if (manifest['os'], manifest['arch'], manifest['libc']) != (metadata['os'], metadata['arch'], metadata['libc']):
            raise ValueError('Invalid native platform metadata')
        if manifest['version'] != complete['version'] or manifest['sourceCommit'] != complete['sourceCommit'] or manifest['sourceDirty']:
            raise ValueError('Native inputs must have matching clean versions and commits')
        if manifest['resourcesSha256'] != manifests[0]['resourcesSha256']:
            raise ValueError('Native targets have different bundled resources')
        if any(item['path'].endswith(('.test.js', '.spec.js')) or '/fixtures/' in item['path'] for item in manifest['files']):
            raise ValueError('Native inputs contain development fixtures; rebuild with the runtime resource allowlist')
        if manifest['archive']['name'] != metadata['asset'] + ('.zip' if manifest['os'] == 'win32' else '.tar.gz'):
            raise ValueError('Unexpected native archive name')
    output.mkdir(parents=True, exist_ok=False)
    common = dict(version=complete['version'], license='MIT', repository={'type': 'git', 'url': settings['repository']},
        engines={'node': settings['node'], 'npm': settings['npm']},
        publishConfig={'access': 'public', 'registry': settings['registry']}, private=channel == 'rehearsal')
    platforms, optional, packages = {}, {}, []
    for manifest in manifests:
        target = key(manifest)
        name = settings['platformPrefix'] + target
        package = output / target
        unpack_checked(manifest, directory / manifest['archive']['name'], package)
        write_json(package / 'native-manifest.json', manifest)
        files = [item['path'] for item in manifest['files']] + ['native-manifest.json']
        package_json = dict(common, name=name, description=f"Native usagestat backend for {target}",
            os=[manifest['os']], cpu=[manifest['arch']], files=files)
        if manifest['libc']: package_json['libc'] = [manifest['libc']]
        write_json(package / 'package.json', package_json)
        platforms[target] = {'package': name, 'manifestSha256': digest((package / 'native-manifest.json').read_bytes()),
            'minimumSystem': manifest['minimumSystem'], 'minimumSystemQualified': manifest['minimumSystemQualified']}
        optional[name] = complete['version']
        packages.append({'name': name, 'directory': target, 'role': 'platform', 'files': sorted(files + ['package.json'])})
    main = output / 'main'
    (main / 'bin').mkdir(parents=True)
    shutil.copyfile(ROOT / 'npm/launcher.cjs', main / 'launcher.cjs')
    shutil.copyfile(ROOT / 'LICENSE', main / 'LICENSE')
    shutil.copyfile(ROOT / 'npm/README.md', main / 'README.md')
    for command in ['usagestat', 'usagestatd']:
        script = main / 'bin' / (command + '.cjs')
        script.write_text(f"#!/usr/bin/env node\n'use strict';\nrequire('../launcher.cjs').launch('{command}');\n", encoding='utf-8')
        script.chmod(0o755)
    write_json(main / 'platforms.json', platforms)
    write_json(main / 'package.json', dict(common, name=settings['name'], description='Native agent usage backend CLI and daemon',
        bin={command: f'bin/{command}.cjs' for command in ['usagestat', 'usagestatd']},
        files=['bin', 'launcher.cjs', 'platforms.json', 'LICENSE', 'README.md'], optionalDependencies=optional))
    packages.append({'name': settings['name'], 'directory': 'main', 'role': 'main',
        'files': ['LICENSE', 'README.md', 'bin/usagestat.cjs', 'bin/usagestatd.cjs', 'launcher.cjs', 'package.json', 'platforms.json']})
    result = output / 'npm-packages.json'
    write_json(result, {'schemaVersion': 1, 'version': complete['version'], 'sourceCommit': complete['sourceCommit'],
        'channel': channel, 'distTag': dist_tag(complete['version'], channel), 'packages': packages})
    return result

def pack(manifest_path: Path) -> Path:
    plan = json.loads(manifest_path.read_text())
    directory = manifest_path.parent
    tarballs = directory / 'tarballs'
    tarballs.mkdir(exist_ok=False)
    for package in plan['packages']:
        packed = json.loads(subprocess.check_output([*npm_command(), 'pack', '--json', '--ignore-scripts',
            '--pack-destination', str(tarballs.resolve())], cwd=directory / package['directory'], text=True))
        if len(packed) != 1 or sorted(file['path'] for file in packed[0]['files']) != sorted(package['files']):
            raise ValueError(f"npm pack included missing or unexpected files: {package['name']}")
        tarball = tarballs / packed[0]['filename']
        data = tarball.read_bytes()
        package.update(tarball='tarballs/' + tarball.name, sha256=digest(data), integrity=packed[0]['integrity'],
            packageJson=json.loads((directory / package['directory'] / 'package.json').read_text()))
        tarball.with_name(tarball.name + '.sha256').write_text(f'{digest(data)}  {tarball.name}\n')
    result = directory / 'npm-packed.json'
    write_json(result, plan)
    return result

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--channel', choices=['rehearsal', 'stable', 'prerelease'], default='rehearsal')
    parser.add_argument('--pack', type=Path)
    args = parser.parse_args()
    if args.pack: print(pack(args.pack.resolve()))
    elif args.artifacts and args.output: print(assemble(args.artifacts.resolve(), args.output.resolve(), args.channel))
    else: parser.error('provide --artifacts/--output or --pack')
