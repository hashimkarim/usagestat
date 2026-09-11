#!/usr/bin/env python3
"""Verify a staged development payload using disposable state and no service mutation."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
from native_smoke import isolated_env, run
from stage_dev import stage
from native_artifacts import verify_provider_inventory


def check(binary_dir, target):
    with tempfile.TemporaryDirectory(prefix='usagestat development 使用 ') as temporary:
        root=Path(temporary).resolve()
        destination=root/'dev installation'
        manifest=stage(binary_dir,destination,target)
        assert manifest['profile']=='usagestat-dev'
        assert not any('.test.' in f['path'] or f['path'].startswith('tests/') for f in manifest['files'])
        env=isolated_env(root/'state')
        if os.name=='nt': env.pop('HOME',None)
        suffix='.exe' if os.name=='nt' else ''
        cli=destination/('usagestat-dev'+suffix)
        daemon=destination/('usagestatd-dev'+suffix)
        assert run(cli,['--version'],root,env).strip().endswith(manifest['version'])
        assert run(daemon,['--version'],root,env).strip().endswith(manifest['version'])
        caps=json.loads(run(cli,['capabilities','--json'],root,env))
        assert caps['profile']=='usagestat-dev'
        providers=json.loads(run(cli,['list','--json'],root,env))
        verify_provider_inventory(providers, manifest)
        for p in providers:
            icon=(p.get('icon') or {}).get('path')
            if icon:
                # Compare file identities: Windows extended/short path spellings
                # and macOS /var aliases can name the very same directory.
                assert Path(icon).is_file() and any(parent.samefile(destination) for parent in Path(icon).parents), (icon, str(destination))
        if os.name=='nt':
            assert (destination/'usagestat-service-dev.exe').is_file()
            assert run(destination/'usagestat-service-dev.exe',['--version'],root,env).strip().endswith(manifest['version'])
        before=(destination/'dev-installation.json').read_bytes()
        try: stage(binary_dir,destination,target)
        except ValueError: pass
        else: raise AssertionError('Existing development output was overwritten')
        assert (destination/'dev-installation.json').read_bytes()==before
        assert not (Path(env['USAGESTAT_CONFIG_DIR'])/'daemon.json').exists()
    return {'checks':['native-dev-binaries-and-windows-supervisor','dev-profile-from-executable-name',
                      'bundled-resources-without-source-cwd','existing-output-preserved','no-implicit-login-registration']}
