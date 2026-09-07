import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'portability'))
import verify_native_reports as evidence


class NativeEvidenceTests(unittest.TestCase):
    def fixture(self, root):
        for target in evidence.TARGETS:
            directory = root / target
            directory.mkdir()
            report = {'suiteSchemaVersion': 1, 'sourceCommit': 'a' * 40, 'sourceDirty': False, 'target': target,
                      'checks': [{'name': name, 'exit_code': 0} for name in sorted(evidence.COMMANDS)]}
            report.update({section: {'checks': ['synthetic evidence']} for section in evidence.SECTIONS})
            if 'apple-darwin' in target or 'windows-msvc' in target:
                command, test = ('launchagent-tests', 'isolated_native_launchagent_lifecycle') if 'apple' in target else ('scheduled-task-tests', 'isolated_native_scheduled_task_lifecycle')
                report['checks'].append({'name': command, 'exit_code': 0})
                (directory / (command + '.log')).write_text(f'test fixture::{test} ... ok\n')
            if 'windows-msvc' in target:
                report['windows_service'] = {'checks': ['synthetic supervisor evidence']}
                (directory / 'windows-installer.json').write_text(json.dumps({'input': 'native-debug-fixture', 'checks': sorted(evidence.INSTALLER_CHECKS)}))
            (directory / 'report.json').write_text(json.dumps(report))

    def test_complete_reports_require_every_target_and_native_phase(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.fixture(root)
            self.assertEqual(evidence.verify(root, 'a' * 40)['result'], 'complete-native-fixtures')
            path = root / 'x86_64-pc-windows-msvc/report.json'
            original = json.loads(path.read_text())
            for modification in [{'error': ''}, {'sourceCommit': 'b' * 40}, {'sourceDirty': True}, {'dev_install': {}}, {'checks': []}]:
                path.write_text(json.dumps({**original, **modification}))
                with self.subTest(modification=modification), self.assertRaises(ValueError): evidence.verify(root, 'a' * 40)
            path.write_text(json.dumps(original))
            log = path.parent / 'scheduled-task-tests.log'
            log.write_text('test result: ok. 0 passed; 0 failed\n')
            with self.assertRaisesRegex(ValueError, 'did not actually complete'): evidence.verify(root, 'a' * 40)
            path.unlink()
            with self.assertRaisesRegex(ValueError, 'Missing native targets'): evidence.verify(root, 'a' * 40)


if __name__ == '__main__': unittest.main()
