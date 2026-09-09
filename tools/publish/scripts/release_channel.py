#!/usr/bin/env python3
"""Validate the stable/alpha publication lane without changing remote state."""
import argparse
import json
import os
import re
import urllib.request

REPOSITORY = 'hashimkarim/usagestat'
BASE = r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'


def alpha_version(tag):
    if not re.fullmatch('v' + BASE + r'-alpha\.(0|[1-9][0-9]*)', tag):
        raise ValueError('The alpha repositories require vMAJOR.MINOR.PATCH-alpha.NUMBER')
    return tag[1:]


def alpha_order(version):
    match = re.fullmatch(BASE + r'-alpha\.(0|[1-9][0-9]*)', alpha_version('v' + version))
    return tuple(map(int, match.groups()))


def get(endpoint):
    headers = {'Cache-Control': 'no-cache', 'Accept': 'application/vnd.github+json'}
    if os.environ.get('GH_TOKEN'):
        headers['Authorization'] = 'Bearer ' + os.environ['GH_TOKEN']
    request = urllib.request.Request('https://api.github.com/repos/' + REPOSITORY + '/' + endpoint,
                                     headers=headers)
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def check(tag, channel):
    if channel == 'alpha':
        alpha_version(tag)
    elif channel != 'stable' or not re.fullmatch('v' + BASE, tag):
        raise ValueError('Stable repositories require vMAJOR.MINOR.PATCH')
    release = get('releases/tags/' + tag)
    if release['draft'] or release['prerelease'] != (channel == 'alpha') or release['tag_name'] != tag:
        raise ValueError('The published release does not match the requested channel')
    if channel == 'stable':
        if get('releases/latest')['tag_name'] != tag:
            raise ValueError('Only the latest stable release can update stable repositories')
    else:
        candidates = []
        for page in range(1, 21):
            releases = get(f'releases?per_page=100&page={page}')
            for item in releases:
                if not item['draft'] and item['prerelease'] and re.fullmatch('v' + BASE + r'-alpha\.(0|[1-9][0-9]*)', item['tag_name']):
                    candidates.append(item['tag_name'])
            if len(releases) < 100:
                break
        else:
            raise ValueError('Release pagination limit reached; cannot establish newest alpha')
        if not candidates or max(candidates, key=lambda value: alpha_order(value[1:])) != tag:
            raise ValueError('Only the newest published alpha can update alpha repositories')
    return release


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    parser.add_argument('--channel', choices=['stable', 'alpha'], default='stable')
    args = parser.parse_args()
    check(args.tag, args.channel)
    print(f'Published {args.channel} release verified: {args.tag}')
