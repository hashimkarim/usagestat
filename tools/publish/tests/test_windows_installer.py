import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tools/publish/scripts'))
from native_artifacts import digest

SHELL = os.environ.get('USAGESTAT_TEST_POWERSHELL') or (str(Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe') if os.name == 'nt' else shutil.which('pwsh'))


@unittest.skipUnless(SHELL, 'PowerShell payload validation runs on Windows CI or with USAGESTAT_TEST_POWERSHELL')
class WindowsInstallerPayloadTests(unittest.TestCase):
    def test_verified_extraction_rejects_windows_aliases_links_and_tampering(self):
        with tempfile.TemporaryDirectory(prefix='installer payload ') as temporary:
            root = Path(temporary)
            cases = ['valid', 'traversal', 'case-alias', 'reserved', 'symlink', 'missing', 'hash', 'dirty-source', 'size']
            for case in cases:
                directory = root / case
                directory.mkdir()
                files = {'usagestat.exe': b'synthetic CLI', 'usagestatd.exe': b'synthetic daemon',
                         'usagestat-service.exe': b'synthetic supervisor', 'LICENSE': b'MIT',
                         'plugins/fixture/plugin.json': b'{"id":"fixture"}'}
                if case == 'traversal': files['plugins/../escape'] = b'escape'
                if case == 'case-alias': files['plugins/fixture/PLUGIN.json'] = b'alias'
                if case == 'reserved': files['plugins/fixture/CON.txt'] = b'reserved'
                raw = io.BytesIO()
                with zipfile.ZipFile(raw, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
                    for name, data in files.items():
                        if case == 'missing' and name == 'LICENSE': continue
                        entry = zipfile.ZipInfo(name)
                        entry.external_attr = (0o120777 if case == 'symlink' and name == 'LICENSE' else 0o100644) << 16
                        archive.writestr(entry, data)
                archive = raw.getvalue()
                name = 'usagestat-windows-x86_64.zip'
                manifest = {'schemaVersion': 1, 'package': 'usagestat', 'version': '1.0.3', 'sourceCommit': 'a' * 40,
                            'sourceDirty': case == 'dirty-source', 'target': 'x86_64-pc-windows-msvc', 'os': 'win32',
                            'arch': 'x64', 'signing': 'unsigned', 'archive': {'name': name, 'sha256': digest(archive), 'size': len(archive)},
                            'files': [{'path': path, 'sha256': digest(data), 'size': len(data)} for path, data in files.items()]}
                if case == 'hash': manifest['files'][0]['sha256'] = '0' * 64
                if case == 'size': manifest['files'][0]['size'] = 513 * 1024 * 1024
                for filename, data in [(name, archive), ('fixture.manifest.json', json.dumps(manifest).encode())]:
                    (directory / filename).write_bytes(data)
                    (directory / (filename + '.sha256')).write_text(f'{digest(data)}  {filename}\n')
            script = root / 'validate.ps1'
            script.write_text(r'''
param([string]$Root, [string]$Installer)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$tokens=$null; $errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile($Installer,[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
# Load function definitions only. Never run the installer's entry point or any
# synthetic executable; this gate tests extraction and ownership validation.
foreach ($definition in $ast.FindAll({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst]},$false)) {
    . ([ScriptBlock]::Create($definition.Extent.Text))
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$utf8=New-Object Text.UTF8Encoding($false)
$app='usagestat-dev'; $BackendProfile='dev'; $markerName='usagestat-installation.json'
function Check-Payload($Directory,$Record) { $script:checked=$true }
if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
    if (-not (Same-Path $Root ('\\?\'+$Root))) { throw 'Extended Windows owner path was not recognized.' }
}
$result=@()
foreach ($case in @('valid','traversal','case-alias','reserved','symlink','missing','hash','dirty-source','size')) {
    $prefix=Join-Path $Root ($case+'/destination'); $stage=Join-Path $Root ($case+'/stage')
    $script:checked=$false; $failed=$false
    try { $record=Stage-Payload $stage (Join-Path $Root ($case+'/fixture.manifest.json')) }
    catch { $failed=$true; if ($case -eq 'valid') { throw } }
    if ($case -eq 'valid') {
        if ($failed -or -not $script:checked) { throw 'Valid extraction failed.' }
        Assert-Owned $stage | Out-Null
        [IO.File]::WriteAllText((Join-Path $stage 'unrelated.txt'),'retained')
        $rejected=$false
        try { Assert-Owned $stage | Out-Null } catch { $rejected=$true }
        if (-not $rejected -or -not [IO.File]::Exists((Join-Path $stage 'unrelated.txt'))) { throw 'Unowned file was not preserved.' }
    } elseif (-not $failed -or $script:checked) { throw "Invalid payload reached execution: $case" }
    $result+=$case
}
$result | ConvertTo-Json -Compress
''', encoding='utf-8')
            result = subprocess.run([SHELL, '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
                                     '-File', str(script), str(root), str(ROOT / 'tools/install/Install-Usagestat.ps1')],
                                    capture_output=True, text=True, encoding='utf-8', timeout=90)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(result.stdout), cases)


if __name__ == '__main__': unittest.main()
