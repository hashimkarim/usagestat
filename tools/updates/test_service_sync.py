import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("service_sync", Path(__file__).with_name("service-sync.py"))
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)


class ServiceUpgrade(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="usagestat sync 使用 ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.binary = self.root / "bin/usagestatd"
        self.binary.parent.mkdir()
        self.binary.write_text("new executable")
        self.previous = self.root / "previous-usagestatd"
        self.previous.write_text("old executable")
        self.saved = self.root / "daemon.json"
        self.saved.write_text(json.dumps({"t3Mode": "auto", "installation": {
            "owner": str(self.root), "binary": str(self.binary), "bind": "127.0.0.1:6736",
            "config": "retained-config", "managementKeyFile": "retained-t3-key", "environment": {"PRIVATE": "not-printed"}}}))
        self.original_settings = self.saved.read_bytes()
        self.unit = self.root / "usagestat.service"
        self.unit.write_text(sync.MARKER + f'[Service]\nExecStart="{self.binary}" --service-settings "{self.saved}"\n')
        self.proc = self.root / "proc"
        for pid, exe in [(100, self.previous), (101, self.binary)]:
            process = self.proc / str(pid)
            process.mkdir(parents=True)
            (process / "exe").symlink_to(exe)
            (process / "cmdline").write_bytes((str(self.binary) + "\0--service-settings\0" + str(self.saved) + "\0").encode())
        self.pid = 100
        self.active = "active"
        self.calls = []

    def runner(self, args):
        self.calls.append(args)
        if "show" in args:
            return f"LoadState=loaded\nActiveState={self.active}\nMainPID={self.pid}\nFragmentPath={self.unit}\n"
        if "try-restart" in args:
            self.pid = 101
            return ""
        if args == [str(self.binary), "--version"]:
            return "usagestatd 2.0.1\n"
        self.fail(args)

    def health(self, url):
        self.assertEqual(url, "http://127.0.0.1:6736/health")
        return {"application": "usagestat", "status": "ok", "owner": str(self.root), "profile": "usagestat",
                "pid": self.pid, "version": "2.0.1" if self.pid == 101 else "2.0.0"}

    def check(self, **kwargs):
        return sync.sync(self.saved, runner=self.runner, health=self.health, proc_root=self.proc, wait=lambda _: None, **kwargs)

    def test_atomic_package_replacement_restarts_only_the_running_native_service(self):
        self.assertEqual(self.check(), {"action": "restarted", "version": "2.0.1"})
        self.assertEqual([args for args in self.calls if "try-restart" in args],
                         [["systemctl", "--user", "try-restart", "usagestat.service"]])
        self.assertEqual(self.saved.read_bytes(), self.original_settings)

    def test_current_binary_does_not_restart(self):
        (self.proc / "100/exe").unlink()
        (self.proc / "100/exe").symlink_to(self.binary)
        self.assertEqual(self.check()["reason"], "current-executable")
        self.assertFalse(any("try-restart" in args for args in self.calls))

    def test_stopped_service_and_autostart_are_preserved(self):
        self.active = "inactive"
        self.assertEqual(self.check()["reason"], "service-not-running")
        self.assertFalse(any("try-restart" in args or "enable" in args for args in self.calls))

    def test_sdk_and_unmanaged_services_cannot_be_updated(self):
        with self.assertRaises(ValueError):
            sync.sync(self.saved, "agenticdriver-litagent-usage.service", runner=self.runner)
        self.unit.write_text(self.unit.read_text().replace(sync.MARKER, "# custom\n"))
        self.assertEqual(self.check()["reason"], "unmanaged-service")
        self.assertFalse(any("try-restart" in args for args in self.calls))

    def test_package_hook_discovers_the_registered_settings_file(self):
        self.assertEqual(sync.sync(runner=self.runner, health=self.health, proc_root=self.proc, wait=lambda _: None),
                         {"action": "restarted", "version": "2.0.1"})
        self.assertEqual(self.saved.read_bytes(), self.original_settings)

    def test_legacy_registration_is_left_for_its_owner(self):
        self.unit.write_text(sync.MARKER + '[Service]\nExecStart=/usr/bin/usagestatd --bind 127.0.0.1:6736\n')
        self.assertEqual(sync.sync(runner=self.runner)["reason"], "legacy-registration")
        self.assertFalse(any("try-restart" in args for args in self.calls))

    def test_rpm_hook_preserves_other_installations(self):
        self.assertEqual(self.check(rpm=True)["reason"], "different-package-owner")
        self.assertFalse(any("try-restart" in args for args in self.calls))

    def test_wrong_endpoint_owner_prevents_restart(self):
        self.health = lambda _: {"status": "ok", "application": "usagestat", "owner": "another installation", "pid": 100}
        with self.assertRaisesRegex(ValueError, "does not belong"):
            self.check()
        self.assertFalse(any("try-restart" in args for args in self.calls))

    def test_replaced_registration_or_process_prevents_restart(self):
        (self.proc / "100/cmdline").write_bytes(b"other-daemon\0")
        with self.assertRaisesRegex(ValueError, "running process"):
            self.check()
        self.assertFalse(any("try-restart" in args for args in self.calls))

    def test_failed_replacement_health_is_not_reported_as_success(self):
        actual = self.health
        self.health = lambda url: {**actual(url), "version": "2.0.0"}
        with self.assertRaisesRegex(RuntimeError, "did not become healthy"):
            self.check(attempts=2)

    def test_concurrent_settings_change_prevents_restart(self):
        actual = self.runner
        def runner(args):
            result = actual(args)
            if "--version" in args:
                self.saved.write_text('{"installation":null}')
            return result
        self.runner = runner
        with self.assertRaisesRegex(ValueError, "changed during"):
            self.check()
        self.assertFalse(any("try-restart" in args for args in self.calls))

    def test_only_local_health_readback_is_allowed(self):
        self.assertEqual(sync.health_url("0.0.0.0:6736"), "http://127.0.0.1:6736/health")
        self.assertEqual(sync.health_url("[::]:6736"), "http://[::1]:6736/health")
        for bind in ["example.com:6736", "192.0.2.5:6736", "127.0.0.1:0"]:
            with self.assertRaises(ValueError):
                sync.health_url(bind)


if __name__ == "__main__":
    unittest.main()
