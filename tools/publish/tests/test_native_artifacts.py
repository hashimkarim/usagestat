import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('artifacts', ROOT / 'tools/publish/scripts/native_artifacts.py')
artifacts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifacts)
guard_spec = importlib.util.spec_from_file_location('release_guard', ROOT / 'tools/publish/scripts/release_guard.py')
guard = importlib.util.module_from_spec(guard_spec)
guard_spec.loader.exec_module(guard)


class NativeArtifactTests(unittest.TestCase):
    def test_resource_allowlist_excludes_provider_tests_and_development_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            provider = root / 'plugins/fixture'
            provider.mkdir(parents=True)
            (provider / 'plugin.json').write_text(json.dumps({'entry': 'plugin.js'}))
            tracked = ['LICENSE', 'plugins/README.md', 'plugins/fixture/plugin.json', 'plugins/fixture/plugin.js',
                'plugins/fixture/plugin.test.js', 'plugins/fixture/icon.svg', 'plugins/fixture/icon-color.svg']
            self.assertEqual(artifacts.runtime_resource_names(root, tracked), [
                'LICENSE', 'plugins/fixture/icon-color.svg', 'plugins/fixture/icon.svg',
                'plugins/fixture/plugin.js', 'plugins/fixture/plugin.json'])
            (provider / 'plugin.json').write_text(json.dumps({'entry': '../outside.js'}))
            with self.assertRaises(ValueError): artifacts.runtime_resource_names(root, tracked)
            (provider / 'plugin.json').write_text(json.dumps({'entry': '../shared/plugin.js'}))
            selected = artifacts.runtime_resource_names(root, tracked + ['plugins/shared/plugin.js'])
            self.assertIn('plugins/shared/plugin.js', selected)
            self.assertNotIn('plugins/fixture/plugin.js', selected)

    def test_every_bundled_manifest_has_a_tracked_runtime_entry(self):
        import subprocess
        tracked = subprocess.check_output(['git', 'ls-files', '-z', '--', 'plugins', 'LICENSE'], cwd=ROOT).decode().split('\0')
        selected = artifacts.runtime_resource_names(ROOT, list(filter(None, tracked)))
        self.assertEqual(sum(name.endswith('/plugin.json') for name in selected), len(list((ROOT / 'plugins').glob('*/plugin.json'))))
        self.assertFalse(any(name.endswith('.test.js') for name in selected))

    def test_packaging_rejects_untracked_providers_and_retains_upstream_notices(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            provider = root / 'plugins/added'
            provider.mkdir(parents=True)
            (provider / 'plugin.json').write_text(json.dumps({'entry': 'plugin.js'}))
            notice = 'plugins/UPSTREAM-LICENSES.md'
            (root / notice).write_text('Upstream MIT notices')
            with self.assertRaisesRegex(ValueError, 'Commit provider resources'):
                artifacts.runtime_resource_names(root, ['LICENSE'])
            tracked = ['LICENSE', 'plugins/added/plugin.json', 'plugins/added/plugin.js']
            with self.assertRaisesRegex(ValueError, 'UPSTREAM-LICENSES'):
                artifacts.runtime_resource_names(root, tracked)
            selected = artifacts.runtime_resource_names(root, tracked + [notice])
            self.assertIn('plugins/added/plugin.json', selected)
            self.assertIn(notice, selected)

    def test_pinned_license_notices_reject_changes_and_unreviewed_versions(self):
        import shutil
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / 'tools/publish/licenses'
            shutil.copytree(ROOT / 'tools/publish/licenses', target)
            manifest = json.loads((target / 'manifest.json').read_text())
            for key, entry in manifest.items():
                name, version = key.rsplit('-', 1)
                package = dict(name=name, version=version, license=entry['license'], repository=entry['repository'])
                self.assertTrue(artifacts.pinned_license_notices(root, package))
                with self.assertRaises(ValueError): artifacts.pinned_license_notices(root, dict(package, version='999.0.0'))
                with self.assertRaises(ValueError): artifacts.pinned_license_notices(root, dict(package, license='changed'))
                file = target / entry['files'][0]['path']
                file.write_bytes(file.read_bytes() + b'changed')
                with self.assertRaises(ValueError): artifacts.pinned_license_notices(root, package)

    def fixture(self, files, executable, windows):
        data = artifacts.archive_bytes(files, executable, windows)
        name = 'fixture.zip' if windows else 'fixture.tar.gz'
        manifest = {'archive': {'name': name, 'sha256': artifacts.digest(data), 'size': len(data)},
                    'files': [{'path': name, 'size': len(value), 'sha256': artifacts.digest(value),
                               'executable': name in executable} for name, value in files.items()]}
        return name, data, manifest

    def unpack(self, root, name, data, manifest):
        path = root / name
        path.write_bytes(data)
        path.with_name(name + '.sha256').write_text(artifacts.digest(data) + '  ' + name + '\n')
        artifacts.unpack_checked(manifest, path, root / 'out')

    def test_deterministic_archives_preserve_unicode_resources_and_executable_modes(self):
        files = {'plugins/使用/plugin.js': b'synthetic', 'LICENSE': b'MIT', 'usagestat': b'fixture'}
        for windows in (False, True):
            name, data, manifest = self.fixture(files, ['usagestat'], windows)
            self.assertEqual(data, artifacts.archive_bytes(dict(reversed(list(files.items()))), ['usagestat'], windows))
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self.unpack(root, name, data, manifest)
                for name, value in files.items():
                    self.assertEqual((root / 'out' / name).read_bytes(), value)

    def test_archive_rejects_traversal_case_aliases_and_windows_path_forms(self):
        for bad in ('../escape', '/escape', 'plugins/../escape', 'C:/escape', 'plugins\\escape', './escape'):
            for windows in (False, True):
                with self.subTest(bad=bad, windows=windows), tempfile.TemporaryDirectory() as temporary:
                    name, data, manifest = self.fixture({bad: b'bad'}, [], windows)
                    with self.assertRaises(ValueError):
                        self.unpack(Path(temporary), name, data, manifest)
        with tempfile.TemporaryDirectory() as temporary:
            name, data, manifest = self.fixture({'plugins/A.js': b'a', 'plugins/a.js': b'b'}, [], True)
            with self.assertRaises(ValueError):
                self.unpack(Path(temporary), name, data, manifest)

    def test_archive_rejects_missing_modified_and_reclassified_files(self):
        for mutation in ('hash', 'mode', 'missing'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                name, data, manifest = self.fixture({'usagestat.exe': b'fixture'}, ['usagestat.exe'], True)
                if mutation == 'hash':
                    manifest['files'][0]['sha256'] = '0' * 64
                elif mutation == 'mode':
                    manifest['files'][0]['executable'] = False
                else:
                    manifest['files'].append(dict(manifest['files'][0], path='usagestatd.exe'))
                with self.assertRaises(ValueError):
                    self.unpack(Path(temporary), name, data, manifest)

    def test_native_headers_reject_wrong_architecture_and_increased_macos_floor(self):
        linux = bytearray(64)
        linux[:6] = b'\x7fELF\x02\x01'
        struct.pack_into('<H', linux, 18, 62)
        self.assertEqual(artifacts.inspect_binary(linux, 'x86_64-unknown-linux-gnu')['machine'], 62)
        with self.assertRaises(ValueError):
            artifacts.inspect_binary(linux, 'aarch64-unknown-linux-gnu')
        mac = bytearray(56)
        struct.pack_into('<IIIIIIII', mac, 0, 0xfeedfacf, 0x0100000c, 0, 2, 1, 24, 0, 0)
        struct.pack_into('<IIIIII', mac, 32, 0x32, 24, 1, 11 << 16, 15 << 16, 0)
        self.assertEqual(artifacts.inspect_binary(mac, 'aarch64-apple-darwin')['deploymentTarget'], '11.0.0')
        struct.pack_into('<I', mac, 44, 12 << 16)
        with self.assertRaises(ValueError):
            artifacts.inspect_binary(mac, 'aarch64-apple-darwin')
        pe = bytearray(512)
        pe[:2] = b'MZ'
        struct.pack_into('<I', pe, 60, 64)
        pe[64:68] = b'PE\0\0'
        struct.pack_into('<H', pe, 68, 0x8664)
        struct.pack_into('<H', pe, 88, 0x20b)
        self.assertEqual(artifacts.inspect_binary(pe, 'x86_64-pc-windows-msvc')['machine'], 0x8664)
        struct.pack_into('<H', pe, 68, 0xaa64)
        with self.assertRaises(ValueError):
            artifacts.inspect_binary(pe, 'x86_64-pc-windows-msvc')

    def test_stable_publication_excludes_candidates_and_prerelease_carries_them(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / 'assets'
            assets.mkdir()
            manifests = []
            for target in ['x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc']:
                metadata = artifacts.TARGETS[target]
                prefix = metadata['asset']
                archive = prefix + ('.zip' if metadata['os'] == 'win32' else '.tar.gz')
                manifest = {'target': target, 'os': metadata['os'], 'archive': {'name': archive},
                            'minimumSystemQualified': metadata['os'] == 'linux'}
                manifests.append(manifest)
                for name in [archive, prefix + '.manifest.json'] + ([prefix] if metadata['os'] == 'linux' else []):
                    data = b'synthetic artifact'
                    (assets / name).write_bytes(data)
                    (assets / (name + '.sha256')).write_text(artifacts.digest(data) + '  ' + name + '\n')
            script = (artifacts.ROOT / 'tools/install/Install-Usagestat.ps1').read_bytes()
            name = 'Install-Usagestat.ps1'
            (assets / name).write_bytes(script)
            (assets / (name + '.sha256')).write_text(artifacts.digest(script) + '  ' + name + '\n')
            complete = json.dumps({'targets': manifests, 'installers': {'windows': {'name': name, 'sha256': artifacts.digest(script)}}}).encode()
            (assets / 'usagestat-artifacts.json').write_bytes(complete)
            (assets / 'usagestat-artifacts.json.sha256').write_text(artifacts.digest(complete) + '  usagestat-artifacts.json\n')
            for channel, count in [('stable', 1), ('prerelease', 2)]:
                output = root / channel
                manifest = artifacts.prepare_publication(assets, output, channel)
                self.assertEqual(len(json.loads(manifest.read_text())['targets']), count)
                self.assertEqual(bool(list(output.glob('usagestat-windows-*'))), channel == 'prerelease')
                self.assertEqual((output / 'Install-Usagestat.ps1').exists(), channel == 'prerelease')

    def test_release_retry_preserves_identical_assets_and_rejects_changed_or_partial_release(self):
        digest = artifacts.digest(b'original')
        expected = {'usagestat-fixture.zip': (8, digest)}
        asset = {'name': 'usagestat-fixture.zip', 'size': 8, 'digest': 'sha256:' + digest}
        release = {'draft': False, 'prerelease': False, 'assets': [asset]}
        guard.compare_existing(release, expected, False)
        for invalid in [dict(release, draft=True), dict(release, prerelease=True),
                        dict(release, assets=[]), dict(release, assets=[dict(asset, digest='sha256:' + '0' * 64)]),
                        dict(release, assets=[dict(asset, size=9)])]:
            with self.assertRaises(ValueError):
                guard.compare_existing(invalid, expected, False)

    def test_release_verification_requires_the_windows_installer_and_checksum(self):
        names = ['usagestat-windows-x86_64.zip', 'Install-Usagestat.ps1', 'Install-Usagestat.ps1.sha256']
        digest = artifacts.digest(b'original')
        expected = {name: (8, digest) for name in names}
        assets = [{'name': name, 'size': 8, 'digest': 'sha256:' + digest} for name in names]
        release = {'draft': False, 'prerelease': True, 'assets': assets}
        guard.compare_existing(release, expected, True)
        for missing in names:
            with self.subTest(missing=missing), self.assertRaises(ValueError):
                guard.compare_existing(dict(release, assets=[a for a in assets if a['name'] != missing]), expected, True)
        changed = [dict(a, digest='sha256:' + '0' * 64) if a['name'] == 'Install-Usagestat.ps1' else a for a in assets]
        with self.assertRaises(ValueError): guard.compare_existing(dict(release, assets=changed), expected, True)


if __name__ == '__main__':
    unittest.main()
