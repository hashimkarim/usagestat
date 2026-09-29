#!/usr/bin/env python3
"""Queue RPM-owned readback checks in existing user managers after a transaction."""
import json
import os
import re
import subprocess

UNIT = "usagestat-package-sync.service"
UNIT_PATH = "/usr/lib/systemd/user/" + UNIT


def run(args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=10).stdout


def queue_checks(runner=run):
    rows = json.loads(runner(["systemctl", "list-units", "user@*.service", "--state=running", "--output=json", "--no-legend"]))
    users = sorted({int(match[1]) for row in rows if (match := re.fullmatch(r"user@([0-9]+)\.service", row.get("unit", ""))) and int(match[1]) > 0})
    queued = []
    for uid in users:
        command = ["systemctl", "--user", "--machine=" + str(uid) + "@.host"]
        try:
            runner(command + ["daemon-reload"])
            # Preserve user overrides of the package helper as well as unrelated
            # service managers. The invoked helper runs as that user, never root.
            fragment = runner(command + ["show", UNIT, "--property=FragmentPath", "--value"]).strip()
            if fragment != UNIT_PATH:
                continue
            runner(command + ["start", "--no-block", UNIT])
            queued.append(uid)
        except (OSError, ValueError, subprocess.SubprocessError):
            continue
    return queued


def main():
    if os.geteuid() != 0:
        return 0
    try:
        queue_checks()
    except (OSError, ValueError, subprocess.SubprocessError):
        # CLI-only/container installations have no live systemd manager. Package
        # installation still succeeds; a later daemon start uses the new binary.
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
