#!/usr/bin/env python3
"""Test the public alpha formula on a disposable native macOS runner."""
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
from homebrew_formula import generate


def check(artifacts):
    if platform.system() != 'Darwin' or os.environ.get('GITHUB_ACTIONS') != 'true':
        raise ValueError('Public formula installation requires a disposable macOS CI runner')
    formula = 'hashimkarim/tap/usagestat-alpha'
    with tempfile.TemporaryDirectory(prefix='usagestat-alpha-feed-') as temporary:
        profile = Path(temporary)
        env = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE='1', HOMEBREW_NO_INSTALL_CLEANUP='1',
                   HOMEBREW_NO_ANALYTICS='1', XDG_CONFIG_HOME=str(profile / 'brew-config'))
        def brew(*args):
            return subprocess.check_output(['brew', *args], env=env, text=True, timeout=300).strip()
        prefix = Path(brew('--prefix'))
        if any((prefix / 'bin' / name).exists() for name in ['usagestat', 'usagestatd']):
            raise ValueError('Refusing to replace an existing backend installation')
        brew('tap', 'hashimkarim/tap')
        tap = Path(brew('--repository', 'hashimkarim/tap'))
        expected = generate(artifacts, alpha=True, formula_name='usagestat-alpha')
        if (tap / 'Formula/usagestat-alpha.rb').read_text() != expected:
            raise ValueError('Public formula differs from the verified release recipe')
        if subprocess.run(['brew', 'command', 'trust'], env=env, capture_output=True).returncode == 0:
            brew('trust', '--formula', formula)
        config, data = profile / 'config', profile / 'data'
        config.mkdir(); data.mkdir()
        (config / 'config.toml').write_text('providers = []\n')
        (data / 'retained-fixture').write_text('synthetic history')
        runtime = dict(env, USAGESTAT_CONFIG_DIR=str(config), USAGESTAT_DATA_DIR=str(data))
        try:
            brew('install', '--formula', formula)
            brew('test', formula)
            cli = prefix / 'bin/usagestat'
            def backend(*args):
                return subprocess.check_output([str(cli), *args], env=runtime, text=True, timeout=30)
            version = json.loads((artifacts / 'usagestat-artifacts.json').read_text())['version']
            assert backend('--version').strip() == 'usagestat ' + version
            providers = json.loads(backend('--json', 'list'))
            assert len(providers) == 61
            assert all(Path(p['icon']['path']).is_file() for p in providers if (p.get('icon') or {}).get('path'))
            status = json.loads(backend('daemon', 'status', '--json'))
            assert not status['configured'] and not status['registered'] and not status['running']
        finally:
            brew('uninstall', '--formula', formula)
        assert (data / 'retained-fixture').read_text() == 'synthetic history'
        assert not (prefix / 'bin/usagestat').exists()
        return dict(version=version, checks=['public-formula-matches-verified-release', 'native-install-test-resources',
                    'no-implicit-startup', 'uninstall-retains-user-data'], desktopAcceptance='pending')


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    report = check(args.artifacts.resolve())
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + '\n')
