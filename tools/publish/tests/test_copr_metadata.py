import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock

ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = ROOT / 'tools/publish/scripts'
SPEC = importlib.util.spec_from_file_location('metadata', SCRIPTS / 'sync-copr-metadata.py')
metadata = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metadata)
DESIRED = json.loads((ROOT / 'packaging/copr/project.json').read_text())
PROJECT = DESIRED['homepage'].removeprefix('https://github.com/')


class MetadataTests(unittest.TestCase):
    def test_only_changed_public_metadata_is_written(self):
        proxy = Mock()
        proxy.get.return_value = dict(homepage=DESIRED['homepage'], description='old description',
                                     chroots=['existing-chroot'], instructions='keep these', bootstrap='off')
        self.assertTrue(metadata.sync(proxy, PROJECT, DESIRED))
        owner, project = PROJECT.split('/')
        proxy.edit.assert_called_once_with(ownername=owner, projectname=project,
                                           description=DESIRED['description'])

    def test_current_metadata_is_a_noop(self):
        proxy = Mock()
        proxy.get.return_value = DESIRED | {'instructions': 'keep these'}
        self.assertFalse(metadata.sync(proxy, PROJECT, DESIRED))
        proxy.edit.assert_not_called()

    def test_api_failure_does_not_write(self):
        proxy = Mock()
        proxy.get.side_effect = RuntimeError('API unavailable')
        with self.assertRaises(RuntimeError):
            metadata.sync(proxy, PROJECT, DESIRED)
        proxy.edit.assert_not_called()

    def test_build_settings_cannot_be_managed_as_metadata(self):
        proxy = Mock()
        with self.assertRaises(ValueError):
            metadata.sync(proxy, PROJECT, DESIRED | {'chroots': ['new-chroot']})
        proxy.get.assert_not_called()
        proxy.edit.assert_not_called()


class CoprPublishingTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.prepared = self.root / 'prepared'
        self.prepared.mkdir()
        (self.prepared / 'manifest.json').write_text('{"tag":"v9.8.7"}')
        self.log = self.root / 'commands.log'
        self.log.touch()
        commands = self.root / 'bin'
        commands.mkdir()
        # Run the real publisher and metadata helper with isolated fake services.
        stubs = {
            'docker': 'exit 0\n',
            'rpmspec': 'exit 0\n',
            'copr-cli': 'printf "build %s\\n" "$*" >> "$COMMAND_LOG"\n',
            'python3': '''case "$1" in
  */publication-state.py)
    case " $* " in *" --wait "*) exit 0 ;; esac
    exit "$REMOTE_STATE" ;;
  */release-info.py|*/release_channel.py) exit "$LATEST_STATUS" ;;
  -) case "${2:-}" in v*) exit "$LATEST_STATUS" ;; esac ;;
esac
exec ''' + shlex.quote(sys.executable) + ' "$@"\n',
        }
        for name, body in stubs.items():
            path = commands / name
            path.write_text('#!/bin/sh\nset -eu\n' + body)
            path.chmod(0o755)
        package = self.root / 'copr'
        package.mkdir()
        (package / '__init__.py').touch()
        (package / 'v3.py').write_text('''import json, os
from pathlib import Path

class Client:
    @staticmethod
    def create_from_config_file(filename):
        config = Path(filename)
        assert config.read_text().strip() == 'fixture config'
        assert config.stat().st_mode & 0o777 == 0o600
        with open(os.environ['COMMAND_LOG'], 'a') as log:
            log.write('config ' + filename + '\\n')
        return Client()

    @property
    def project_proxy(self):
        return self

    def get(self, **target):
        return {'homepage': None, 'description': 'old description'}

    def edit(self, **fields):
        if os.environ.get('SYNC_FAILURE') == 'true':
            raise RuntimeError('fixture config must never reach logs')
        with open(os.environ['COMMAND_LOG'], 'a') as log:
            log.write('edit ' + json.dumps(fields) + '\\n')
''')
        self.env = {key: value for key, value in os.environ.items()
                    if key not in {'COPR_CONFIG', 'GITHUB_STEP_SUMMARY', 'GNUPGHOME'}}
        self.env.update(PATH=str(commands) + os.pathsep + os.environ['PATH'],
                        PYTHONPATH=str(self.root), COPR_CONFIG='fixture config',
                        RELEASE_TAG='v9.8.7', DRY_RUN='false', REMOTE_STATE='0',
                        LATEST_STATUS='0', COMMAND_LOG=str(self.log), SYNC_FAILURE='false')

    def publish(self, **overrides):
        self.log.write_text('')
        result = subprocess.run(['bash', str(SCRIPTS / 'publish-platform.sh'),
                                 'copr', str(self.prepared)],
                                env=self.env | overrides, text=True, capture_output=True, timeout=10)
        for line in self.log.read_text().splitlines():
            if line.startswith('config '):
                self.assertFalse(Path(line.removeprefix('config ')).exists(), 'Temporary credentials leaked')
        return result

    def test_metadata_sync_runs_for_existing_and_new_versions(self):
        for state in ['0', '1']:
            with self.subTest(remote_state=state):
                result = self.publish(REMOTE_STATE=state)
                self.assertEqual(result.returncode, 0, result.stderr)
                lines = self.log.read_text().splitlines()
                edits = [line for line in lines if line.startswith('edit ')]
                owner, project = PROJECT.split('/')
                self.assertEqual([json.loads(line[5:]) for line in edits],
                                 [DESIRED | dict(ownername=owner, projectname=project)])
                builds = [line for line in lines if line.startswith('build ')]
                self.assertEqual(len(builds), int(state))
                if builds:
                    self.assertIn(PROJECT, builds[0])
                    self.assertLess(lines.index(edits[0]), lines.index(builds[0]))

    def test_dry_run_needs_no_credentials_and_never_changes_metadata(self):
        result = self.publish(DRY_RUN='true', COPR_CONFIG='')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.log.read_text(), '')

    def test_alpha_publication_only_updates_the_alpha_project(self):
        result = self.publish(RELEASE_TAG='v9.8.7-alpha.4', PACKAGE_CHANNEL='alpha', REMOTE_STATE='1')
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = self.log.read_text().splitlines()
        edits = [json.loads(line[5:]) for line in lines if line.startswith('edit ')]
        self.assertEqual(edits, [DESIRED | dict(ownername='hashimkarim', projectname='usagestat-alpha')])
        builds = [line for line in lines if line.startswith('build ')]
        self.assertEqual(len(builds), 1)
        self.assertIn('build hashimkarim/usagestat-alpha --enable-net', builds[0])

    def test_failed_status_or_stale_release_prevents_writes(self):
        for overrides in [dict(REMOTE_STATE='2'), dict(REMOTE_STATE='7'), dict(LATEST_STATUS='1')]:
            with self.subTest(overrides=overrides):
                result = self.publish(**overrides)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.log.read_text(), '')

    def test_metadata_failure_stops_submission_and_redacts_client_errors(self):
        result = self.publish(REMOTE_STATE='1', SYNC_FAILURE='true')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('build ', self.log.read_text())
        self.assertIn('COPR metadata sync failed', result.stderr)
        self.assertNotIn('fixture config', result.stderr + result.stdout)


if __name__ == '__main__':
    unittest.main()
