#!/usr/bin/env python3
"""Check remote build status; exit 0 when done, 1 when submission is needed.

Network/API errors are fatal (exit 2), never a reason to upload a duplicate.
"""
import argparse
import json
import sys
import time
import urllib.parse
import urllib.request


def ppa_version(version):
    # 1.0.3-1ppa1 built, but Launchpad rejected its epoch-zero file timestamps.
    revision = 2 if version == '1.0.3' else 1
    return f'{version}-1ppa{revision}'


def get(url):
    # Cached "missing" results can outlive publication and trigger duplicate uploads.
    request = urllib.request.Request(url, headers={'Cache-Control': 'no-cache'})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def state(platform, version, channel='stable'):
    if channel not in ('stable', 'alpha'):
        raise ValueError('Unknown publication channel')
    project = 'usagestat-alpha' if channel == 'alpha' else 'usagestat'
    if channel == 'alpha':
        from release_channel import alpha_version
        version = alpha_version('v' + version).replace('-', '~')
    if platform == 'copr':
        data = get(f'https://copr.fedorainfracloud.org/api_3/build/list?ownername=hashimkarim&projectname={project}&limit=100')
        builds = [b for b in data['items'] if (b.get('source_package') or {}).get('version') == version + '-1']
        if not builds:
            if any(not b.get('source_package') and b['state'] not in {'succeeded', 'failed', 'canceled', 'skipped'} for b in data['items']):
                return 'pending'
            return 'missing'
        build = max(builds, key=lambda b: b['id'])
        status = build['state']
        if status == 'succeeded':
            return 'done'
        if status in {'failed', 'canceled', 'skipped'}:
            return 'failed'
        return 'pending'
    archive = 'https://api.launchpad.net/1.0/~hashimkarim/+archive/ubuntu/' + project
    deb_version = ppa_version(version)
    query = urllib.parse.urlencode({'ws.op': 'getPublishedSources', 'source_name': 'usagestat', 'exact_match': 'true', 'version': deb_version})
    entries = get(archive + '?' + query)['entries']
    entries = [e for e in entries if e['source_package_version'] == deb_version]
    if not entries:
        return 'missing'
    if all(e['status'] in {'Deleted', 'Obsolete', 'Superseded'} for e in entries):
        raise RuntimeError('PPA version was already used; increment the package revision before uploading again')
    entry = max(entries, key=lambda e: e['date_created'])
    builds = get(entry['self_link'] + '?ws.op=getBuilds')['entries']
    if any(b['buildstate'] in {'Failed to build', 'Dependency wait', 'Chroot problem', 'Failed to upload', 'Cancelled'} for b in builds):
        raise RuntimeError('Launchpad build failed; inspect the PPA build page before retrying')
    if builds and all(b['buildstate'] == 'Successfully built' for b in builds):
        binaries = get(archive + '?' + urllib.parse.urlencode({'ws.op': 'getPublishedBinaries', 'binary_name': 'usagestat', 'exact_match': 'true', 'version': deb_version, 'status': 'Published'}))['entries']
        published = {b['distro_arch_series_link'] for b in binaries if b['binary_package_version'] == deb_version}
        if all(b['distro_series_link'] + '/' + b['arch_tag'] in published for b in builds):
            return 'done'
    return 'pending'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=['copr', 'ppa'])
    parser.add_argument('version')
    parser.add_argument('--wait', action='store_true')
    parser.add_argument('--channel', choices=['stable', 'alpha'], default='stable')
    parser.add_argument('--print-version', action='store_true', help='Print the package revision without querying the platform')
    args = parser.parse_args()
    if args.print_version:
        version = args.version.replace('-', '~') if args.channel == 'alpha' else args.version
        print(ppa_version(version) if args.platform == 'ppa' else version + '-1')
        return 0
    deadline = time.monotonic() + 2700
    while True:
        result = state(args.platform, args.version, args.channel)
        print(f'{args.platform} {args.version}: {result}', flush=True)
        if result == 'done':
            return 0
        if result == 'failed':
            if args.wait:
                raise RuntimeError('Remote package build failed')
            return 1
        if result == 'missing' and not args.wait:
            return 1
        # Existing pending uploads/builds are awaited, never submitted again.
        if time.monotonic() >= deadline:
            raise RuntimeError('Timed out waiting for repository publication')
        time.sleep(30)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print(f'::error::{error}', file=sys.stderr)
        sys.exit(2)
