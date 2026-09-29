#!/usr/bin/env python3
"""Restart an already-running owned user service after its executable is replaced."""
from __future__ import annotations

import argparse
import ipaddress
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import urllib.request

MARKER = "# Managed by usagestat daemon enable\n"


def run(arguments):
    result = subprocess.run(arguments, capture_output=True, text=True, timeout=25)
    if result.returncode:
        raise RuntimeError("service command failed: " + arguments[0])
    return result.stdout


def health_url(bind):
    host, port = bind.rsplit(":", 1)
    address = ipaddress.ip_address(host.strip("[]"))
    if address.is_unspecified:
        address = ipaddress.ip_address("::1" if address.version == 6 else "127.0.0.1")
    if not address.is_loopback or not 1 <= int(port) <= 65535:
        raise ValueError("service health must use a fixed loopback endpoint")
    host = f"[{address}]" if address.version == 6 else str(address)
    return f"http://{host}:{int(port)}/health"


def read_health(url):
    # The daemon is local; never send its readback through an HTTP proxy.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(url, timeout=2) as response:
        return json.loads(response.read(16384))


def properties(unit, runner):
    raw = runner(["systemctl", "--user", "show", unit, "--property=LoadState,ActiveState,MainPID,FragmentPath"])
    return dict(line.split("=", 1) for line in raw.splitlines() if "=" in line)


def validate_health(body, owner, pid, profile, version=None):
    return (body.get("application") == "usagestat" and body.get("status") == "ok"
            and body.get("owner") == str(owner) and body.get("pid") == pid
            and body.get("profile") == profile
            and (version is None or body.get("version") == version))


def sync(settings_path=None, unit="usagestat.service", *, runner=run, health=read_health,
         proc_root=Path("/proc"), wait=time.sleep, attempts=20, rpm=False):
    if unit not in ("usagestat.service", "usagestat-dev.service"):
        raise ValueError("only the native Usagestat readback service is supported")
    state = properties(unit, runner)
    if state.get("LoadState") != "loaded" or state.get("ActiveState") != "active":
        return {"action": "unchanged", "reason": "service-not-running"}
    unit_text = Path(state["FragmentPath"]).read_text()
    if not unit_text.startswith(MARKER):
        return {"action": "unchanged", "reason": "unmanaged-service"}
    commands = [line.removeprefix("ExecStart=") for line in unit_text.splitlines() if line.startswith("ExecStart=")]
    words = [word.replace("%%", "%").replace("$$", "$") for word in shlex.split(commands[0])] if len(commands) == 1 else []
    if settings_path is None:
        if len(words) != 3 or words[1] != "--service-settings":
            return {"action": "unchanged", "reason": "legacy-registration"}
        settings_path = Path(words[2])
    settings_path = settings_path.resolve(strict=True)
    saved = json.loads(settings_path.read_text())
    installation = saved.get("installation")
    if not installation:
        raise ValueError("service has no saved installation owner")
    binary = Path(installation["binary"])
    owner = Path(installation["owner"])
    if rpm and (binary != Path("/usr/bin/usagestatd") or owner != Path("/usr")):
        return {"action": "unchanged", "reason": "different-package-owner"}
    if not binary.is_absolute() or not owner.is_absolute():
        raise ValueError("installation paths must be absolute")
    installed = binary.resolve(strict=True)
    if not installed.is_relative_to(owner.resolve(strict=True)):
        raise ValueError("installed executable is outside its owner")
    if words != [str(binary), "--service-settings", str(settings_path)]:
        raise ValueError("service registration does not match its saved installation")
    pid = int(state["MainPID"])
    process = proc_root / str(pid)
    if pid <= 0 or process.stat().st_uid != os.getuid():
        raise ValueError("running service is not owned by this user")
    arguments = process.joinpath("cmdline").read_bytes().split(b"\0")
    arguments = [item.decode() for item in arguments if item]
    if arguments != [str(binary), "--service-settings", str(settings_path)]:
        raise ValueError("running process does not match its saved installation")
    running_stat, installed_stat = process.joinpath("exe").stat(), installed.stat()
    if (running_stat.st_dev, running_stat.st_ino) == (installed_stat.st_dev, installed_stat.st_ino):
        return {"action": "unchanged", "reason": "current-executable"}
    url = health_url(installation["bind"])
    profile = "usagestat-dev" if unit == "usagestat-dev.service" else "usagestat"
    if not validate_health(health(url), owner, pid, profile):
        raise ValueError("readback endpoint does not belong to this running service")
    version = runner([str(binary), "--version"]).strip()
    if not version.startswith("usagestatd ") or len(version) > 128:
        raise ValueError("replacement is not a Usagestat daemon")
    version = version.removeprefix("usagestatd ")
    # Recheck immediately before restarting. A concurrent owner/config change
    # must not transfer this action to another installation.
    if json.loads(settings_path.read_text()) != saved or properties(unit, runner) != state:
        raise ValueError("service installation changed during the update check")
    runner(["systemctl", "--user", "try-restart", unit])
    for _ in range(attempts):
        try:
            current = properties(unit, runner)
            new_pid = int(current.get("MainPID", "0"))
            replacement_stat = proc_root.joinpath(str(new_pid), "exe").stat()
            if (new_pid != pid and new_pid > 0
                    and (replacement_stat.st_dev, replacement_stat.st_ino) == (installed_stat.st_dev, installed_stat.st_ino)
                    and validate_health(health(url), owner, new_pid, profile, version)):
                return {"action": "restarted", "version": version}
        except (OSError, ValueError, RuntimeError):
            pass
        wait(0.5)
    raise RuntimeError("updated daemon did not become healthy; inspect the service journal")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--settings", type=Path, help="Defaults to the managed user service's registered settings file")
    parser.add_argument("--unit", default="usagestat.service")
    parser.add_argument("--rpm", action="store_true", help="Preserve installations not owned by the system RPM")
    args = parser.parse_args()
    try:
        print(json.dumps(sync(args.settings, args.unit, rpm=args.rpm)))
    except Exception as error:
        # Configuration can contain credential environment values. Never dump
        # its contents, process environments, HTTP bodies or subprocess output.
        print("Usagestat service sync failed: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
