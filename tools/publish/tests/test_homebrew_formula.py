import copy
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'tools/publish/scripts'))
import homebrew_formula as brew
import native_artifacts as artifacts


class HomebrewTests(unittest.TestCase):
    def inputs(self, root):
        manifests = []
        for target, metadata in artifacts.TARGETS.items():
            if metadata['os'] == 'win32':
                continue
            if metadata['os'] == 'linux':
                binary = bytearray(64)
                binary[:6] = b'\x7fELF\x02\x01'
                struct.pack_into('<H', binary, 18, 62 if metadata['arch'] == 'x64' else 183)
            else:
                binary = bytearray(48)
                struct.pack_into('<IIIIIIII', binary, 0, 0xfeedfacf, 0x01000007 if metadata['arch'] == 'x64' else 0x0100000c,
                                 0, 2, 1, 16, 0, 0)
                struct.pack_into('<IIII', binary, 32, 0x24, 16, 11 << 16, 11 << 16)
            files = {'usagestat': bytes(binary), 'usagestatd': bytes(binary), 'LICENSE': b'MIT',
                     'plugins/fixture/plugin.json': b'{"id":"fixture"}'}
            archive = artifacts.archive_bytes(files, ['usagestat', 'usagestatd'], False)
            name = metadata['asset'] + '.tar.gz'
            manifest = dict(schemaVersion=1, version='1.0.3', sourceDirty=False, sourceCommit='a' * 40,
                target=target, os=metadata['os'], arch=metadata['arch'], minimumSystemQualified=metadata['os'] == 'linux',
                archive=dict(name=name, sha256=artifacts.digest(archive), size=len(archive)),
                resourcesSha256=artifacts.digest(json.dumps({n: artifacts.digest(v) for n, v in files.items()
                    if n not in ('usagestat', 'usagestatd')}, sort_keys=True).encode()),
                files=[dict(path=n, size=len(v), sha256=artifacts.digest(v), executable=n.startswith('usagestat')) for n, v in files.items()])
            for filename, data in [(name, archive), (metadata['asset'] + '.manifest.json', json.dumps(manifest).encode())]:
                (root / filename).write_bytes(data)
                (root / (filename + '.sha256')).write_text(artifacts.digest(data) + '  ' + filename + '\n')
            manifests.append(manifest)
        self.aggregate(root, manifests)
        return manifests

    def aggregate(self, root, manifests):
        data = json.dumps(dict(schemaVersion=1, version='1.0.3', sourceCommit='a' * 40, targets=manifests)).encode()
        (root / 'usagestat-artifacts.json').write_bytes(data)
        (root / 'usagestat-artifacts.json.sha256').write_text(artifacts.digest(data) + '  usagestat-artifacts.json\n')

    def test_native_archives_feed_qualified_stable_or_explicit_local_rehearsal(self):
        with tempfile.TemporaryDirectory(prefix='formula 使用 # ') as directory:
            root = Path(directory)
            manifests = self.inputs(root)
            stable = brew.generate(root)
            self.assertIn('depends_on :linux', stable)
            self.assertNotIn('on_macos', stable)
            self.assertEqual(stable.count('sha256 "'), 2)
            local = brew.generate(root, rehearsal=True)
            self.assertNotIn('depends_on :linux', local)
            self.assertIn('on_macos', local)
            self.assertEqual(local.count('sha256 "'), 4)
            self.assertIn('file://', local)
            self.assertIn('%23', local)
            self.assertNotIn('service do', local)
            with self.assertRaises(ValueError): brew.render('1.0.3', manifests)
            for value in (stable, local):
                subprocess.run(['ruby', '-c'], input=value, text=True, check=True, capture_output=True)

    def test_architecture_pairs_identity_and_ruby_inputs_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            manifests = self.inputs(Path(directory))[:2]
            for field, value in [('version', '2.0.0'), ('sourceCommit', 'b' * 40), ('sourceDirty', True),
                                 ('resourcesSha256', 'b' * 64), ('arch', 'invalid')]:
                changed = copy.deepcopy(manifests); changed[0][field] = value
                with self.subTest(field=field), self.assertRaises(ValueError): brew.render('1.0.3', changed)
            with self.assertRaises(ValueError): brew.render('1.0.3', manifests[:1])
            with self.assertRaises(ValueError): brew.render('1.0.3', manifests + manifests)
            for version in ['1.0.3-next', '1.0.3\n', '1.0.3#{exit}']:
                with self.assertRaises(ValueError): brew.render(version, manifests)
            with self.assertRaises(ValueError): brew.render('1.0.3', manifests, formula_name='#{exit}')

    def test_prereleases_are_local_rehearsals_only(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.inputs(root)
            complete = json.loads((root / 'usagestat-artifacts.json').read_text())
            for version in ['2.0.0-alpha.1', '2.0.0-beta.2']:
                manifests = [dict(m, version=version) for m in complete['targets']]
                with self.assertRaises(ValueError): brew.render(version, manifests)
                formula = brew.render(version, manifests, local_directory=root)
                self.assertIn(f'version "{version}"', formula)
                self.assertIn('Local unsigned rehearsal only', formula)
            for version in ['2.0.0-alpha.01', '2.0.0-alpha..1', '2.0.0-#{exit}', '2.0.0-alpha.1\n']:
                manifests = [dict(m, version=version) for m in complete['targets']]
                with self.subTest(version=version), self.assertRaises(ValueError):
                    brew.render(version, manifests, local_directory=root)

    def test_changed_manifest_or_archive_is_rejected_before_rendered_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); manifests = self.inputs(root)
            changed = copy.deepcopy(manifests); changed[0]['files'][0]['sha256'] = 'b' * 64
            self.aggregate(root, changed)
            with self.assertRaises(ValueError): brew.generate(root)
            self.aggregate(root, manifests)
            archive = root / manifests[0]['archive']['name']
            archive.write_bytes(archive.read_bytes() + b'corruption')
            with self.assertRaises(ValueError): brew.generate(root)


if __name__ == '__main__':
    unittest.main()
