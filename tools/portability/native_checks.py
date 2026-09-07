#!/usr/bin/env python3
"""Run the native foundation gate and retain the exact commands and output."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import tomllib
import traceback

from native_smoke import isolated_env, smoke
from probe_cancellation import check as check_probe_cancellation
from daemon_lifecycle import check as check_daemon_lifecycle
from diagnostics import check as check_diagnostics
from ide_discovery import check as check_ide_discovery
from local_usage import check as check_local_usage
from provider_storage import check as check_provider_storage
from codex_auth import check as check_codex_auth
from browser_import import check as check_browser_import
from dev_install import check as check_dev_install

ROOT = Path(__file__).resolve().parents[2]


def main() -> int:
    # Redirected Windows consoles default to a legacy code page. Node and Rust
    # logs include Unicode paths and result glyphs; retain them in CI output.
    sys.stdout.reconfigure(encoding="utf-8", errors="backslashreplace")
    sys.stderr.reconfigure(encoding="utf-8", errors="backslashreplace")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--report-dir", type=Path, default=ROOT / "target/native-results")
    parser.add_argument("--smoke-temp-dir", type=Path, help="Optional larger volume for installed binary copies")
    args = parser.parse_args()
    report_dir = args.report_dir.resolve()
    report_dir.mkdir(parents=True, exist_ok=True)
    rustc = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    machine = platform.machine().lower()
    architecture = "aarch64" if machine in {"aarch64", "arm64"} else "x86_64" if machine in {"x86_64", "amd64"} else machine
    if host != args.target or not args.target.startswith(architecture + "-"):
        raise RuntimeError(f"Native execution required: Rust host={host}, Python architecture={machine}, target={args.target}")
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    native_deps = {"rquickjs", "rquickjs-sys", "rusqlite", "libsqlite3-sys", "ring", "rustls", "reqwest", "signal-hook", "getrandom", "process-wrap", "nix", "windows"}
    report = {"target": args.target, "system": platform.platform(), "rustc": rustc,
              "python": sys.version, "node": subprocess.check_output(["node", "--version"], text=True).strip(),
              "runner_image": os.environ.get("ImageVersion", "local"),
              "dependencies": [{"name": p["name"], "version": p["version"]}
                               for p in lock["package"] if p["name"] in native_deps], "checks": []}
    with tempfile.TemporaryDirectory(prefix="usagestat native tests ") as directory:
        env = isolated_env(Path(directory).resolve())
        # Keep Rust's toolchain/cache locations after replacing HOME for tests.
        for name, fallback in {"CARGO_HOME": ".cargo", "RUSTUP_HOME": ".rustup"}.items():
            env[name] = os.environ.get(name, str(Path.home() / fallback))

        def command(name: str, argv: list[str]) -> bool:
            print(f"Running {name}: {argv}", flush=True)
            with (report_dir / f"{name}.log").open("w", encoding="utf-8") as output:
                completed = subprocess.run(argv, cwd=ROOT, env=env, stdout=output,
                                           stderr=subprocess.STDOUT, timeout=1800)
            report["checks"].append({"name": name, "command": argv, "exit_code": completed.returncode})
            print((report_dir / f"{name}.log").read_text(encoding="utf-8", errors="replace"), flush=True)
            return completed.returncode == 0

        try:
            built = command("build", ["cargo", "build", "--locked", "--workspace", "--target", args.target])
            command("rust-tests", ["cargo", "test", "--locked", "--workspace", "--target", args.target])
            node_tests = sorted(ROOT.glob("tests/*.test.cjs")) + sorted(ROOT.glob("crates/ai-usage-daemon/tests/*.test.cjs")) + sorted(ROOT.glob("npm/*.test.cjs"))
            if node_tests:
                # Expand here: Windows subprocess does not expand shell globs.
                command("node-tests", ["node", "--test", *[str(path) for path in node_tests]])
            if (ROOT / "tools/tests/test_build_dev.py").exists():
                command("python-tests", [sys.executable, "-m", "unittest", "discover", "-s", "tools/tests", "-p", "test_build_dev.py"])
            command("provider-inventory", [sys.executable, "tools/portability/provider_inventory.py", "--check"])
            if built:
                target_dir = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
                report["smoke"] = smoke(target_dir / args.target / "debug", temp_dir=args.smoke_temp_dir)
                suffix = ".exe" if os.name == "nt" else ""
                report["probe_cancellation"] = check_probe_cancellation(target_dir / args.target / "debug" / ("usagestat" + suffix))
                report["daemon_lifecycle"] = check_daemon_lifecycle(target_dir / args.target / "debug")
                report["diagnostics"] = check_diagnostics(target_dir / args.target / "debug", args.smoke_temp_dir)
                report["ide_discovery"] = check_ide_discovery(target_dir / args.target / "debug" / ("usagestat" + suffix))
                report["local_usage"] = check_local_usage(target_dir / args.target / "debug" / ("usagestatd" + suffix))
                report["provider_storage"] = check_provider_storage(target_dir / args.target / "debug" / ("usagestat" + suffix))
                report["codex_auth"] = check_codex_auth(target_dir / args.target / "debug" / ("usagestat" + suffix))
                report["browser_import"] = check_browser_import(target_dir / args.target / "debug" / ("usagestat" + suffix))
                report["bar_contract"] = json.loads(subprocess.check_output([
                    "node", str(ROOT / "tools/portability/bar_contract.cjs"),
                    str(target_dir / args.target / "debug" / ("usagestat" + suffix)),
                ], env=env, text=True, encoding="utf-8", timeout=120))
                report["dev_install"] = check_dev_install(target_dir / args.target / "debug", args.target)
                if platform.system() == "Darwin":
                    env["USAGESTAT_TEST_DAEMON_BINARY"] = str(target_dir / args.target / "debug" / "usagestatd")
                    command("launchagent-tests", ["cargo", "test", "--locked", "-p", "usagestat-cli",
                            "--target", args.target, "isolated_native_launchagent_lifecycle", "--", "--ignored", "--nocapture"])
                if os.name == "nt":
                    from windows_service import check as check_windows_service
                    report["windows_service"] = check_windows_service(target_dir / args.target / "debug" / "usagestat-service.exe")
                    env["USAGESTAT_TEST_DAEMON_BINARY"] = str(target_dir / args.target / "debug" / "usagestatd.exe")
                    command("scheduled-task-tests", ["cargo", "test", "--locked", "-p", "usagestat-cli",
                            "--target", args.target, "isolated_native_scheduled_task_lifecycle", "--", "--ignored", "--nocapture"])
            else:
                report["smoke"] = {"status": "blocked", "reason": "native build failed; see build.log"}
        except Exception as error:
            report["error"] = f"{type(error).__name__}: {error}"
            traceback.print_exc()
        finally:
            (report_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return int("error" in report or any(check["exit_code"] for check in report["checks"]))


if __name__ == "__main__":
    raise SystemExit(main())
