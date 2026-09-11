#!/usr/bin/env python3
"""Install/upgrade/remove one disposable Homebrew formula on a hosted macOS runner."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import subprocess
import tempfile
import time
import uuid
from homebrew_formula import generate
from native_artifacts import read_checked, verify_provider_inventory


def check(directory):
    if platform.system() != 'Darwin' or os.environ.get('GITHUB_ACTIONS') != 'true':
        raise ValueError('Homebrew mutation rehearsal requires a disposable hosted macOS CI runner')
    brew = shutil.which('brew')
    if not brew:
        raise ValueError('Native Homebrew installation is required')
    env = {**os.environ, 'HOMEBREW_NO_AUTO_UPDATE': '1', 'HOMEBREW_NO_INSTALL_CLEANUP': '1',
           'HOMEBREW_NO_ANALYTICS': '1', 'HOMEBREW_NO_ENV_HINTS': '1',
           'HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK': '1'}
    def run(*args, capture=False):
        result = subprocess.run([brew, *args], env=env, check=True, timeout=300,
                                stdout=subprocess.PIPE if capture else None, text=True)
        return result.stdout.strip() if capture else None
    prefix = Path(run('--prefix', capture=True))
    for binary in ('usagestat', 'usagestatd'):
        if (prefix / 'bin' / binary).exists() or (prefix / 'bin' / binary).is_symlink():
            raise ValueError('Rehearsal will not replace an existing backend installation')
    suffix = uuid.uuid4().hex[:12]
    tap = 'usagestat-fixture/native-' + suffix
    name = 'usagestat-fixture-' + suffix
    formula_id = tap + '/' + name
    formula = generate(directory, rehearsal=True, formula_name=name)
    release = json.loads(read_checked(directory / 'usagestat-artifacts.json'))
    manifest = next(m for m in release['targets'] if m['os'] == 'darwin'
                    and m['arch'] == ('arm64' if platform.machine() == 'arm64' else 'x64'))
    created = False
    service_owned = False
    installed = None
    first_keg = None
    with tempfile.TemporaryDirectory(prefix='usagestat brew profile 使用 ') as temporary:
        profile = Path(temporary)
        env['XDG_CONFIG_HOME'] = str(profile / 'homebrew-config')
        config = profile / 'config'; data = profile / 'data'
        config.mkdir(); data.mkdir()
        (config / 'config.toml').write_text('providers = []\n')
        home = profile / 'home'; home.mkdir()
        (config / 'retained-fixture').write_text('synthetic configuration')
        (data / 'retained-fixture').write_text('synthetic history')
        runtime_env = {**env, 'HOME': str(home), 'USAGESTAT_CONFIG_DIR': str(config), 'USAGESTAT_DATA_DIR': str(data)}
        # Do not pass the synthetic HOME to Homebrew itself; it needs its runner installation.
        runtime_env.pop('USAGESTAT_PLUGIN_DIR', None)
        runtime_env.pop('AI_USAGE_PLUGIN_DIR', None)
        try:
            run('tap-new', '--no-git', tap)
            created = True
            tap_dir = Path(run('--repository', tap, capture=True))
            recipe = tap_dir / 'Formula' / (name + '.rb')
            recipe.write_text(formula, encoding='utf-8')
            subprocess.run(['ruby', '-c', str(recipe)], check=True, timeout=30)
            # New Homebrew releases require trust for local tap code. Scope it
            # to this generated formula and a disposable configuration directory.
            if subprocess.run([brew, 'command', 'trust'], env=env, stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, timeout=30).returncode == 0:
                run('trust', '--formula', formula_id)
            run('install', '--formula', '--build-from-source', formula_id)
            run('test', formula_id)
            installed = prefix / 'opt' / name
            assert installed.resolve() == Path(run('--prefix', formula_id, capture=True)).resolve()
            assert (prefix / 'bin/usagestat').resolve() == (installed / 'bin/usagestat').resolve()
            first_keg = installed.resolve()
            def backend(*args, executable=None, check=True):
                completed = subprocess.run([str(executable or (installed / 'bin/usagestat')), *args],
                    env=runtime_env, cwd=profile, text=True, capture_output=True, timeout=90)
                if check and completed.returncode:
                    raise RuntimeError(f'Backend {args}: {completed.stderr}')
                return completed
            def status(): return json.loads(backend('--json', 'daemon', 'status').stdout)
            def saved(): return json.loads((config / 'daemon.json').read_text())
            def retained():
                installation = saved()['installation']
                files = [config / 'config.toml', config / 'retained-fixture', data / 'retained-fixture',
                         Path(installation['managementKeyFile']), Path(installation['controlKeyFile'])]
                return {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
            def inspect(fresh=True):
                result = subprocess.check_output([str(installed / 'bin/usagestat'), '--json', 'list'],
                    env=runtime_env, cwd=profile, text=True, timeout=30)
                providers = json.loads(result)
                verify_provider_inventory(providers, manifest)
                assert all(Path(p['icon']['path']).is_file() for p in providers if (p.get('icon') or {}).get('path'))
                assert (installed / 'share/usagestat/LICENSE').is_file()
                report = json.loads(subprocess.check_output([str(installed / 'bin/usagestat'), 'daemon', 'status', '--json'],
                    env=runtime_env, cwd=profile, text=True, timeout=30))
                if fresh: assert not report['configured'] and not report['registered'] and report['managerAvailable'], 'Install must not register startup or collide with a pre-existing agent'
                assert (config / 'retained-fixture').read_text() == 'synthetic configuration'
                assert (data / 'retained-fixture').read_text() == 'synthetic history'
            inspect()
            run('install', '--formula', formula_id)
            with socket.socket() as held:
                held.bind(('127.0.0.1', 0))
                bind = f'127.0.0.1:{held.getsockname()[1]}'
            # The fresh runner has no agent at this label; HOME/config/data are
            # disposable. No real provider is enabled or credentialed.
            service_owned = True
            backend('daemon', 't3', 'auto')
            backend('daemon', 'enable', '--bind', bind)
            baseline = retained()
            for revision, (running, autostart) in enumerate([(True, True), (False, True), (True, False), (False, False)], 1):
                backend('daemon', 'autostart', 'on' if autostart else 'off')
                backend('daemon', 'start' if running else 'stop')
                previous = saved()
                previous_keg = installed.resolve()
                # Retain the old keg until the registered path update commits.
                # Each actual Homebrew revision uses the same verified binaries.
                recipe.write_text(formula.replace('  license "MIT"', f'  revision {revision}\n  license "MIT"'), encoding='utf-8')
                run('upgrade', '--formula', formula_id)
                assert installed.resolve() != previous_keg and previous_keg.is_dir()
                backend('daemon', 'relocate')
                backend('daemon', 'relocate')
                current = status()
                assert (current['running'], current['autostart']) == (running, autostart), current
                assert not running or current['healthy']
                updated = saved()
                assert Path(updated['installation']['binary']).samefile(installed / 'bin/usagestatd')
                expected = previous['installation'].copy()
                actual = updated['installation'].copy()
                for field in ('binary', 'pluginDirs'):
                    expected.pop(field); actual.pop(field)
                assert expected == actual and updated['t3Mode'] == previous['t3Mode']
                assert retained() == baseline and not (config / 'daemon-relocation.json').exists()
                inspect(fresh=False)
            run('test', formula_id)
            # A version-reporting executable which cannot serve tests recovery
            # after launch failure. It is inside this fixture-owned Cellar owner.
            bad_keg = first_keg.parent / '0.0.0-relocation-fixture'
            bad_bin = bad_keg / 'bin/usagestatd'
            bad_bin.parent.mkdir(parents=True)
            version = json.loads((directory / 'usagestat-artifacts.json').read_text())['version']
            bad_bin.write_text(f'#!/bin/sh\nif [ "$1" = "--version" ]; then printf "usagestatd {version}\\n"; exit 0; fi\nexit 79\n')
            bad_bin.chmod(0o700)
            backend('daemon', 'autostart', 'off')
            backend('daemon', 'start')
            previous = saved()
            failed = backend('daemon', 'relocate', '--binary', str(bad_bin), check=False)
            assert failed.returncode != 0 and 'recovered' in failed.stderr, failed.stderr
            assert saved() == previous and retained() == baseline
            assert status()['healthy'] and not status()['autostart']
            journal = config / 'daemon-relocation.json'
            process = subprocess.Popen([str(installed / 'bin/usagestat'), 'daemon', 'relocate', '--binary', str(bad_bin)],
                env=runtime_env, cwd=profile, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 15
                while True:
                    if process.poll() is not None: raise AssertionError(('relocation ended before interruption', process.communicate()))
                    if journal.exists() and Path(saved()['installation']['binary']) == bad_bin.resolve(): break
                    if time.monotonic() > deadline: raise TimeoutError('relocation did not reach its saved replacement')
                    time.sleep(.02)
                process.kill(); process.communicate(timeout=10)
                before = (config / 'daemon.json').read_bytes()
                blocked = backend('daemon', 'start', check=False)
                assert blocked.returncode != 0 and 'recovery' in blocked.stderr
                assert (config / 'daemon.json').read_bytes() == before
                backend('daemon', 'recover')
                backend('daemon', 'recover')
                assert not journal.exists() and saved() == previous and retained() == baseline
                assert status()['healthy'] and not status()['autostart']
            finally:
                if process.poll() is None: process.kill(); process.communicate(timeout=10)
                if journal.exists(): backend('daemon', 'recover')
            shutil.rmtree(bad_keg)
            backend('daemon', 'unregister')
            assert not status()['registered']
            service_owned = False
            run('uninstall', '--formula', '--force', formula_id)
            assert not (prefix / 'bin/usagestat').exists()
            assert not (prefix / 'bin/usagestatd').exists()
            assert (config / 'retained-fixture').read_text() == 'synthetic configuration'
            assert (data / 'retained-fixture').read_text() == 'synthetic history'
        finally:
            if service_owned:
                usable = installed / 'bin/usagestat'
                if not usable.is_file(): usable = first_keg / 'bin/usagestat'
                backend('daemon', 'recover', executable=usable)
                backend('daemon', 'unregister', executable=usable)
            if created:
                subprocess.run([brew, 'uninstall', '--formula', '--force', formula_id], env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
                run('untap', tap)
    return {'checks': ['native-formula-install-both-binaries-and-resources', 'durable-linked-cli-discovery',
                       'install-twice', 'versioned-keg-revision-upgrade', 'no-implicit-startup',
                       'uninstall-retains-user-data', 'registered-relocation-preserves-four-running-autostart-states',
                       'failed-relocation-restores-retained-keg-and-profile', 'interrupted-relocation-private-journal-recovery'],
            'activeDaemonUpgrade': 'native-fixtures', 'signedDistribution': 'pending'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    result = check(args.artifacts.resolve())
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
