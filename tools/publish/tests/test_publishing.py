import hashlib
import importlib.util
import io
import json
import os
import subprocess
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'tools/publish/scripts' / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prepare = load('prepare', 'prepare-packages.py')
publication = load('publication', 'publication-state.py')


def make_archive(assets, arch, extra=None, plugin=b'{}'):
    path = assets / f'usagestat-linux-{arch}.tar.gz'
    with tarfile.open(path, 'w:gz') as archive:
        machine = {'x86_64': 62, 'aarch64': 183}[arch]
        elf = b'\x7fELF\x02\x01' + bytes(12) + machine.to_bytes(2, 'little')
        for name, data in [('usagestat', elf), ('usagestatd', elf), ('LICENSE', b'MIT'), ('plugins/codex/plugin.json', plugin)]:
            member = tarfile.TarInfo(name)
            member.size = len(data)
            member.mode = 0o755 if name.startswith('usagestat') else 0o644
            archive.addfile(member, io.BytesIO(data))
        if extra:
            archive.addfile(extra)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    path.with_name(path.name + '.sha256').write_text(f'{digest}  {path.name}\n')


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.assets = self.base / 'assets'
        self.assets.mkdir()
        for arch in ['x86_64', 'aarch64']:
            make_archive(self.assets, arch)

    def test_rejects_prerelease_and_injected_tags(self):
        for tag in ['1.0.3', 'v1.0.3-beta.2', 'v01.0.3', 'v1.0.3\n', 'v1.0.3;echo bad', '../main']:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                prepare.stable_version(tag)
        self.assertEqual(prepare.stable_version('v12.3.0'), '12.3.0')

    def test_rejects_corrupted_archive_and_checksum_filename(self):
        checksum = self.assets / 'usagestat-linux-x86_64.tar.gz.sha256'
        original = checksum.read_text()
        for invalid in ['0' * 64 + '  usagestat-linux-x86_64.tar.gz', original.replace('usagestat-linux-x86_64.tar.gz', '../bad')]:
            checksum.write_text(invalid)
            with self.assertRaises(ValueError):
                prepare.unpack(self.assets, 'x86_64', self.base / 'out')

    def test_rejects_traversal_links_and_duplicate_members(self):
        for name, kind in [('../escape', tarfile.REGTYPE), ('plugins/link', tarfile.SYMTYPE), ('usagestat', tarfile.REGTYPE)]:
            member = tarfile.TarInfo(name)
            member.type = kind
            member.linkname = '/etc/passwd'
            make_archive(self.assets, 'x86_64', extra=member)
            with self.subTest(name=name), self.assertRaises(ValueError):
                prepare.unpack(self.assets, 'x86_64', self.base / 'out')
        self.assertFalse((self.base / 'escape').exists())

    def test_wrong_architecture_is_rejected(self):
        src = self.assets / 'usagestat-linux-aarch64.tar.gz'
        dest = self.assets / 'usagestat-linux-x86_64.tar.gz'
        dest.write_bytes(src.read_bytes())
        dest.with_name(dest.name + '.sha256').write_text(hashlib.sha256(dest.read_bytes()).hexdigest() + '  ' + dest.name)
        with self.assertRaises(ValueError):
            prepare.unpack(self.assets, 'x86_64', self.base / 'out')

    def test_architecture_plugins_must_match(self):
        make_archive(self.assets, 'aarch64', plugin=b'{"different":true}')
        with self.assertRaises(ValueError):
            prepare.prepare('v1.0.3', self.assets, self.base / 'out', ROOT)

    def test_recipes_and_reproducible_ppa_source(self):
        for name in ['first', 'second']:
            prepare.prepare('v9.8.7', self.assets, self.base / name, ROOT)
        out = self.base / 'first'
        self.assertIn('pkgver=9.8.7', (out / 'PKGBUILD').read_text())
        self.assertIn('version "9.8.7"', (out / 'usagestat.rb').read_text())
        self.assertIn('Version:        9.8.7', (out / 'usagestat.spec').read_text())
        for recipe in ['PKGBUILD', 'usagestat.rb', 'usagestat.spec',
                       'usagestat-9.8.7/debian/control', 'usagestat-9.8.7/debian/copyright']:
            with self.subTest(recipe=recipe):
                content = (out / recipe).read_text()
                self.assertIn('https://github.com/hashimkarim/usagestat', content)
                self.assertNotIn('github.com/Hashim-K/', content)
        self.assertEqual((out / 'usagestat_9.8.7.orig.tar.gz').read_bytes(), (self.base / 'second/usagestat_9.8.7.orig.tar.gz').read_bytes())
        with tarfile.open(out / 'usagestat_9.8.7.orig.tar.gz') as archive:
            names = archive.getnames()
            for arch in ['amd64', 'arm64']:
                self.assertIn(f'usagestat-9.8.7/bin/{arch}/usagestatd', names)
            self.assertFalse(any('/debian' in name for name in names))

    def test_debian_install_replaces_epoch_zero_timestamps(self):
        prepare.prepare('v1.0.3', self.assets, self.base / 'out', ROOT)
        extracted = self.base / 'build'
        with tarfile.open(self.base / 'out/usagestat_1.0.3.orig.tar.gz') as archive:
            archive.extractall(extracted, filter='data')
        source = extracted / 'usagestat-1.0.3'
        self.assertEqual((source / 'plugins/codex/plugin.json').stat().st_mtime, 0)
        epoch = 1788566400
        subprocess.run(['make', '-f', str(ROOT / 'packaging/debian/rules'),
                        'override_dh_auto_install', 'DEB_HOST_ARCH=amd64'],
                       cwd=source, env={**os.environ, 'SOURCE_DATE_EPOCH': str(epoch)},
                       check=True, capture_output=True)
        installed = source / 'debian/usagestat'
        for path in [installed, *installed.rglob('*')]:
            self.assertEqual(path.stat().st_mtime, epoch, str(path))
        self.assertEqual((installed / 'usr/share/usagestat/plugins/codex/plugin.json').read_bytes(), b'{}')
        self.assertTrue(os.access(installed / 'usr/bin/usagestatd', os.X_OK))


class HomebrewPublishingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.prepared = self.base / 'prepared'
        (self.prepared / 'x86_64').mkdir(parents=True)
        (self.prepared / 'usagestat.rb').write_text('# test formula\n')
        self.bin = self.base / 'bin'
        self.bin.mkdir()
        self.log = self.base / 'commands.log'
        self.log.touch()
        self.executable(self.prepared / 'x86_64/usagestat', 'echo "usagestat 1.0.3"\n')
        self.executable(self.prepared / 'x86_64/usagestatd', 'exit 0\n')
        self.executable(self.bin / 'ruby', 'exit 0\n')
        # Replace network and Git operations, while exercising the real publisher.
        self.executable(self.bin / 'python3', 'echo python3 >> "$COMMAND_LOG"\n')
        self.executable(self.bin / 'git', '''printf 'git %s\\n' "$*" >> "$COMMAND_LOG"
if [ "$1" = clone ]; then
    mkdir -p "$3/Formula"
elif [ "$1" = diff ]; then
    exit 1
fi
''')
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith('HOMEBREW_') and key != 'GITHUB_STEP_SUMMARY'}
        self.env.update(PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        RELEASE_TAG='v1.0.3', DRY_RUN='false', COMMAND_LOG=str(self.log))

    @staticmethod
    def executable(path, body):
        path.write_text('#!/bin/sh\nset -eu\n' + body)
        path.chmod(0o755)

    def publish(self, **env):
        return subprocess.run(
            ['bash', str(ROOT / 'tools/publish/scripts/publish-platform.sh'),
             'homebrew', str(self.prepared)],
            env={**self.env, **env}, text=True, capture_output=True, timeout=10)

    def test_missing_tap_fails_before_network_access(self):
        result = self.publish(HOMEBREW_SSH_PRIVATE_KEY='test key')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('HOMEBREW_TAP_REPOSITORY', result.stderr)
        self.assertEqual(self.log.read_text(), '')

    def test_configured_tap_is_used_for_publication(self):
        result = self.publish(HOMEBREW_SSH_PRIVATE_KEY='test key',
                              HOMEBREW_TAP_REPOSITORY='test-owner/homebrew-configured')
        self.assertEqual(result.returncode, 0, result.stderr)
        commands = self.log.read_text().splitlines()
        self.assertIn(f'git clone git@github.com:test-owner/homebrew-configured.git {self.prepared}/tap', commands)
        self.assertIn('git push origin HEAD', commands)
        self.assertEqual((self.prepared / 'tap/Formula/usagestat.rb').read_text(),
                         (self.prepared / 'usagestat.rb').read_text())

    def test_dry_run_needs_no_tap_or_publishing_key(self):
        result = self.publish(DRY_RUN='true')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.log.read_text(), '')


class PublicationTests(unittest.TestCase):
    def test_ppa_recovery_revision(self):
        self.assertEqual(publication.ppa_version('1.0.3'), '1.0.3-1ppa2')
        self.assertEqual(publication.ppa_version('1.0.4'), '1.0.4-1ppa1')

    def test_copr_selects_latest_attempt_and_waits_for_pending(self):
        builds = [{'id': 1, 'source_package': {'version': '1.0.3-1'}, 'state': 'succeeded'}, {'id': 2, 'source_package': {'version': '1.0.3-1'}, 'state': 'running'}, {'id': 3, 'source_package': None, 'state': 'failed'}]
        with patch.object(publication, 'get', return_value={'items': builds}):
            self.assertEqual(publication.state('copr', '1.0.3'), 'pending')
            builds[1]['state'] = 'succeeded'
            self.assertEqual(publication.state('copr', '1.0.3'), 'done')
            builds[1]['state'] = 'failed'
            self.assertEqual(publication.state('copr', '1.0.3'), 'failed')
            self.assertEqual(publication.state('copr', '9.8.7'), 'missing')

    def test_network_failure_is_not_treated_as_missing(self):
        with patch.object(publication, 'get', side_effect=OSError('network failed')):
            with self.assertRaises(OSError):
                publication.state('copr', '1.0.3')

    def test_ppa_requires_binaries_published_for_every_build(self):
        entry = {'source_package_version': '1.0.3-1ppa2', 'status': 'Published', 'date_created': '2026-09-06', 'self_link': 'source'}
        builds = [{'buildstate': 'Successfully built', 'distro_series_link': 'noble', 'arch_tag': arch} for arch in ['amd64', 'arm64']]
        binaries = [{'binary_package_version': '1.0.3-1ppa2', 'distro_arch_series_link': 'noble/amd64'}]
        def get(url):
            if 'getPublishedSources' in url:
                self.assertIn('version=1.0.3-1ppa2', url)
                return {'entries': [entry]}
            if 'getBuilds' in url:
                return {'entries': builds}
            return {'entries': binaries}
        with patch.object(publication, 'get', side_effect=get):
            self.assertEqual(publication.state('ppa', '1.0.3'), 'pending')
            binaries.append({'binary_package_version': '1.0.3-1ppa2', 'distro_arch_series_link': 'noble/arm64'})
            self.assertEqual(publication.state('ppa', '1.0.3'), 'done')
            builds[1]['buildstate'] = 'Failed to build'
            with self.assertRaises(RuntimeError):
                publication.state('ppa', '1.0.3')


if __name__ == '__main__':
    unittest.main()
