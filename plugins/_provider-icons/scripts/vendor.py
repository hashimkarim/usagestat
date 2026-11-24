#!/usr/bin/env python3
"""Vendor an exact release into a non-npm application. Never fetch at runtime."""
import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import tarfile
import urllib.request

MAX_ARCHIVE = 4_000_000
MAX_FILE = 8_000_000
MAX_UNPACKED = 32_000_000
MAX_MEMBERS = 10_000


def package_files(data, sha256):
    # These are input checks, not assertions: python -O must enforce them too.
    if len(data) > MAX_ARCHIVE:
        raise ValueError('Archive exceeds size limit')
    if hashlib.sha256(data).hexdigest() != sha256.lower():
        raise ValueError('Archive digest mismatch')
    files, total = {}, 0
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        for count, member in enumerate(archive, 1):
            if count > MAX_MEMBERS:
                raise ValueError('Too many archive members')
            path = PurePosixPath(member.name)
            if (path.is_absolute() or not path.parts or path.parts[0] != 'package'
                    or '..' in path.parts or '\\' in member.name or ':' in member.name):
                raise ValueError('Unsafe package path')
            if member.isdir():
                continue
            if not member.isfile() or not 0 <= member.size <= MAX_FILE:
                raise ValueError('Only bounded regular package files are allowed')
            total += member.size
            if total > MAX_UNPACKED:
                raise ValueError('Unpacked archive exceeds size limit')
            name = path.relative_to('package')
            if name.parts and name.parts[0] == 'assets':
                name = name.relative_to('assets')
            name = str(name)
            if name in ('.', 'source.json') or name in files:
                raise ValueError('Duplicate or reserved package path')
            files[name] = archive.extractfile(member).read()
    metadata = json.loads(files.get('package.json', b'{}'))
    if not isinstance(metadata, dict) or metadata.get('name') != '@agenticdriver/provider-icons':
        raise ValueError('Unexpected package identity')
    return files


def write_package(files, target, source):
    root = Path(target).absolute()
    if root.is_symlink():
        raise ValueError('Target must not be a symbolic link')
    root = root.resolve()
    # Validate every destination before removing old SVGs or writing any files.
    for name in [*files, 'source.json']:
        destination = root / name
        if destination.is_symlink() or (destination.exists() and not destination.is_file()):
            raise ValueError(f'Target file is not a regular file: {name}')
        for parent in destination.parents:
            if parent.is_symlink() or (parent.exists() and not parent.is_dir()):
                raise ValueError(f'Target directory is not a regular directory: {name}')
            if parent == root:
                break
            if str(parent.relative_to(root)) in files:
                raise ValueError(f'Conflicting package paths: {name}')
    root.mkdir(parents=True, exist_ok=True)
    # This directory contains generated library files only. Remove obsolete artwork.
    for old in root.glob('*.svg'):
        if old.name not in files:
            old.unlink()
    for name, data in files.items():
        destination = root / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
    (root / 'source.json').write_text(json.dumps(source, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', help='Local tarball or HTTPS release URL')
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--target', required=True)
    args = parser.parse_args()
    try:
        stream = (urllib.request.urlopen(args.archive, timeout=30) if args.archive.startswith('https://')
                  else Path(args.archive).open('rb'))
        with stream:
            data = stream.read(MAX_ARCHIVE + 1)
        files = package_files(data, args.sha256)
        write_package(files, args.target, {'archive': args.archive, 'sha256': args.sha256.lower()})
    except (ValueError, OSError, tarfile.TarError) as error:
        parser.error(str(error))
    print(f'Vendored {len(files)} files; SHA-256 verified')


if __name__ == '__main__':
    main()
