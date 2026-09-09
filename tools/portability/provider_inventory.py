#!/usr/bin/env python3
"""Inventory every declared provider/source and its actual native host mechanisms.

This is static source evidence, never a claim of credentialed compatibility.
--check detects changed manifests, entry scripts, host calls or classifications.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
# These notes record inspected implementation gaps, not unsupported upstream apps.
NOTES = {
    'amp': 'Home-relative CLI secrets layout carried from baseline; native upstream directories and overrides need verification.',
    'augment': 'Home auth file plus a Linux-only VS Code fallback; native IDE roots/profile selection remain to port.',
    'codebuff': 'Home-relative manicode credential path; XDG/native override behavior unverified.',
    'antigravity': 'Native IDE roots and profile/cache isolation fixtures; private OAuth schema/current app still unverified.',
    'antigravity-cli': 'Exact gemini/antigravity account only; upstream keyring target/encoding and CLI-state versions need verification.',
    'antigravity-ide': 'Native discovery/argument fixtures; real IDE version and process permissions pending #12/#20.',
    'claude': 'Native local collectors and macOS profile/account isolation fixtures. OAuth/web/API credentials and real app versions unverified.',
    'codex': 'Native file/direct-keyring/age auth, canonical profile/revision/account guards and collectors. Encrypted store is read-only; config layers and non-OAuth auth modes are not interpreted.',
    'copilot': 'Bounded CLI helper host and native home expansion; real gh/Copilot auth stores, CLI versions and account mappings unverified.',
    'cursor': 'Native DB resolver and stable/custom-profile isolation. Shared CLI fallback only for default stable profile; SQLite refresh writeback capability needs audit.',
    'cursor-nightly': 'Separate native DB and override; shared stable CLI auth/history excluded. SQLite refresh writeback capability needs audit.',
    'devin': 'Native IDE and Windows local-data primitives, explicit account selection. Current CLI credential-file location/schema and preview variants unverified.',
    'factory': 'Legacy file/keychain paths exist, but v2 encrypted files require AES-GCM host methods that are absent; formats/account selection need implementation and native evidence.',
    'droid': 'Aliases Factory code: v2 encrypted files require absent AES-GCM host methods; legacy methods and current auth formats remain unverified.',
    'gemini': 'Home OAuth files exist; OAuth-client extraction searches Unix/macOS package layouts. Windows/global-node installation discovery and current schemas remain to port.',
    'grok': 'Home credential-file method carried; CLI auth schema and declared cli/local equivalence need verification.',
    'jetbrains-ai-assistant': 'Native settings roots and explicit IDE selection fixtures; XML cache schema, actual IDE versions and account permissions pending.',
    'kilo': 'Configured API credentials plus home-relative CLI auth path; native upstream data roots/overrides unverified.',
    'kimi': 'Home credential-file reader/refresh carried; current CLI schema and store behavior unverified.',
    'kiro': 'Native app-support/custom-root fixtures for carried cached usage layouts; current Kiro auth/usage schema and app version unverified.',
    'opencode-go': 'Upstream XDG roots on all OSes, explicit data/database and inline auth overrides covered by fixtures. Preview channels require databasePath/OPENCODE_DB; current database schema, API, and account versions unverified.',
    'perplexity': 'Entry always uses legacy macOS CFNetwork cache, including declared web/oauth modes. Linux/Windows cache method explicitly unsupported; manual web-cookie implementation is absent.',
    'synthetic': 'Configured API credentials plus several CLI fallback files; account ambiguity and upstream data roots/overrides need audit.',
    't3chat': 'Manual cookies/full-cURL capture fixtures on all OS conventions. Challenge preserves credentials and gives full-cURL guidance. Real browser/device-bound sessions unverified.',
    'vertex-ai': 'Bounded gcloud helper and explicit/application credentials; actual native SDK installs and account selection unverified.',
    'windsurf': 'Native stable/Next/Devin profile selection fixtures; carried auth schemas and current application versions unverified.',
    'zed': 'Upstream native/Flatpak/custom data settings paths and JSONC fixtures. Explicit credentials available; real app versions and OS credential mappings unverified.',
}
PARTIAL = {'amp','augment','codebuff','factory','droid','gemini','kilo','synthetic'}
FIXTURES = {
    'claude': ['tests/provider-account-isolation.test.cjs','tools/portability/local_usage.py'],
    'codex': ['tests/codex-auth.test.cjs','tools/portability/codex_auth.py','tools/portability/local_usage.py'],
    'cursor': ['tests/provider-account-isolation.test.cjs'],
    'cursor-nightly': ['tests/provider-account-isolation.test.cjs'],
    'antigravity': ['tests/provider-remaining-paths.test.cjs','tools/portability/ide_discovery.py'],
    'antigravity-ide': ['tools/portability/ide_discovery.py'],
    'devin': ['tests/provider-remaining-paths.test.cjs'],
    'perplexity': ['tests/provider-remaining-paths.test.cjs'],
    'kiro': ['tests/provider-paths.test.cjs'],
    'windsurf': ['tests/provider-paths.test.cjs'],
    'jetbrains-ai-assistant': ['tests/provider-paths.test.cjs'],
    't3chat': ['tests/browser-manual-auth.test.cjs'],
    'opencode-go': ['tests/provider-xdg-paths.test.cjs'],
    'zed': ['tests/provider-xdg-paths.test.cjs'],
}


# Provider sync fixtures exercise the shipped helpers and synthetic API contracts.
SYNC_PROVIDERS = {
    'aiand', 'clawrouter', 'clinepass', 'codebuddy', 'deepinfra', 'ibmbob',
    'longcat', 'notion', 'qoder', 'qwencloud', 'sakana', 'sub2api', 'wayfinder',
    'xai', 'zenmux', 'zoommate', 'claude', 'copilot', 'cursor', 'cursor-nightly',
    'factory', 'droid', 'grok', 'kimi', 'kiro', 'ollama', 'opencode-go',
    'openrouter', 'zai',
}
for ident in SYNC_PROVIDERS:
    FIXTURES.setdefault(ident, []).extend([
        'tests/plugin-contracts.test.cjs', 'tests/provider-updates.test.cjs',
        'tests/quota-regressions.test.cjs',
    ])
for ident in ['codex', 'command-code', 'elevenlabs', 'kiro', 'minimax', 'moonshot', 'openrouter', 'poe']:
    FIXTURES.setdefault(ident, []).append('tests/codex-auth.test.cjs' if ident == 'codex' else 'tests/inspo-sync-20260909.test.cjs')
FIXTURES['codex'] = list(dict.fromkeys(FIXTURES['codex']))
FIXTURES.setdefault('kimi', []).append('tests/kimi-updates.test.cjs')
NOTES['factory'] = 'Configured API keys and manual web credentials are implemented; legacy file/keychain paths remain. V2 encrypted files still require absent AES-GCM host methods; real auth formats/account selection are unverified.'
NOTES['droid'] = 'Aliases Factory API/manual-web and legacy auth code; v2 encrypted files still require absent AES-GCM host methods. Live credentials remain unverified.'
NOTES['kimi'] = 'Configured Code API key and isolated CLI-home OAuth refresh plus optional membership/web metadata fixtures; real CLI/Desktop schemas and account stores remain unverified.'
NOTES['kiro'] = 'Native app-support/custom-root fixtures, included/overage credit parsing, and validated supported IDE API profile regions. CLI SQLite credential import and live app/account versions remain unverified.'

def inventory():
    records=[]
    for path in sorted((ROOT/'plugins').glob('*/plugin.json')):
        m=json.loads(path.read_text(encoding='utf-8')); ident=m['id']
        assert ident==path.parent.name,path
        modes=m.get('supportedModes',[]);assert modes and len(modes)==len(set(modes)),path
        entry=(path.parent/m['entry']).resolve();assert entry.is_relative_to(ROOT/'plugins'),path
        source=entry.read_text(encoding='utf-8')
        calls=sorted({'.'.join(call) for call in re.findall(r'(?:ctx\.)?host\.([A-Za-z0-9_]+)\.([A-Za-z0-9_]+)',source) if call[0]!='log'})
        literal_paths=sorted(set(re.findall(r'''["']((?:~/|/usr/|/opt/)[^"'\n]{0,160})["']''',source)))
        methods=[]
        if 'apiKey' in source or re.search(r'API_KEY|ACCESS_TOKEN|AUTH_TOKEN',source):methods.append('configured token/key')
        if 'cookieHeader' in source or '_COOKIE' in source:methods.append('manual web credential')
        if any(call.startswith('fs.') for call in calls):methods.append('files')
        if any(call.startswith('sqlite.') for call in calls):methods.append('SQLite')
        if any(call.startswith('keychain.') for call in calls):methods.append('OS store')
        if any(call.startswith('ls.') for call in calls):methods.append('IDE process')
        if 'command.run' in calls:methods.append('CLI helper')
        if any(call.startswith(('ccusage.','cursorLogs.','cursorUsageExport.')) for call in calls):methods.append('local/export collector')
        if ident=='codex':methods.append('native Codex auth adapter')
        if not methods:methods.append('provider-specific settings/HTTPS')
        note=NOTES.get(ident,'Configured credentials and HTTP request code inspected; real credentials, API responses and provider/account behavior remain unverified on each OS.')
        states={os:('P' if ident in PARTIAL else 'I') for os in ['linux','macos','windows']}
        if ident=='perplexity':states={'linux':'U-cache','macos':'P','windows':'U-cache'}
        records.append({'id':ident,'manifest':path.relative_to(ROOT).as_posix(),'entry':entry.relative_to(ROOT).as_posix(),'entrySha256':hashlib.sha256(entry.read_bytes()).hexdigest(),'declaredModes':modes,'autoMode':m.get('autoMode','auto'),'osStatus':states,'liveCredentialQualification':'unverified','methods':methods,'hostCalls':calls,'literalPathEvidence':literal_paths,'branchesOnSourceMode':'sourceMode' in source,'fixtures':FIXTURES.get(ident,[]),'auditNote':note})
    assert set(NOTES)<=set(r['id'] for r in records)
    return records

def render(records):
    lines=['# Provider compatibility inventory','',
        'Generated by `python tools/portability/provider_inventory.py`; validate with `--check`.',
        'The companion [machine-readable source audit](provider-audit.json) records entry hashes,',
        'native host calls, literal path evidence, fixture links and inspected gaps for every provider.','',
        '**I** = implemented, live credentials/app versions unverified. **P** = partial: a specific',
        'method/path gap is recorded below. **U-cache** = this entry uses an unsupported CFNetwork',
        'cache method on that OS; it does not mean the upstream provider/app is unsupported.',
        'No row is marked credential-verified. Native CI and fixture coverage are separate evidence.',
        'The obsolete “blocked on native build” baseline is removed: #3 is complete.','',
        'Manifest modes are declarations, not proof that the script implements distinct paths.',
        'Scripts that do not branch on `sourceMode` may share one mechanism across declared modes;',
        'the source audit records that fact rather than promising a separate web/OAuth/CLI flow.',
        'Manual web credentials and automatic browser import are separate. See',
        '[browser authentication](browser-authentication.md), [native paths](provider-paths.md),',
        '[Codex auth](codex-authentication.md), and [IDE discovery](ide-discovery.md).','',
        f"Inventory: {len(records)} providers; {sum(len(r['declaredModes']) for r in records)} declared provider/source pairs.",'',
        '| Provider | Source | Auto selects | Mechanisms in entry | Linux | macOS | Windows |',
        '| --- | --- | --- | --- | --- | --- | --- |']
    for r in records:
        for mode in r['declaredModes']:
            lines.append(f"| [{r['id']}](../{r['entry']}) | {mode} | {r['autoMode']} | {', '.join(r['methods'])} | {r['osStatus']['linux']} | {r['osStatus']['macos']} | {r['osStatus']['windows']} |")
    lines+=['','## Inspected method gaps and fixture coverage','',
        'Source inspection is the evidence for I/P/U-cache classifications. Fixtures below use',
        'synthetic data; file presence or a mocked HTTP response never verifies a real account.',
        'The same entry is audited once when several manifest modes share it.','',
        '| Provider | Audit finding | Relevant fixtures |','| --- | --- | --- |']
    for r in records:
        fixtures=', '.join(f'[{Path(p).name}](../{p})' for p in r['fixtures']) or 'Shared native host primitives only; provider fixture pending'
        lines.append(f"| [{r['id']}](../{r['entry']}) | {r['auditNote']} | {fixtures} |")
    lines+=['','Real-credential acceptance requires dated OS, architecture, app/CLI/browser version,',
        'authentication method, selected account/profile and expected-versus-observed normalized',
        'usage. Record only sanitized outcomes in #20; never attach cookies, tokens, raw auth',
        'files or browser database copies. Build floors and native session gates remain in',
        '[the support contract](platform-support.md).']
    return '\n'.join(lines)+'\n'

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--check',action='store_true');args=parser.parse_args()
    records=inventory()
    outputs={'docs/provider-compatibility.md':render(records),'docs/provider-audit.json':json.dumps({'schemaVersion':1,'evidence':'static-source-and-synthetic-fixtures','providers':records},indent=2,ensure_ascii=False)+'\n'}
    for name,result in outputs.items():
        path=ROOT/name
        if args.check:
            if not path.exists() or path.read_text(encoding='utf-8')!=result:raise SystemExit('Provider audit is stale; run tools/portability/provider_inventory.py')
        else:path.write_text(result,encoding='utf-8')
    print(f"Provider audit covers {len(records)} manifests and all declared source modes.")
