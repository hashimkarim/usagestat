#!/usr/bin/env python3
"""Exercise the actual PowerShell installer using only disposable native inputs."""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

from native_smoke import isolated_env

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/publish/scripts'))
from native_artifacts import archive_bytes, digest, resource_files, version

INSTALLER = ROOT / 'tools/install/Install-Usagestat.ps1'


@contextlib.contextmanager
def hold_directory(path: Path):
    """Keep a non-inherited native handle without FILE_SHARE_DELETE.

    The installer runs in another process. Ordinary reads/launches are allowed,
    but this handle prevents renaming/deleting the selected directory.
    https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew
    """
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                  ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    handle = kernel.CreateFileW(str(path), 0x80000000, 3, None, 3, 0x02000000, None)
    if handle == wintypes.HANDLE(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        yield
    finally:
        if not kernel.CloseHandle(handle): raise ctypes.WinError(ctypes.get_last_error())


def native_fixture(binary_dir: Path, output: Path) -> Path:
    """Use real locally built executables; this fixture is not a release claim."""
    files = resource_files()
    executables = ['usagestat.exe', 'usagestatd.exe', 'usagestat-service.exe']
    files.update({name: (binary_dir / name).read_bytes() for name in executables})
    files['plugins/installer-fixture/plugin.json'] = json.dumps({
        'id': 'installer-fixture', 'name': 'Installer fixture', 'entry': 'plugin.js',
        'version': '1', 'description': 'Synthetic local installer acceptance',
        'icon': 'icon.svg', 'supportedModes': ['local'], 'autoMode': 'local', 'enabledByDefault': False,
    }).encode()
    files['plugins/installer-fixture/icon.svg'] = b'<svg xmlns="http://www.w3.org/2000/svg"/>'
    files['plugins/installer-fixture/plugin.js'] = b'globalThis.__usagestat_plugin={id:"installer-fixture",probe:function(){return {plan:"fixture",metrics:[{type:"text",label:"Polling",value:"synthetic"}]};}};'
    output.mkdir()
    archive = archive_bytes(files, executables, True)
    name = 'usagestat-windows-x86_64.zip'
    (output / name).write_bytes(archive)
    (output / (name + '.sha256')).write_text(f'{digest(archive)}  {name}\n')
    manifest = {'schemaVersion': 1, 'package': 'usagestat', 'version': version(),
                'sourceCommit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                'sourceDirty': False, 'target': 'x86_64-pc-windows-msvc', 'os': 'win32', 'arch': 'x64',
                'signing': 'unsigned', 'fixtureKind': 'native-debug-not-release-qualified',
                'archive': {'name': name, 'size': len(archive), 'sha256': digest(archive)},
                'files': [{'path': path, 'size': len(data), 'sha256': digest(data), 'executable': path in executables}
                          for path, data in sorted(files.items())]}
    path = output / 'usagestat-windows-x86_64.manifest.json'
    path.write_text(json.dumps(manifest), encoding='utf-8')
    path.with_name(path.name + '.sha256').write_text(f'{digest(path.read_bytes())}  {path.name}\n')
    return path


def check(binary_dir: Path | None = None, manifest: Path | None = None, installer: Path = INSTALLER) -> dict:
    if os.name != 'nt' or os.environ.get('GITHUB_ACTIONS') != 'true':
        raise RuntimeError('Installer acceptance requires a disposable Windows CI runner; it owns the otherwise absent dev task only.')
    installer = installer.resolve()
    if installer.read_bytes() != INSTALLER.read_bytes():
        raise ValueError('Installer bytes disagree with the checked-out release source')
    if installer != INSTALLER:
        from native_artifacts import read_checked
        read_checked(installer)
    powershell = str(Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe')
    # Before any service changes, prove the exact per-user dev task is unused.
    task_check = "$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $s=New-Object -ComObject Schedule.Service; $s.Connect(); $tasks=@($s.GetFolder('\\').GetTasks(1) | Where-Object {$_.Name -eq ('usagestat-dev-'+$sid)}); if ($tasks.Count) {throw 'Preserve the existing dev task.'}"
    subprocess.run([powershell, '-NoProfile', '-NonInteractive', '-Command', task_check], check=True, timeout=30)
    result = {'checks': [], 'shell': 'Windows PowerShell 5.1', 'realDesktopAcceptance': 'pending',
              'input': 'verified-release' if manifest else 'native-debug-fixture'}
    with tempfile.TemporaryDirectory(prefix='usagestat installer 使用 & ') as temporary:
        root = Path(temporary).resolve()
        env = isolated_env(root)
        env.pop('HOME', None)
        # The real native scheduled task uses this saved, isolated environment.
        prefix = root / 'programs 使用 & space' / 'usagestat-dev'
        cli = prefix / 'bin/usagestat-dev.exe'
        config_root = Path(env['USAGESTAT_CONFIG_DIR'])
        data_root = Path(env['USAGESTAT_DATA_DIR'])
        config = config_root / 'config.toml'
        config.write_text('providers = []\n', encoding='utf-8')
        history = data_root / 'retained-history-marker'
        history.write_text('synthetic retained user history', encoding='utf-8')
        initial_path = subprocess.check_output([powershell, '-NoProfile', '-Command', "[Environment]::GetEnvironmentVariable('Path','User')"], text=True)
        selected = manifest.resolve() if manifest else native_fixture(binary_dir.resolve(), root / 'inputs')
        journal = prefix.parent / '.usagestat-dev.usagestat-transaction.json'

        def install(action='Install', script=installer, expected=0, selected_manifest=selected):
            command = [powershell, '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
                       '-File', str(script), '-Action', action, '-Destination', str(prefix), '-BackendProfile', 'dev']
            if action == 'Install': command += ['-Manifest', str(selected_manifest)]
            process = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=240)
            if expected == 0 and process.returncode != 0 or expected != 0 and process.returncode == 0:
                raise AssertionError((action, process.returncode, process.stdout, process.stderr))
            return process

        def command(*args):
            process = subprocess.run([str(cli), *args], cwd=root, env=env, capture_output=True, text=True, encoding='utf-8', timeout=60)
            if process.returncode: raise AssertionError((args, process.stdout, process.stderr))
            return process.stdout

        def state(): return json.loads(command('--json', 'daemon', 'status'))

        def retained():
            return {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in [
                config, history, config_root / 'daemon.json', data_root / 'daemon-control-key', data_root / 't3-management-key']}

        try:
            install()
            assert not state()['registered'] and not (config_root / 'daemon.json').exists()
            providers = json.loads(command('--json', 'list'))
            assert len(providers) >= 61 and all(Path(p['icon']['path']).is_file() for p in providers if (p.get('icon') or {}).get('path'))
            install()
            assert not state()['registered']
            result['checks'].append('install-twice-unicode-resources-no-implicit-startup')
            # A corrupt sidecar must fail before service/file replacement.
            bad = root / 'bad-input'
            bad.mkdir()
            bad_manifest = bad / selected.name
            shutil.copyfile(selected, bad_manifest)
            bad_manifest.with_name(bad_manifest.name + '.sha256').write_text('0' * 64 + '  ' + bad_manifest.name + '\n')
            record_bytes = (prefix / 'usagestat-installation.json').read_bytes()
            install(expected=1, selected_manifest=bad_manifest)
            assert (prefix / 'usagestat-installation.json').read_bytes() == record_bytes and not journal.exists()
            foreign = prefix / 'user-owned.txt'
            foreign.write_text('preserve this unrelated file')
            install(expected=1)
            install('Uninstall', expected=1)
            assert foreign.read_text() == 'preserve this unrelated file'
            foreign.unlink()
            result['checks'].append('bad-checksum-and-unowned-file-refusal-before-mutation')

            if not manifest:
                config.write_text('refreshSec = 1\n[[providers]]\nid = "installer-fixture"\nenabled = true\nsource = "local"\n', encoding='utf-8')
            with socket.socket() as held:
                held.bind(('127.0.0.1', 0))
                bind = f'127.0.0.1:{held.getsockname()[1]}'
            command('daemon', 't3', 'auto')
            command('daemon', 'enable', '--bind', bind)
            baseline = retained()
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def get(route):
                with opener.open('http://' + bind + route, timeout=2) as response: return json.load(response)

            if not manifest:
                deadline = time.monotonic() + 15
                while not get('/v1/usage'):
                    if time.monotonic() > deadline: raise TimeoutError('synthetic provider did not poll')
                    time.sleep(.1)
            old_pid = get('/health')['pid']
            install()
            assert get('/health')['pid'] != old_pid and retained() == baseline
            if not manifest: assert get('/v1/providers')
            result['checks'].append('active-native-replacement-health-and-retained-data')

            for running, autostart in [(False, True), (True, False), (False, False)]:
                command('daemon', 'autostart', 'on' if autostart else 'off')
                command('daemon', 'start' if running else 'stop')
                install()
                current = state()
                assert (current['running'], current['autostart']) == (running, autostart), current
                assert retained() == baseline
            result['checks'].append('all-four-running-autostart-states-preserved')

            command('daemon', 'autostart', 'on')
            command('daemon', 'start')
            with hold_directory(prefix):
                before = (prefix / 'usagestat-installation.json').read_bytes()
                install(expected=1)
                assert prefix.is_dir() and not journal.exists()
                assert (prefix / 'usagestat-installation.json').read_bytes() == before
                assert state()['healthy'] and state()['autostart'] and retained() == baseline
            install()
            assert state()['healthy'] and state()['autostart'] and retained() == baseline
            result['checks'].append('locked-prefix-upgrade-restores-prior-service-state')

            # Fault injection affects a disposable script copy only. There is no
            # test-only environment hook in the shipped installer.
            anchor = '        Check-Payload $prefix $record\n        Restore-State $state'
            original = installer.read_text(encoding='utf-8')
            assert original.count(anchor) == 1
            failed = root / 'fail-after-replacement.ps1'
            failed.write_text(original.replace(anchor, '        throw "synthetic replacement health failure"\n        Restore-State $state'), encoding='utf-8')
            before = (prefix / 'usagestat-installation.json').read_bytes()
            process = install(script=failed, expected=1)
            assert 'synthetic replacement health failure' in process.stderr
            assert (prefix / 'usagestat-installation.json').read_bytes() == before
            assert not journal.exists() and state()['healthy'] and state()['autostart'] and retained() == baseline
            result['checks'].append('failed-health-restores-previous-files-and-running-state')

            interrupted = root / 'interrupt-after-replacement.ps1'
            interrupted.write_text(original.replace(anchor, '        [Environment]::Exit(86)\n        Restore-State $state'), encoding='utf-8')
            process = install(script=interrupted, expected=1)
            assert process.returncode == 86 and journal.exists()
            assert not state()['running'] and not state()['autostart']
            with hold_directory(prefix):
                before = journal.read_bytes()
                directories = sorted(prefix.parent.glob('.usagestat-dev.backup-*'))
                assert len(directories) == 1
                previous_record = (directories[0] / 'usagestat-installation.json').read_bytes()
                install('Recover', expected=1)
                assert journal.read_bytes() == before and prefix.is_dir()
                assert (directories[0] / 'usagestat-installation.json').read_bytes() == previous_record
                assert not state()['running'] and retained() == baseline
            install('Recover')
            assert not journal.exists() and state()['healthy'] and state()['autostart'] and retained() == baseline
            result['checks'].append('interrupted-replacement-journal-recovery')
            result['checks'].append('locked-recovery-retains-journal-until-release')

            with hold_directory(prefix):
                before = (prefix / 'usagestat-installation.json').read_bytes()
                install('Uninstall', expected=1)
                assert prefix.is_dir() and not state()['registered']
                assert (prefix / 'usagestat-installation.json').read_bytes() == before and retained() == baseline
            install('Uninstall')
            install('Uninstall')
            assert not prefix.exists() and retained() == baseline
            subprocess.run([powershell, '-NoProfile', '-NonInteractive', '-Command', task_check], check=True, timeout=30)
            current_path = subprocess.check_output([powershell, '-NoProfile', '-Command', "[Environment]::GetEnvironmentVariable('Path','User')"], text=True)
            assert initial_path == current_path
            result['checks'].append('uninstall-owned-task-payload-retains-data-and-user-path')
            result['checks'].append('locked-uninstall-preserves-payload-for-retry')
        finally:
            if journal.exists(): install('Recover')
            if cli.exists():
                current = state()
                if current['registered'] and Path(current['owner']) == prefix:
                    command('daemon', 'unregister')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument('--binary-dir', type=Path)
    inputs.add_argument('--manifest', type=Path)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--installer', type=Path, default=INSTALLER)
    args = parser.parse_args()
    args.report.parent.mkdir(parents=True, exist_ok=True)
    try:
        report = check(args.binary_dir, args.manifest, args.installer)
    except Exception as error:
        args.report.write_text(json.dumps({'error': str(error)}, indent=2), encoding='utf-8')
        raise
    args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report))
