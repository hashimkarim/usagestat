import base64
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tools/publish/scripts'))
from npm_publish import validate, existing_matches, publication_state, version_order, check_default_tag, registry_package
from npm_packages import dist_tag

class NpmPublicationTests(unittest.TestCase):
    def test_install_metadata_and_full_version_verify_new_packages_without_overview(self):
        name, version = '@fixture/native', '2.0.0-alpha.1'
        full = {'name': name, 'version': version, 'libc': ['glibc'], 'dist': {'integrity': 'sha512-fixture'}}
        brief = {'dist-tags': {'alpha': version}, 'versions': {version: {'dist': full['dist']}}}
        with patch('npm_publish.registry_json', side_effect=[brief, full]) as read:
            result = registry_package('https://registry.npmjs.org/', name, version)
            self.assertEqual(result['versions'][version]['libc'], ['glibc'])
            self.assertEqual(read.call_args_list[0].args[1], 'application/vnd.npm.install-v1+json')
            self.assertTrue(read.call_args_list[1].args[0].endswith('/' + version))
        for missing_or_mismatched in [None, dict(full, name='wrong'), dict(full, dist={'integrity': 'different'})]:
            with patch('npm_publish.registry_json', side_effect=[brief, missing_or_mismatched]):
                with self.assertRaises(ValueError):
                    registry_package('https://registry.npmjs.org/', name, version)

    def test_first_alpha_default_is_allowed_but_existing_defaults_are_preserved(self):
        version = '2.0.0-alpha.1'
        first = {'dist-tags': {'alpha': version, 'latest': version}, 'versions': {version: {}}}
        check_default_tag(None, first, version, 'alpha')
        check_default_tag(first, first, version, 'alpha')
        stable = {'dist-tags': {'latest': '1.0.0'}, 'versions': {'1.0.0': {}}}
        published = {'dist-tags': {'latest': '1.0.0', 'alpha': version},
                     'versions': {'1.0.0': {}, version: {}}}
        check_default_tag(stable, published, version, 'alpha')
        with self.assertRaises(ValueError):
            check_default_tag(stable, first, version, 'alpha')
        with self.assertRaises(ValueError):
            check_default_tag(None, published, version, 'alpha')

    def test_alpha_has_an_explicit_dist_tag(self):
        self.assertEqual(dist_tag('2.0.0-alpha.1', 'prerelease'), 'alpha')
        self.assertEqual(dist_tag('2.0.0-beta.1', 'prerelease'), 'next')
        self.assertEqual(dist_tag('2.0.0', 'stable'), 'latest')

    def test_retry_preserves_newer_tags_and_reports_unpromoted_existing_versions(self):
        package = {'name': '@fixture/native', 'integrity': 'sha512-fixture',
            'packageJson': {'name': '@fixture/native', 'version': '1.2.3'}}
        document = {'dist-tags': {'latest': '1.2.3'}, 'versions': {'1.2.3': dict(package['packageJson'], dist={'integrity': package['integrity']})}}
        self.assertTrue(publication_state(package, document, '1.2.3', 'latest'))
        for tagged in ['1.2.2', '1.2.4']:
            document['dist-tags']['latest'] = tagged
            with self.assertRaises(ValueError): publication_state(package, document, '1.2.3', 'latest')
        self.assertLess(version_order('1.2.3-beta.9'), version_order('1.2.3-beta.10'))
        self.assertLess(version_order('1.2.3-rc.1'), version_order('1.2.3'))
        self.assertEqual(version_order('1.2.3+build.1'), version_order('1.2.3'))

    def test_exact_payload_graph_and_tarball_integrity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data = b'synthetic tarball for validation only'
            (root / 'fixture.tgz').write_bytes(data)
            platform = {'name': '@fixture/native', 'role': 'platform', 'tarball': 'fixture.tgz',
                'sha256': hashlib.sha256(data).hexdigest(),
                'integrity': 'sha512-' + base64.b64encode(hashlib.sha512(data).digest()).decode(),
                'packageJson': {'name': '@fixture/native', 'version': '1.2.3', 'private': False}}
            main = dict(platform, name='@fixture/main', role='main',
                packageJson={'name': '@fixture/main', 'version': '1.2.3', 'private': False,
                             'optionalDependencies': {'@fixture/native': '1.2.3'}})
            plan = {'schemaVersion': 1, 'version': '1.2.3', 'channel': 'stable', 'distTag': 'latest', 'packages': [main, platform]}
            self.assertEqual([p['role'] for p in validate(plan, root)], ['platform', 'main'])
            for mutation in ('private', 'version', 'graph', 'channel', 'tarball'):
                broken = json.loads(json.dumps(plan))
                if mutation == 'private': broken['packages'][0]['packageJson']['private'] = True
                elif mutation == 'version': broken['packages'][1]['packageJson']['version'] = '1.2.4'
                elif mutation == 'graph': broken['packages'][0]['packageJson']['optionalDependencies']['@fixture/native'] = '^1.2.3'
                elif mutation == 'channel': broken['channel'] = 'rehearsal'
                else: broken['packages'][0]['sha256'] = 'wrong'
                with self.assertRaises(ValueError): validate(broken, root)

    def test_retry_requires_identical_published_bytes_and_metadata(self):
        package = {'name': '@fixture/native', 'integrity': 'sha512-fixture',
                   'packageJson': {'name': '@fixture/native', 'version': '1.2.3', 'os': ['linux'], 'cpu': ['x64']}}
        remote = dict(package['packageJson'], dist={'integrity': package['integrity']})
        self.assertFalse(existing_matches(package, None))
        self.assertTrue(existing_matches(package, remote))
        with self.assertRaises(ValueError): existing_matches(package, dict(remote, dist={'integrity': 'different'}))
        with self.assertRaises(ValueError): existing_matches(package, dict(remote, os=['win32']))
