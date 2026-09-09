#!/usr/bin/env python3
"""Install packed npm payloads through an isolated registry; never publish."""
from __future__ import annotations
import argparse
import contextlib
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import socket
import urllib.request
import urllib.error
import urllib.parse

from native_smoke import isolated_env
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/publish/scripts'))
from npm_packages import npm_command, key

@contextlib.contextmanager
def registry(packed: Path):
    plan = json.loads(packed.read_text())
    packages = {entry['name']: entry for entry in plan['packages']}
    tarballs = {'/' + entry['tarball']: entry for entry in plan['packages']}
    requested = []
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            route = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path)
            requested.append(route)
            if route in tarballs:
                payload = (packed.parent / tarballs[route]['tarball']).read_bytes()
                content_type = 'application/octet-stream'
            elif route.lstrip('/') in packages:
                name = route.lstrip('/')
                package = packages[name]
                manifest = dict(package.get('packageJson') or json.loads((packed.parent / package['directory'] / 'package.json').read_text()))
                manifest['dist'] = {'tarball': f'http://127.0.0.1:{self.server.server_port}/' + package['tarball'],
                                    'integrity': package['integrity']}
                payload = json.dumps({'name': name, 'dist-tags': {'latest': plan['version']},
                    'versions': {plan['version']: manifest}}).encode()
                content_type = 'application/json'
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        def log_message(self, *_args): pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try: yield f'http://127.0.0.1:{server.server_port}/', requested
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)

def check(packed: Path, temp_dir: Path | None = None, *, expect_doctor: bool = True, public_registry: bool = False) -> dict:
    result = {'checks': []}
    plan = json.loads(packed.read_text())
    main = next(p for p in plan['packages'] if p['role'] == 'main')
    node = shutil.which('node')
    source = contextlib.nullcontext(('https://registry.npmjs.org/', [])) if public_registry else registry(packed)
    with tempfile.TemporaryDirectory(prefix='usagestat npm install 使用 ', dir=temp_dir) as directory, source as (url, requested):
        root = Path(directory)
        env = {k: v for k, v in isolated_env(root).items() if not k.lower().startswith('npm_config_')}
        config_root, data_root = Path(env['USAGESTAT_CONFIG_DIR']), Path(env['USAGESTAT_DATA_DIR'])
        config = config_root / 'config.toml'
        # Empty explicit fixture resources keep probes away from real accounts.
        config.write_text('providers = []\n', encoding='utf-8')
        for name in ['npm-user', 'npm-global']:
            (root / name).write_text('', encoding='utf-8')
        env.update(NPM_CONFIG_USERCONFIG=str(root / 'npm-user'), NPM_CONFIG_GLOBALCONFIG=str(root / 'npm-global'),
                   NPM_CONFIG_REGISTRY=url, NPM_CONFIG_CACHE=str(root / 'npm-cache'), NPM_CONFIG_UPDATE_NOTIFIER='false')
        prefix = root / 'global prefix 使用'

        def execute(args, *, expected=0, cwd=root):
            completed = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True,
                                       encoding='utf-8', errors='replace', timeout=120)
            assert completed.returncode == expected, (args, completed.returncode, completed.stdout, completed.stderr)
            return completed.stdout

        def npm(*args, **kwargs): return execute([*npm_command(), *args], **kwargs)
        flags = ['--ignore-scripts', '--no-audit', '--no-fund']
        before = {str(p.relative_to(config_root)): p.read_bytes() for p in config_root.rglob('*') if p.is_file()}
        npm('install', '--global', '--prefix', str(prefix), main['name'] + '@' + plan['version'], '--include=optional', *flags)
        assert before == {str(p.relative_to(config_root)): p.read_bytes() for p in config_root.rglob('*') if p.is_file()}
        assert not list(data_root.rglob('*')), 'npm install changed the data profile'
        result['checks'].append('global-install-scripts-disabled-no-profile-or-service-setup')
        modules = prefix / ('node_modules' if os.name == 'nt' else 'lib/node_modules')
        package_root = modules.joinpath(*main['name'].split('/'))
        cli = [node, str(package_root / 'bin/usagestat.cjs')]
        daemon = [node, str(package_root / 'bin/usagestatd.cjs')]
        host_os = {'Linux': 'linux', 'Darwin': 'darwin', 'Windows': 'win32'}[platform.system()]
        host_arch = 'arm64' if platform.machine().lower() in ('aarch64', 'arm64') else 'x64'
        advertised = any((p.get('packageJson') or {}).get('os') == [host_os] and p['packageJson'].get('cpu') == [host_arch] for p in plan['packages'])
        # Older local rehearsal plans omit inline package metadata.
        if not any('packageJson' in p for p in plan['packages']): advertised = True
        if not advertised:
            failed = subprocess.run([*cli, '--version'], cwd=root, env=env, capture_output=True, text=True, timeout=30)
            assert failed.returncode == 1 and 'target-not-qualified' in failed.stderr, failed.stderr
            npm('uninstall', '--global', '--prefix', str(prefix), main['name'], *flags)
            result['checks'].append('unqualified-stable-target-is-explicitly-unsupported')
            return result
        assert plan['version'] in execute([*cli, '--version'])
        assert plan['version'] in execute([*daemon, '--version'])
        providers = json.loads(execute([*cli, '--json', 'list']))
        assert providers and all(Path(p['icon']['path']).is_file() for p in providers if (p.get('icon') or {}).get('path'))
        execute([*cli, 'plugin', 'validate'])
        execute([*cli, 'config', 'validate'])
        literal_config = root / 'config & (literal) 使用.toml'
        literal_config.write_text('providers = []\n', encoding='utf-8')
        execute([*cli, '--config', str(literal_config), 'config', 'validate'])
        execute([*cli, '--definitely-invalid-option'], expected=2)
        if expect_doctor:
            report = json.loads(execute([*cli, 'doctor', '--json']))
            assert report['readOnly'] and any(c['id'] == 'backend' and c['code'] == 'ready' for c in report['checks'])
        # npm's generated shim itself is exercised, without asking a shell to
        # parse any user-controlled arguments. cmd is only used for the static
        # Windows .cmd smoke command, with a fixture-owned prefix and --version.
        if os.name == 'nt':
            shim = prefix / 'usagestat.cmd'
            cmd = os.environ.get('COMSPEC', r'C:\Windows\System32\cmd.exe')
            # Python's list quoting targets the C runtime; cmd.exe has its own
            # /s /c grammar. Pass this fixed fixture-only command line unchanged.
            execute(f'"{cmd}" /d /s /c ""{shim}" --version"')
        else: execute([str(prefix / 'bin/usagestat'), '--version'])
        result['checks'].append('installed-cli-daemon-versions-resources-validation-and-npm-shims')

        # Start the packaged backend with every real provider explicitly
        # disabled. Health checks must never access a developer/provider account.
        config.write_text('\n'.join('[[providers]]\nid = ' + json.dumps(p['id']) + '\nenabled = false\n' for p in providers), encoding='utf-8')
        with socket.socket() as held:
            held.bind(('127.0.0.1', 0))
            bind = f'127.0.0.1:{held.getsockname()[1]}'
        control_key = data_root / 'npm-fixture-control'
        control_key.write_text('synthetic-npm-shutdown', encoding='utf-8')
        control_key.chmod(0o600)
        child = subprocess.Popen([*daemon, '--bind', bind, '--config', str(config), '--control-key-file', str(control_key)], cwd=root, env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                assert child.poll() is None, child.communicate()
                try:
                    with opener.open('http://' + bind + '/health', timeout=1) as response: health = json.load(response)
                    if health.get('version') == plan['version']: break
                except (OSError, urllib.error.URLError): pass
                time.sleep(0.05)
            else: raise AssertionError('Packaged npm backend did not become healthy')
            if os.name != 'nt':
                child.terminate()
                child.communicate(timeout=8)
                assert child.returncode == 0, child.returncode
            else:
                # Windows Node kill is abrupt. Use native authenticated control
                # for graceful shutdown and verify the wrapper propagates exit.
                token = control_key.read_text().strip()
                request = urllib.request.Request('http://' + bind + '/v1/daemon/shutdown', method='POST',
                    headers={'Authorization': 'Bearer ' + token})
                with opener.open(request, timeout=2) as response: assert json.load(response)['status'] == 'stopping'
                child.communicate(timeout=8)
                assert child.returncode == 0
        finally:
            if child.poll() is None:
                child.terminate()
                child.communicate(timeout=8)
        result['checks'].append('packaged-daemon-health-graceful-stop-and-wrapper-exit')

        # Project-local npm exec uses the same durable exact-version payloads.
        project = root / 'project 使用'
        project.mkdir()
        (project / 'package.json').write_text('{"private":true}', encoding='utf-8')
        npm('install', main['name'] + '@' + plan['version'], '--include=optional', *flags, cwd=project)
        assert plan['version'] in npm('exec', '--offline', '--', 'usagestat', '--version', cwd=project)
        result['checks'].append('project-local-npm-exec')

        assert plan['version'] in npm('exec', '--yes', '--package=' + main['name'] + '@' + plan['version'], '--', 'usagestat', '--version')
        # Autostart from the real npm exec cache is rejected before any service
        # operation. This is separate from durable project-local execution.
        temporary = subprocess.run([*npm_command(), 'exec', '--yes', '--package=' + main['name'] + '@' + plan['version'],
            '--', 'usagestat', 'daemon', 'enable'], cwd=root, env=env, capture_output=True, text=True, timeout=120)
        assert temporary.returncode == 1 and 'temporary npm execution cannot own autostart' in temporary.stderr, temporary.stderr
        assert not (config_root / 'daemon.json').exists()
        result['checks'].append('one-off-npm-exec-rejects-ephemeral-autostart')

        # Omitted optional packages fail before executing any backend.
        omitted = root / 'optional omitted'
        omitted.mkdir()
        (omitted / 'package.json').write_text('{"private":true}', encoding='utf-8')
        npm('install', main['name'] + '@' + plan['version'], '--omit=optional', *flags, cwd=omitted)
        omitted_root = omitted / 'node_modules'
        omitted_main = omitted_root.joinpath(*main['name'].split('/'))
        failure = subprocess.run([node, str(omitted_main / 'bin/usagestat.cjs'), '--version'], cwd=root, env=env,
                                 capture_output=True, text=True, timeout=30)
        assert failure.returncode == 1 and 'native-package-missing' in failure.stderr, (failure.returncode, failure.stdout, failure.stderr,
            [str(p.relative_to(omitted)) for p in omitted.rglob('package.json')])
        result['checks'].append('omitted-optional-dependencies-actionable-failure')

        # Explicit update/removal retain user data, independently of hooks.
        sentinel = data_root / 'retain-history'
        sentinel.write_text('synthetic retained data', encoding='utf-8')
        npm('install', '--global', '--prefix', str(prefix), main['name'] + '@' + plan['version'], '--include=optional', *flags)
        assert plan['version'] in execute([*cli, '--version'])
        npm('uninstall', '--global', '--prefix', str(prefix), main['name'], *flags)
        assert sentinel.read_text() == 'synthetic retained data'
        assert not package_root.exists()
        result['checks'].append('stopped-reinstall-and-uninstall-retain-user-data')
        assert all(p.startswith(('/@hashimkarim/', '/tarballs/')) for p in requested), requested
        result['registry'] = 'public npm' if public_registry else 'isolated fixture'
        if not public_registry: result['registryRequests'] = len(requested)
    return result

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('packed', type=Path)
    parser.add_argument('--temp-dir', type=Path)
    parser.add_argument('--legacy-artifacts', action='store_true', help='Only for development against artifacts predating doctor')
    parser.add_argument('--public-registry', action='store_true', help='Install the exact version from public npm using an isolated profile/prefix')
    args = parser.parse_args()
    print(json.dumps(check(args.packed.resolve(), args.temp_dir, expect_doctor=not args.legacy_artifacts,
                          public_registry=args.public_registry), indent=2))
