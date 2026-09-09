#!/usr/bin/env python3
"""Read-only release preflight/verification; never overwrite existing assets."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import urllib.error
import urllib.parse
import urllib.request

def get(endpoint):
    request = urllib.request.Request('https://api.github.com/' + endpoint, headers={
        'Authorization': 'Bearer ' + os.environ['GH_TOKEN'],
        'Accept': 'application/vnd.github+json', 'Cache-Control': 'no-cache',
    })
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)

def compare_existing(release, expected, prerelease):
    if release['draft'] or release['prerelease'] != prerelease:
        raise ValueError('Existing release has a different draft/prerelease state')
    installer_names = {'Install-Usagestat.ps1', 'Install-Usagestat.ps1.sha256'}
    assets = {asset['name']: asset for asset in release['assets']
              if asset['name'].startswith('usagestat-') or asset['name'] in installer_names}
    if set(assets) != set(expected):
        raise ValueError('Existing release is incomplete or has different native assets; inspect it before retrying')
    for name, (size, digest) in expected.items():
        asset = assets[name]
        if asset['size'] != size:
            raise ValueError(f'Existing release asset differs: {name}')
        actual = asset.get('digest')
        if not actual:
            # Public download URL from GitHub metadata; do not forward a token
            # to the asset redirect destination. Stream at most the expected size.
            hashed, received = hashlib.sha256(), 0
            with urllib.request.urlopen(asset['browser_download_url'], timeout=30) as response:
                while chunk := response.read(1024 * 1024):
                    received += len(chunk)
                    if received > size:
                        raise ValueError(f'Existing asset exceeds expected size: {name}')
                    hashed.update(chunk)
            if received != size:
                raise ValueError(f'Existing asset is truncated: {name}')
            actual = 'sha256:' + hashed.hexdigest()
        if actual != 'sha256:' + digest:
            raise ValueError(f'Existing release asset checksum differs: {name}')

def check(directory, *, require_existing=False, tag=None):
    repository = os.environ['GITHUB_REPOSITORY']
    tag = tag or os.environ['GITHUB_REF_NAME']
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository):
        raise ValueError('Invalid source repository')
    manifest = json.loads((directory / 'usagestat-artifacts.json').read_text())
    if tag != 'v' + manifest['version']:
        raise ValueError('Release tag does not match the staged manifest')
    # Prove repository access before interpreting a release's 404 as absence.
    get(f'repos/{repository}')
    reference = get(f'repos/{repository}/git/ref/tags/' + urllib.parse.quote(tag, safe=''))['object']
    for _ in range(8):
        if reference['type'] == 'commit':
            break
        if reference['type'] != 'tag':
            raise ValueError('Release tag does not resolve to a commit')
        reference = get(f'repos/{repository}/git/tags/{reference["sha"]}')['object']
    if reference['type'] != 'commit' or reference['sha'] != manifest['sourceCommit']:
        raise ValueError('Live tag moved or does not match the tested artifact source')
    try:
        release = get(f'repos/{repository}/releases/tags/' + urllib.parse.quote(tag, safe=''))
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        if require_existing:
            raise ValueError('Published release is still missing') from error
        return True
    expected = {path.name: (path.stat().st_size, hashlib.sha256(path.read_bytes()).hexdigest())
                for path in directory.iterdir() if path.is_file()}
    compare_existing(release, expected, '-' in manifest['version'])
    return False

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--require-existing', action='store_true')
    parser.add_argument('--tag', help='Existing release tag for a package workflow dispatched from a branch')
    args = parser.parse_args()
    create = check(args.directory, require_existing=args.require_existing, tag=args.tag)
    print('Release does not exist; publication can create it.' if create else 'Existing release matches all staged assets; no upload needed.')
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write('create=' + str(create).lower() + '\n')
