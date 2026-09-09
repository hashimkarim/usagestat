#!/usr/bin/env python3
"""Verify npm publication inputs and recover exact-version partial publication."""
from __future__ import annotations
import argparse
import base64
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from npm_packages import ROOT, npm_command, dist_tag

def validate(plan: dict, directory: Path) -> list[dict]:
    if plan['schemaVersion'] != 1 or plan['channel'] not in ('stable', 'prerelease'):
        raise ValueError('Only explicit stable/prerelease package inputs may be published')
    if ('-' in plan['version']) != (plan['channel'] == 'prerelease') or plan['distTag'] != dist_tag(plan['version'], plan['channel']):
        raise ValueError('Version and npm release channel disagree')
    packages = plan['packages']
    mains = [p for p in packages if p['role'] == 'main']
    platforms = [p for p in packages if p['role'] == 'platform']
    if len(mains) != 1 or not platforms or len({p['name'] for p in packages}) != len(packages):
        raise ValueError('Expected unique platform packages and one main package')
    if mains[0]['packageJson']['optionalDependencies'] != {p['name']: plan['version'] for p in platforms}:
        raise ValueError('Main optional dependencies must exactly match the staged platform packages')
    for package in packages:
        meta = package['packageJson']
        if meta['name'] != package['name'] or meta['version'] != plan['version'] or meta.get('private') or meta.get('scripts'):
            raise ValueError('Invalid or private npm publication metadata')
        relative = Path(package['tarball'])
        if relative.is_absolute() or '..' in relative.parts:
            raise ValueError('Invalid package tarball path')
        data = (directory / relative).read_bytes()
        integrity = 'sha512-' + base64.b64encode(hashlib.sha512(data).digest()).decode()
        if hashlib.sha256(data).hexdigest() != package['sha256'] or integrity != package['integrity']:
            raise ValueError('Packed npm integrity mismatch')
    return platforms + mains

def registry_package(registry: str, name: str):
    url = registry.rstrip('/') + '/' + urllib.parse.quote(name, safe='@')
    try:
        request = urllib.request.Request(url, headers={'Cache-Control': 'no-cache'})
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read(16 * 1024 * 1024 + 1)
            if len(data) > 16 * 1024 * 1024: raise ValueError('Registry metadata exceeds the size limit')
            return json.loads(data)
    except urllib.error.HTTPError as error:
        if error.code == 404: return None
        raise ValueError(f'Registry verification failed with HTTP {error.code}') from None

def version_order(value: str):
    match = re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?', value)
    if not match: raise ValueError('Invalid release version in registry metadata')
    pre = match[4]
    tokens = pre.split('.') if pre else []
    if any(not token or (token.isdigit() and len(token) > 1 and token.startswith('0')) for token in tokens):
        raise ValueError('Invalid prerelease version in registry metadata')
    return tuple(int(match[i]) for i in (1, 2, 3)), (1 if pre is None else 0), tuple((0, int(token)) if token.isdigit() else (1, token) for token in tokens)

def publication_state(package: dict, document, version: str, tag: str) -> bool:
    if document is None: return False
    tagged = document.get('dist-tags', {}).get(tag)
    if tagged and version_order(tagged) > version_order(version):
        raise ValueError(f"Refusing to move {package['name']}:{tag} backwards from {tagged}")
    exists = existing_matches(package, document.get('versions', {}).get(version))
    if exists and tagged != version:
        raise ValueError(f"Identical {package['name']}@{version} exists under a different tag; verify and promote it with authenticated npm dist-tag before retrying")
    return exists

def existing_matches(package: dict, remote) -> bool:
    if remote is None: return False
    if remote.get('dist', {}).get('integrity') != package['integrity']:
        raise ValueError(f"Refusing to replace different published bytes: {package['name']}")
    for field in ['name', 'version', 'optionalDependencies', 'os', 'cpu', 'libc', 'bin']:
        if remote.get(field) != package['packageJson'].get(field):
            raise ValueError(f"Published package metadata differs: {package['name']} ({field})")
    return True

def wait_for_publication(registry, package, version, tag):
    # New npm packages can remain absent from read replicas after a successful PUT.
    for attempt in range(36):
        document = registry_package(registry, package['name'])
        if publication_state(package, document, version, tag): return document
        if attempt == 0: print(f"Waiting for {package['name']} to become visible in the public registry", flush=True)
        time.sleep(5)
    raise ValueError('Published package was not visible; retain these exact inputs and inspect before retrying')

def check_default_tag(previous, current, version, tag):
    if tag == 'latest': return
    old = (previous or {}).get('dist-tags', {}).get('latest')
    new = current.get('dist-tags', {}).get('latest')
    if new == old: return
    # npm can assign a new package's only version as its default even when
    # publish explicitly requested alpha. Preserve every existing default.
    if previous is None and new == version and set(current['versions']) == {version}:
        return
    raise ValueError('Prerelease publication changed an existing npm default tag')

def run(manifest: Path, publish: bool, bootstrap: bool = False) -> None:
    settings = json.loads((ROOT / 'npm/distribution.json').read_text())
    plan = json.loads(manifest.read_text())
    packages = validate(plan, manifest.parent)
    if settings['name'] != packages[-1]['name'] or any(not p['name'].startswith(settings['platformPrefix']) for p in packages[:-1]):
        raise ValueError('Package names differ from the configured namespace')
    if bootstrap:
        if not publish or settings['publicationEnabled'] or os.environ.get('GITHUB_ACTIONS') == 'true' or plan['distTag'] != 'alpha':
            raise ValueError('Bootstrap is a local first-alpha publication before trusted publishing is enabled')
        identity = subprocess.check_output([*npm_command(), 'whoami', '--registry', settings['registry']], text=True).strip()
        if identity != 'hashimkarim': raise ValueError('First publication requires the configured namespace owner')
    if publish and not bootstrap and not settings['publicationEnabled']:
        raise ValueError('First publication and npm trusted-publisher setup are pending; publicationEnabled is false')
    registry = settings['registry']
    if registry != 'https://registry.npmjs.org/':
        raise ValueError('Production publication only supports the configured public npm registry')
    # Inspect every existing version before changing anything. A conflicting
    # partial publication must fail before uploading another package.
    before = {p['name']: registry_package(registry, p['name']) for p in packages}
    states = {p['name']: publication_state(p, before[p['name']], plan['version'], plan['distTag']) for p in packages}
    if not publish:
        print(json.dumps({'version': plan['version'], 'distTag': plan['distTag'], 'publicationEnabled': settings['publicationEnabled'],
            'packages': [{'name': p['name'], 'state': 'identical' if states[p['name']] else 'missing'} for p in packages]}, indent=2))
        return
    for package in packages:
        if not states[package['name']]:
            # Each platform is verified before uploading the main package.
            # npm trusted publishing authenticates publish, not dist-tag edits;
            # set the final tag atomically in publish instead of a later edit.
            subprocess.run([*npm_command(), 'publish', str((manifest.parent / package['tarball']).resolve()),
                '--access', 'public', '--tag', plan['distTag'], '--ignore-scripts',
                *([] if bootstrap else ['--provenance']), '--registry', registry], check=True)
        document = wait_for_publication(registry, package, plan['version'], plan['distTag'])
        check_default_tag(before[package['name']], document, plan['version'], plan['distTag'])
    print('Verified all exact-version native payloads and published platform packages before the main package.')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--publish', action='store_true')
    parser.add_argument('--bootstrap', action='store_true', help='Authenticated local first-alpha publish; no CI provenance claim')
    args = parser.parse_args()
    run(args.manifest.resolve(), args.publish, args.bootstrap)
