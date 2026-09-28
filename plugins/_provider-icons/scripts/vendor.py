#!/usr/bin/env python3
"""Vendor an exact release into a non-npm application. Never fetch at runtime."""
import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import tarfile
import urllib.request

p=argparse.ArgumentParser()
p.add_argument('archive', help='Local tarball or HTTPS release URL')
p.add_argument('--sha256', required=True)
p.add_argument('--target', required=True)
args=p.parse_args()
data=urllib.request.urlopen(args.archive).read(4_000_001) if args.archive.startswith('https://') else Path(args.archive).read_bytes()
assert len(data)<=4_000_000 and hashlib.sha256(data).hexdigest()==args.sha256, 'Archive digest mismatch'
files={}
with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
    for m in archive.getmembers():
        path=PurePosixPath(m.name)
        assert not path.is_absolute() and '..' not in path.parts and path.parts[0]=='package'
        if m.isdir(): continue
        assert m.isfile() and m.size<=2_000_000, 'Only bounded regular package files are allowed'
        name=str(path.relative_to('package'))
        assert name not in files, 'Duplicate package path'
        if name.startswith('assets/'): name=name.removeprefix('assets/')
        files[name]=archive.extractfile(m).read()
assert json.loads(files['package.json'])['name']=='@agenticdriver/provider-icons'
root=Path(args.target);root.mkdir(parents=True,exist_ok=True)
# This directory contains generated library files only. Remove obsolete artwork.
for old in root.glob('*.svg'):
    if old.name not in files: old.unlink()
for name,data in files.items():
    dst=root/name;dst.parent.mkdir(parents=True,exist_ok=True);dst.write_bytes(data)
(root/'source.json').write_text(json.dumps({'archive':args.archive,'sha256':args.sha256},indent=2)+'\n')
print(f'Vendored {len(files)} files; SHA-256 verified')
