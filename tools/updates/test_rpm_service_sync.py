import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("rpm_service_sync", Path(__file__).with_name("rpm-service-sync.py"))
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)


class PackageTransaction(unittest.TestCase):
    def setUp(self):
        self.calls = []
        self.rows = [{"unit": "user@1001.service"}, {"unit": "user@1000.service"}]
        self.overrides = {}
        self.failed = set()

    def runner(self, args):
        self.calls.append(args)
        if "list-units" in args:
            return json.dumps(self.rows)
        uid = int(args[2].removeprefix("--machine=").removesuffix("@.host"))
        if uid in self.failed:
            raise subprocess.CalledProcessError(1, args)
        if "show" in args:
            return self.overrides.get(uid, sync.UNIT_PATH) + "\n"
        return ""

    def test_checks_run_as_existing_users_after_their_manager_reload(self):
        self.assertEqual(sync.queue_checks(self.runner), [1000, 1001])
        for uid in [1000, 1001]:
            command = ["systemctl", "--user", f"--machine={uid}@.host"]
            self.assertEqual([args for args in self.calls if args[:3] == command], [
                command + ["daemon-reload"],
                command + ["show", sync.UNIT, "--property=FragmentPath", "--value"],
                command + ["start", "--no-block", sync.UNIT],
            ])

    def test_root_duplicate_and_unrelated_units_are_excluded(self):
        self.rows += [{"unit": "user@0.service"}, {"unit": "user@1000.service"},
                      {"unit": "agenticdriver-litagent-usage.service"}, {"unit": "user@other.service"}]
        self.assertEqual(sync.queue_checks(self.runner), [1000, 1001])
        self.assertEqual(self.calls[0], ["systemctl", "list-units", "user@*.service", "--state=running", "--output=json", "--no-legend"])

    def test_user_override_or_mask_of_the_helper_is_preserved(self):
        self.overrides = {1000: "/home/user/.config/systemd/user/" + sync.UNIT, 1001: "/dev/null"}
        self.assertEqual(sync.queue_checks(self.runner), [])
        self.assertFalse(any("start" in args for args in self.calls))

    def test_unreachable_user_manager_does_not_skip_other_users(self):
        self.failed = {1000}
        self.assertEqual(sync.queue_checks(self.runner), [1001])

    def test_no_running_user_manager_requires_no_new_service(self):
        self.rows = []
        self.assertEqual(sync.queue_checks(self.runner), [])
        self.assertEqual(len(self.calls), 1)

    def test_container_without_systemd_does_not_fail_installation(self):
        with patch.object(sync.os, "geteuid", return_value=0), patch.object(sync, "queue_checks", side_effect=subprocess.CalledProcessError(1, "systemctl")):
            self.assertEqual(sync.main(), 0)

    def test_non_root_invocation_does_not_queue_changes(self):
        with patch.object(sync.os, "geteuid", return_value=1000), patch.object(sync, "queue_checks") as queue:
            self.assertEqual(sync.main(), 0)
            queue.assert_not_called()


if __name__ == "__main__":
    unittest.main()
