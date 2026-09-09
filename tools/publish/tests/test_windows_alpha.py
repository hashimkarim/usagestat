import base64
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import windows_alpha as windows


class WindowsAlphaTests(unittest.TestCase):
    def recipes(self, channel, version='2.0.0-alpha.1'):
        return windows.recipes(windows.target(), 'v' + version, [dict(arch='x64', type='zip',
            bin=['usagestat.exe', 'usagestatd.exe'], scope='user',
            url='https://github.com/hashimkarim/usagestat/releases/download/v' + version + '/usagestat-windows-x86_64.zip',
            sha256='a' * 64)], channel)

    def test_prerelease_versions_and_command_inventory(self):
        self.assertEqual(windows.chocolatey_version('2.0.0-alpha.1'), '2.0.0-alpha000001')
        self.assertLess(windows.chocolatey_version('2.0.0-alpha.2'), windows.chocolatey_version('2.0.0-alpha.10'))
        for invalid in ['2.0.0', '2.0.0-beta.1', '2.0.0-alpha.01', '2.0.0-alpha.1000000']:
            with self.subTest(version=invalid), self.assertRaises(ValueError):
                windows.chocolatey_version(invalid)
        scoop = json.loads(next(iter(self.recipes('scoop').values())))
        self.assertEqual(scoop['bin'], ['usagestat.exe', 'usagestatd.exe'])
        self.assertNotIn('installer', scoop)
        winget = next(v for k, v in self.recipes('winget').items() if k.endswith('.installer.yaml'))
        self.assertIn('ArchiveBinariesDependOnPath: true', winget)
        self.assertNotIn('usagestat-service', winget)
        self.assertIn('usagestat-service.exe.ignore', self.recipes('chocolatey')['tools/chocolateyinstall.ps1'])

    def test_scoop_retries_preserve_newer_versions_and_same_version_bytes(self):
        files = self.recipes('scoop', '2.0.0-alpha.2')
        path, content = next(iter(files.items()))
        metadata = dict(private=False, permissions=dict(push=True))
        tree = dict(tree=[dict(path=path, sha='a' * 40)])
        for previous in ['2.0.0-alpha.2', '2.0.0-alpha.10']:
            old = next(iter(self.recipes('scoop', previous).values()))
            if previous.endswith('.2'):
                old = old.replace('Unsigned backend alpha.', 'Changed recipe.')
            with patch.object(windows, 'gh_api', side_effect=[metadata, tree,
                    dict(content=base64.b64encode(old.encode()).decode())]) as api, self.assertRaises(ValueError):
                windows.publish_scoop(windows.target(), files, 'fixture-token')
            self.assertTrue(all(len(c.args) == 1 for c in api.call_args_list))
        with patch.object(windows, 'gh_api', side_effect=[metadata, tree,
                dict(content=base64.b64encode(content.encode()).decode())]):
            self.assertEqual(windows.publish_scoop(windows.target(), files, 'fixture-token')['state'], 'unchanged')

    def test_chocolatey_checks_packed_files_and_fails_closed_on_registry_errors(self):
        files = self.recipes('chocolatey')
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary) / 'usagestat-alpha.2.0.0-alpha000001.nupkg'
            with zipfile.ZipFile(package, 'w') as archive:
                for name, content in files.items(): archive.writestr(name, content)
            missing = urllib.error.HTTPError('https://fixture.invalid', 404, 'missing', {}, None)
            with patch.object(windows.urllib.request, 'urlopen', side_effect=missing):
                windows.chocolatey_preflight(package, files, 'v2.0.0-alpha.1')
            failed = urllib.error.HTTPError('https://fixture.invalid', 403, 'denied', {}, None)
            with patch.object(windows.urllib.request, 'urlopen', side_effect=failed), self.assertRaises(ValueError):
                windows.chocolatey_preflight(package, files, 'v2.0.0-alpha.1')
            with patch.object(windows.urllib.request, 'urlopen'), self.assertRaises(ValueError):
                windows.chocolatey_preflight(package, files, 'v2.0.0-alpha.1')
            with zipfile.ZipFile(package, 'a') as archive: archive.writestr('tools/unreviewed.ps1', 'bad')
            with patch.object(windows.urllib.request, 'urlopen') as request, self.assertRaises(ValueError):
                windows.chocolatey_preflight(package, files, 'v2.0.0-alpha.1')
            request.assert_not_called()


if __name__ == '__main__':
    unittest.main()
