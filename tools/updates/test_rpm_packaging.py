"""Build private non-release RPM fixtures to check payload and lifecycle metadata."""
import configparser
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
REPO = "/etc/yum.repos.d/_copr:copr.fedorainfracloud.org:hashimkarim:usagestat.repo"


def rpm_macros_available():
    if not all(shutil.which(command) for command in ["rpmbuild", "rpmspec", "rpm"]):
        return False
    return subprocess.check_output(["rpm", "--eval", "%{_userunitdir}"], text=True).strip() == "/usr/lib/systemd/user"


@unittest.skipUnless(rpm_macros_available(), "Requires Fedora RPM build tools and systemd macros")
class RpmLifecycle(unittest.TestCase):
    def build(self, alpha=False):
        temp = tempfile.TemporaryDirectory(prefix="usagestat-rpm-fixture-")
        self.addCleanup(temp.cleanup)
        top = Path(temp.name)
        source = top / "usagestat-9.8.7"
        (source / "target/release").mkdir(parents=True)
        for binary in ["usagestat", "usagestatd"]:
            (source / "target/release" / binary).write_text("#!/bin/sh\necho 'private packaging fixture, not a release'\n")
        (source / "plugins").mkdir()
        (source / "plugins/fixture.txt").write_text("Packaging fixture only; no provider or model calls.\n")
        shutil.copy2(ROOT / "LICENSE", source / "LICENSE")
        shutil.copytree(ROOT / "tools/updates", source / "tools/updates")
        shutil.copytree(ROOT / "packaging/rpm", source / "packaging/rpm")
        (top / "SOURCES").mkdir()
        with tarfile.open(top / "SOURCES/v9.8.7.tar.gz", "w:gz") as archive:
            archive.add(source, arcname=source.name)
        spec = (ROOT / "packaging/rpm/usagestat.spec").read_text()
        spec = re.sub(r"^Version:.*$", "Version:        9.8.7", spec, flags=re.M)
        # Exercise the real install/files/scriptlets, without rebuilding native
        # binaries or implying this fixture qualifies the application release.
        spec = re.sub(r"%build\n.*?\n%install", "%build\n:\n\n%install", spec, flags=re.S)
        spec = re.sub(r"%check\n.*?\n%post\n", "%check\n:\n\n%post\n", spec, flags=re.S)
        if alpha:
            spec = "%global usagestat_alpha 1\n" + spec
        recipe = top / "fixture.spec"
        recipe.write_text(spec)
        result = subprocess.run(["rpmbuild", "-bb", "--nodeps", "--define", "_topdir " + str(top),
                                 "--define", "debug_package %{nil}", str(recipe)],
                                capture_output=True, text=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        packages = list((top / "RPMS").rglob("usagestat-*.rpm"))
        self.assertEqual(len(packages), 1)
        return packages[0]

    def test_stable_payload_and_post_transaction_are_packaged(self):
        package = self.build()
        files = subprocess.check_output(["rpm", "-qpl", str(package)], text=True).splitlines()
        for path in [REPO, "/usr/libexec/usagestat/service-sync.py", "/usr/libexec/usagestat/rpm-service-sync.py",
                     "/usr/lib/systemd/user/usagestat-package-sync.service",
                     "/usr/lib/systemd/system/usagestat-rpm-update.timer",
                     "/usr/lib/systemd/system/usagestat-rpm-update.service"]:
            self.assertIn(path, files)
        self.assertFalse(any("install-rpm" in path or "install-service" in path for path in files))
        scripts = subprocess.check_output(["rpm", "-qp", "--scripts", str(package)], text=True)
        self.assertIn("posttrans", scripts)
        self.assertIn("/usr/bin/python3 /usr/libexec/usagestat/rpm-service-sync.py", scripts)
        flags = subprocess.check_output(["rpm", "-qp", "--qf", "[%{FILENAMES} %{FILEFLAGS}\n]", str(package)], text=True)
        repo_flags = next(int(line.rsplit(" ", 1)[1]) for line in flags.splitlines() if line.startswith(REPO + " "))
        self.assertEqual(repo_flags & (1 | 16), 1 | 16)  # config and noreplace

    def test_alpha_payload_preserves_its_feed_and_download_policy(self):
        package = self.build(alpha=True)
        files = subprocess.check_output(["rpm", "-qpl", str(package)], text=True).splitlines()
        self.assertNotIn(REPO, files)
        self.assertFalse(any("usagestat-rpm-update" in path for path in files))
        self.assertIn("/usr/lib/systemd/user/usagestat-package-sync.service", files)
        scripts = subprocess.check_output(["rpm", "-qp", "--scripts", str(package)], text=True)
        self.assertIn("rpm-service-sync.py", scripts)
        self.assertNotIn("usagestat-rpm-update.timer", scripts)


class StableFeed(unittest.TestCase):
    def test_daily_downloads_select_stable_feed_with_rpm_signature_checks(self):
        config = configparser.ConfigParser(interpolation=None)
        config.read(ROOT / "packaging/rpm/usagestat-copr.repo")
        sections = config.sections()
        self.assertEqual(sections, ["copr:copr.fedorainfracloud.org:hashimkarim:usagestat"])
        feed = config[sections[0]]
        self.assertEqual(feed.getint("gpgcheck"), 1)
        self.assertTrue(feed["gpgkey"].startswith("https://download.copr.fedorainfracloud.org/results/hashimkarim/usagestat/"))
        self.assertNotIn("alpha", feed["baseurl"])
        service = (ROOT / "tools/updates/usagestat-rpm-update.service").read_text()
        self.assertIn("--from-repo=" + sections[0] + " usagestat\n", service)


if __name__ == "__main__":
    unittest.main()
