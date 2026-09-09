#!/usr/bin/env python3
"""Compare public npm payloads to staged release contents without installation."""
import argparse
import base64
import hashlib
import io
import json
from pathlib import Path
import tarfile
import urllib.parse
import urllib.request
from npm_publish import registry_package, validate


def contents(data):
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        result = {}
        total = 0
        for item in archive:
            if item.isdir(): continue
            if not item.isfile() or item.name in result:
                raise ValueError('npm archive contains links or duplicate files')
            total += item.size
            if total > 256 * 1024 * 1024:
                raise ValueError('Unpacked npm archive exceeds the size limit')
            result[item.name] = hashlib.sha256(archive.extractfile(item).read()).hexdigest()
        return result


def check(manifest):
    plan = json.loads(manifest.read_text())
    packages = validate(plan, manifest.parent)
    for package in packages:
        document = registry_package('https://registry.npmjs.org/', package['name'])
        if not document or document.get('dist-tags', {}).get(plan['distTag']) != plan['version']:
            raise ValueError('Public npm channel/version does not match the release')
        if (plan['distTag'] == 'alpha' and document.get('dist-tags', {}).get('latest') == plan['version']
                and set(document['versions']) != {plan['version']}):
            raise ValueError('Only a new package with a single alpha version may default to that alpha')
        remote = document['versions'][plan['version']]
        for field in ['name', 'version', 'optionalDependencies', 'os', 'cpu', 'libc', 'bin']:
            if remote.get(field) != package['packageJson'].get(field):
                raise ValueError('Public npm package metadata differs from the release')
        url = remote['dist']['tarball']
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme != 'https' or parsed.netloc != 'registry.npmjs.org':
            raise ValueError('Unexpected public npm tarball origin')
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read(64 * 1024 * 1024 + 1)
        if len(data) > 64 * 1024 * 1024:
            raise ValueError('Public npm tarball exceeds the size limit')
        integrity = 'sha512-' + base64.b64encode(hashlib.sha512(data).digest()).decode()
        if integrity != remote['dist']['integrity']:
            raise ValueError('Public npm tarball integrity mismatch')
        # npm tar headers differ on filesystems that cannot retain POSIX modes.
        # Compare every file's actual bytes against the checked release staging.
        if contents(data) != contents((manifest.parent / package['tarball']).read_bytes()):
            raise ValueError('Public npm file contents differ from the staged release')
        print('Verified public package contents:', package['name'], flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    check(parser.parse_args().manifest.resolve())
