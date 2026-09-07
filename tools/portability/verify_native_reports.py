#!/usr/bin/env python3
"""Reject incomplete native evidence, even when an upstream job reports success."""
import argparse
import json
from pathlib import Path
import re

TARGETS = {'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu',
           'x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-pc-windows-msvc'}
SECTIONS = {'smoke', 'probe_cancellation', 'daemon_lifecycle', 'diagnostics',
            'ide_discovery', 'local_usage', 'provider_storage', 'codex_auth',
            'browser_import', 'bar_contract', 'dev_install'}
COMMANDS = {'build', 'rust-tests', 'node-tests', 'python-tests', 'provider-inventory'}
INSTALLER_CHECKS = {'install-twice-unicode-resources-no-implicit-startup',
    'bad-checksum-and-unowned-file-refusal-before-mutation',
    'active-native-replacement-health-and-retained-data',
    'all-four-running-autostart-states-preserved',
    'failed-health-restores-previous-files-and-running-state',
    'interrupted-replacement-journal-recovery',
    'uninstall-owned-task-payload-retains-data-and-user-path',
    'locked-prefix-upgrade-restores-prior-service-state',
    'locked-recovery-retains-journal-until-release',
    'locked-uninstall-preserves-payload-for-retry'}


def checked_report(path):
    if path.stat().st_size > 8 * 1024 * 1024:
        raise ValueError(f'Oversized evidence: {path}')
    value = json.loads(path.read_text(encoding='utf-8'))
    if not isinstance(value, dict) or 'error' in value:
        raise ValueError(f'Recorded failure in {path}: {value.get("error") if isinstance(value, dict) else "not an object"!r}')
    return value


def verify(directory: Path, source_commit: str) -> dict:
    if not re.fullmatch('[0-9a-f]{40}', source_commit):
        raise ValueError('An exact source commit is required')
    found = set()
    for path in sorted(directory.glob('*/report.json')):
        report = checked_report(path)
        target = report.get('target')
        if target not in TARGETS or target in found:
            raise ValueError(f'Duplicate or unexpected target: {target}')
        found.add(target)
        if report.get('suiteSchemaVersion') != 1 or report.get('sourceCommit') != source_commit or report.get('sourceDirty') is not False:
            raise ValueError(f'Missing, mixed or dirty source identity: {target}')
        commands = report.get('checks', [])
        names = [item['name'] for item in commands]
        if len(set(names)) != len(names) or not COMMANDS.issubset(names) or any(item.get('exit_code') != 0 for item in commands):
            raise ValueError(f'Failed, duplicated or omitted test command: {target}')
        for name in SECTIONS | ({'windows_service'} if target.endswith('windows-msvc') else set()):
            value = report.get(name)
            if not isinstance(value, dict) or 'error' in value or not value.get('checks'):
                raise ValueError(f'Missing/failed native fixture {name}: {target}')
        native = None
        if target.endswith('apple-darwin'):
            native = ('launchagent-tests', 'isolated_native_launchagent_lifecycle')
        elif target.endswith('windows-msvc'):
            native = ('scheduled-task-tests', 'isolated_native_scheduled_task_lifecycle')
        if native:
            command, test = native
            log = path.parent / (command + '.log')
            if command not in names or not log.is_file() or not re.search(r'\btest [^\r\n]*::' + test + r' \.\.\. ok\b', log.read_text(encoding='utf-8', errors='replace')):
                raise ValueError(f'Native login-service fixture did not actually complete: {target}')
        if target.endswith('windows-msvc'):
            installer = checked_report(path.parent / 'windows-installer.json')
            if not INSTALLER_CHECKS.issubset(installer.get('checks', [])) or installer.get('input') != 'native-debug-fixture':
                raise ValueError('Windows installer lifecycle evidence is incomplete')
    if found != TARGETS:
        raise ValueError(f'Missing native targets: {sorted(TARGETS - found)}')
    return {'sourceCommit': source_commit, 'targets': sorted(found), 'result': 'complete-native-fixtures',
            'desktopSessionQualification': 'pending', 'minimumSystemQualification': 'pending'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reports', type=Path, required=True)
    parser.add_argument('--source-commit', required=True)
    args = parser.parse_args()
    print(json.dumps(verify(args.reports, args.source_commit), indent=2))
