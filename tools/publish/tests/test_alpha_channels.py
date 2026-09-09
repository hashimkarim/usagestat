import importlib.util
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tools/publish/scripts'))
import homebrew_formula
import native_artifacts
import release_channel
import release_guard
import test_homebrew_formula as fixtures


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'tools/publish/scripts' / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


prepare = load('alpha_prepare', 'prepare-packages.py')
publication = load('alpha_publication', 'publication-state.py')


class AlphaChannelsTests(unittest.TestCase):
    def test_branch_dispatch_verifies_the_explicit_release_tag(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            data = json.dumps({'version': '2.0.0-alpha.1', 'sourceCommit': 'a' * 40}).encode()
            (root / 'usagestat-artifacts.json').write_bytes(data)
            release = dict(draft=False, prerelease=True, assets=[dict(name='usagestat-artifacts.json',
                size=len(data), digest='sha256:' + hashlib.sha256(data).hexdigest())])
            with patch.dict('os.environ', {'GITHUB_REPOSITORY': 'hashimkarim/usagestat', 'GITHUB_REF_NAME': 'feature/alpha'}), \
                    patch.object(release_guard, 'get', side_effect=[{}, {'object': {'type': 'commit', 'sha': 'a' * 40}}, release]) as get:
                self.assertFalse(release_guard.check(root, require_existing=True, tag='v2.0.0-alpha.1'))
                self.assertIn('git/ref/tags/v2.0.0-alpha.1', get.call_args_list[1].args[0])

    def test_channel_gates_and_numeric_alpha_order(self):
        for invalid in ['v2.0.0', 'v2.0.0-beta.1', 'v2.0.0-alpha.01', 'v2.0.0-alpha.1\n', 'v2.0.0-alpha.1;bad']:
            with self.subTest(tag=invalid), self.assertRaises(ValueError):
                release_channel.alpha_version(invalid)
        self.assertLess(release_channel.alpha_order('2.0.0-alpha.2'), release_channel.alpha_order('2.0.0-alpha.10'))
        release = dict(tag_name='v2.0.0-alpha.2', draft=False, prerelease=True)
        newer = dict(release, tag_name='v2.0.0-alpha.10')
        with patch.object(release_channel, 'get', side_effect=[release, [release, newer]]), self.assertRaises(ValueError):
            release_channel.check(release['tag_name'], 'alpha')
        with patch.object(release_channel, 'get', side_effect=[newer, [release, newer]]):
            self.assertEqual(release_channel.check(newer['tag_name'], 'alpha'), newer)
        with self.assertRaises(ValueError):
            release_channel.check(release['tag_name'], 'stable')

    def test_alpha_recipes_use_distinct_names_and_native_version_order(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            assets = root / 'assets'; assets.mkdir()
            fixtures.HomebrewTests().inputs(assets)
            aggregate = json.loads((assets / 'usagestat-artifacts.json').read_text())
            aggregate['version'] = '2.0.0-alpha.1'
            for manifest in aggregate['targets']:
                manifest['version'] = aggregate['version']
                name = native_artifacts.TARGETS[manifest['target']]['asset'] + '.manifest.json'
                self.checked(assets, name, manifest)
            self.checked(assets, 'usagestat-artifacts.json', aggregate)
            output = root / 'prepared'
            prepare.prepare('v2.0.0-alpha.1', assets, output, ROOT, 'alpha')
            aur = (output / 'PKGBUILD').read_text()
            self.assertIn('pkgname=usagestat-alpha-bin', aur)
            self.assertIn('pkgver=2.0.0alpha.1', aur)
            self.assertIn('_upstream_version=2.0.0-alpha.1', aur)
            rpm = (output / 'usagestat.spec').read_text()
            self.assertIn('Version:        2.0.0~alpha.1', rpm)
            self.assertIn('v%{upstream_version}.tar.gz', rpm)
            self.assertTrue((output / 'usagestat_2.0.0~alpha.1.orig.tar.gz').is_file())
            brew = (output / 'usagestat.rb').read_text()
            self.assertIn('class UsagestatAlpha < Formula', brew)
            self.assertIn('conflicts_with "usagestat"', brew)
            self.assertIn('on_macos', brew)
            self.assertEqual(brew.count('sha256 "'), 4)
            self.assertNotIn('file://', brew)
            with self.assertRaises(ValueError):
                homebrew_formula.generate(assets)
            with self.assertRaises(ValueError):
                homebrew_formula.generate(assets, alpha=True)

    @staticmethod
    def checked(root, name, data):
        content = json.dumps(data).encode()
        (root / name).write_bytes(content)
        (root / (name + '.sha256')).write_text(native_artifacts.digest(content) + '  ' + name + '\n')

    def test_alpha_remote_checks_never_query_the_stable_repository(self):
        build = dict(id=1, state='succeeded', source_package=dict(version='2.0.0~alpha.1-1'))
        with patch.object(publication, 'get', return_value={'items': [build]}) as get:
            self.assertEqual(publication.state('copr', '2.0.0-alpha.1', 'alpha'), 'done')
            self.assertIn('projectname=usagestat-alpha&', get.call_args.args[0])
        with patch.object(publication, 'get', return_value={'entries': []}) as get:
            self.assertEqual(publication.state('ppa', '2.0.0-alpha.1', 'alpha'), 'missing')
            self.assertIn('/usagestat-alpha?', get.call_args.args[0])
            self.assertIn('version=2.0.0~alpha.1-1ppa1', get.call_args.args[0])


if __name__ == '__main__':
    unittest.main()
