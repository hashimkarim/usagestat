#!/usr/bin/env python3
"""Stage deterministic native archives and manifests without publishing."""
from __future__ import annotations
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import posixpath
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[3]
TARGETS = {
    "x86_64-unknown-linux-gnu": dict(os="linux", arch="x64", asset="usagestat-linux-x86_64", minimum="glibc 2.39", libc="glibc"),
    "aarch64-unknown-linux-gnu": dict(os="linux", arch="arm64", asset="usagestat-linux-aarch64", minimum="glibc 2.39", libc="glibc"),
    "aarch64-apple-darwin": dict(os="darwin", arch="arm64", asset="usagestat-macos-aarch64", minimum="11.0", libc=None),
    "x86_64-apple-darwin": dict(os="darwin", arch="x64", asset="usagestat-macos-x86_64", minimum="11.0", libc=None),
    "x86_64-pc-windows-msvc": dict(os="win32", arch="x64", asset="usagestat-windows-x86_64", minimum="Windows 10 / Server 2016", libc=None),
}

def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()

def version(root=ROOT) -> str:
    return tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]

def binary_names(target: str) -> list[str]:
    names = ["usagestat", "usagestatd"]
    return [name + ".exe" for name in names + ["usagestat-service"]] if TARGETS[target]["os"] == "win32" else names

def native_target(target: str) -> None:
    expected = TARGETS[target]
    host = {"Linux": "linux", "Darwin": "darwin", "Windows": "win32"}[platform.system()]
    arch = {"AMD64": "x64", "x86_64": "x64", "arm64": "arm64", "aarch64": "arm64"}.get(platform.machine())
    if (host, arch) != (expected["os"], expected["arch"]):
        raise ValueError("Artifact execution must run on its native OS and architecture")

def inspect_binary(data: bytes, target: str) -> dict:
    expected = TARGETS[target]
    if expected["os"] == "linux":
        machine = 62 if expected["arch"] == "x64" else 183
        if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01" or int.from_bytes(data[18:20], "little") != machine:
            raise ValueError("ELF architecture does not match the target")
        return {"format": "elf", "machine": machine}
    if expected["os"] == "darwin":
        machine = 0x01000007 if expected["arch"] == "x64" else 0x0100000c
        if len(data) < 32 or struct.unpack_from("<II", data) != (0xfeedfacf, machine):
            raise ValueError("Mach-O architecture does not match the target")
        position = 32
        minimums, libraries = [], []
        for _ in range(struct.unpack_from("<I", data, 16)[0]):
            command, size = struct.unpack_from("<II", data, position)
            if size < 8 or position + size > len(data):
                raise ValueError("Invalid Mach-O load command")
            if command in (0x24, 0x32):
                encoded = struct.unpack_from("<I", data, position + (12 if command == 0x32 else 8))[0]
                minimums.append((encoded >> 16, (encoded >> 8) & 255, encoded & 255))
            if command & 0x7fffffff in (0xc, 0x18, 0x1f, 0x23):
                offset = struct.unpack_from("<I", data, position + 8)[0]
                if offset < 8 or offset >= size:
                    raise ValueError("Invalid Mach-O library command")
                libraries.append(data[position + offset:position + size].split(b"\0", 1)[0].decode())
            position += size
        if not minimums or max(minimums) > (11, 0, 0):
            raise ValueError("Mach-O deployment target exceeds the candidate macOS 11 floor")
        if any(not library.startswith(("/usr/lib/", "/System/Library/")) for library in libraries):
            raise ValueError("Archive has an unbundled non-system macOS library")
        return {"format": "mach-o", "machine": machine, "deploymentTarget": ".".join(map(str, max(minimums))), "libraries": sorted(libraries)}
    if len(data) < 64 or data[:2] != b"MZ":
        raise ValueError("Missing Windows PE header")
    offset = struct.unpack_from("<I", data, 60)[0]
    if data[offset:offset + 4] != b"PE\0\0" or struct.unpack_from("<H", data, offset + 4)[0] != 0x8664:
        raise ValueError("PE architecture does not match Windows x64")
    section_count = struct.unpack_from("<H", data, offset + 6)[0]
    optional_size = struct.unpack_from("<H", data, offset + 20)[0]
    optional = offset + 24
    if struct.unpack_from("<H", data, optional)[0] != 0x20b:
        raise ValueError("Expected a PE32+ executable")
    sections = []
    for index in range(section_count):
        start = optional + optional_size + index * 40
        virtual_size, virtual, raw_size, raw = struct.unpack_from("<IIII", data, start + 8)
        sections.append((virtual, max(virtual_size, raw_size), raw, raw_size))
    def file_offset(rva):
        for virtual, size, raw, raw_size in sections:
            if virtual <= rva < virtual + size and rva - virtual < raw_size:
                return raw + rva - virtual
        raise ValueError("PE import points outside file-backed sections")
    imports = []
    import_rva, import_size = struct.unpack_from("<II", data, optional + 120)
    if import_rva:
        position = file_offset(import_rva)
        for _ in range(min(import_size // 20 + 1, 4096)):
            descriptor = struct.unpack_from("<IIIII", data, position)
            if not any(descriptor):
                break
            name_at = file_offset(descriptor[3])
            imports.append(data[name_at:name_at + 512].split(b"\0", 1)[0].decode("ascii").lower())
            position += 20
        else:
            raise ValueError("Unterminated PE import table")
    if any(name.startswith(("vcruntime", "msvcp", "concrt")) for name in imports):
        raise ValueError("Portable Windows binaries require a statically linked MSVC runtime")
    return {"format": "pe", "machine": 0x8664, "subsystem": struct.unpack_from("<H", data, optional + 68)[0], "libraries": sorted(imports)}

def resource_files(root=ROOT) -> dict[str, bytes]:
    names = subprocess.check_output(["git", "ls-files", "-z", "--", "plugins", "LICENSE"], cwd=root).decode().split("\0")
    names = runtime_resource_names(root, list(filter(None, names)))
    result = {}
    for name in filter(None, names):
        path = root / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"Resource must be a regular tracked file: {name}")
        result[name] = path.read_bytes()
    if "LICENSE" not in result or not any(name.endswith("/plugin.json") for name in result):
        raise ValueError("Missing tracked license/provider resources")
    # Existing Linux consumers already install LICENSE. Keep that layout while
    # carrying notices for the native/static dependencies in the same payload.
    result["LICENSE"] += third_party_notices(root)
    return result

def runtime_resource_names(root: Path, tracked: list[str]) -> list[str]:
    """Only manifests, their entry scripts, icons and licenses enter a release."""
    names = set(tracked)
    provider_files = {path.relative_to(root).as_posix() for path in (root / 'plugins').glob('*/plugin.json')}
    notice = 'plugins/UPSTREAM-LICENSES.md'
    if (root / notice).exists():
        provider_files.add(notice)
    missing = sorted(provider_files - names)
    if missing:
        raise ValueError('Commit provider resources before packaging: ' + ', '.join(missing))
    selected = names & {'LICENSE', notice}
    for name in sorted(names):
        path = PurePosixPath(name)
        if len(path.parts) != 3 or path.name != "plugin.json":
            continue
        manifest = json.loads((root / name).read_text(encoding="utf-8"))
        entry = manifest.get("entry")
        if not isinstance(entry, str) or not entry or entry.startswith('/') or "\\" in entry or ":" in entry:
            raise ValueError(f"Unsafe provider entry resource: {name}")
        # Alias providers intentionally share a sibling provider's entry file.
        # Normalize that reference while keeping it within tracked plugins/.
        script = posixpath.normpath(str(path.parent / entry))
        if not script.startswith('plugins/') or script not in names or script.endswith((".test.js", ".spec.js")):
            raise ValueError(f"Missing runtime provider entry resource: {name}")
        selected.update([name, script])
        selected.update(item for item in names if PurePosixPath(item).parent == path.parent
            and (PurePosixPath(item).suffix.lower() in (".svg", ".png", ".jpg", ".webp") or PurePosixPath(item).name in ("LICENSE", "NOTICE")))
    return sorted(selected)

def pinned_license_notices(root: Path, package: dict) -> list[str]:
    """Crate archives sometimes omit licenses; use reviewed exact-version copies."""
    directory = root / "tools/publish/licenses"
    entries = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
    key = f"{package['name']}-{package['version']}"
    entry = entries.get(key)
    if not entry or entry["license"] != package.get("license") or entry["repository"] != package.get("repository", "").removesuffix(".git"):
        raise ValueError(f"Missing reviewed dependency license text: {key}")
    if not entry.get("files"):
        raise ValueError(f"Empty dependency license entry: {key}")
    sections = []
    for item in entry["files"]:
        relative = PurePosixPath(item["path"])
        if relative.is_absolute() or ".." in relative.parts or "\\" in item["path"] or ":" in item["path"]:
            raise ValueError("Unsafe dependency license path")
        path = directory / relative
        if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(directory.resolve()):
            raise ValueError("Dependency license must be a regular contained file")
        data = path.read_bytes()
        if digest(data) != item["sha256"]:
            raise ValueError(f"Dependency license digest mismatch: {key}")
        sections.append(f"\n[Bundled upstream notice: {item['path']}]\n{item['source']}\n" + data.decode("utf-8") + "\n")
    return sections


def third_party_notices(root=ROOT) -> bytes:
    resolved = {}
    for target in TARGETS:
        metadata = json.loads(subprocess.check_output([
            "cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target
        ], cwd=root))
        ids = {node["id"] for node in metadata["resolve"]["nodes"]}
        resolved.update({package["id"]: package for package in metadata["packages"]
                         if package["id"] in ids and package.get("source")})
    quickjs = next(package for package in resolved.values() if package["name"] == "rquickjs")
    shared_quickjs_license = Path(quickjs["manifest_path"]).parent / "LICENSE"
    sections = ["\n\nTHIRD-PARTY NOTICES\nLocked dependency union for all initial native targets.\n"]
    for package in sorted(resolved.values(), key=lambda p: (p["name"], p["version"], p["id"])):
        directory = Path(package["manifest_path"]).parent
        licenses = {path for path in directory.rglob("*") if path.is_file() and
                    path.name.lower().startswith(("license", "licence", "copying", "copyright", "notice"))}
        if package.get("license_file"):
            licenses.add(directory / package["license_file"])
        if package["name"] in ("rquickjs-core", "rquickjs-sys"):
            licenses.add(shared_quickjs_license)
        supplemental = pinned_license_notices(root, package) if not licenses else []
        sections.append(f"\n--- {package['name']} {package['version']} ({package.get('license') or 'see license text'}) ---\n")
        if package.get("repository"):
            sections.append(package["repository"] + "\n")
        sections.extend(supplemental)
        for path in sorted(licenses, key=lambda p: (p.name, p.relative_to(directory).as_posix() if p.is_relative_to(directory) else "rquickjs/LICENSE")):
            label = path.relative_to(directory).as_posix() if path.is_relative_to(directory) else "rquickjs workspace LICENSE"
            sections.append(f"\n[{label}]\n" + path.read_text(encoding="utf-8") + "\n")
    # libsqlite3-sys's bundled amalgamation carries this dedication in its header.
    sqlite = next(package for package in resolved.values() if package["name"] == "libsqlite3-sys")
    amalgamation = Path(sqlite["manifest_path"]).parent / "sqlite3/sqlite3.c"
    header = re.search(r"/\*\n\*\* 2001 September 15\n.*?\*/", amalgamation.read_text(encoding="utf-8")[:8192], re.S)
    if not header or "author disclaims copyright" not in header.group():
        raise ValueError("Bundled SQLite dedication changed; inspect its license header")
    sections.append("\n--- Bundled SQLite dedication ---\n" + header.group() + "\n")
    return "".join(sections).encode("utf-8")

def archive_bytes(files: dict[str, bytes], executables: list[str], windows: bool) -> bytes:
    raw = io.BytesIO()
    if windows:
        with zipfile.ZipFile(raw, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for name, data in sorted(files.items()):
                info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = (0o100755 if name in executables else 0o100644) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, data, compresslevel=9)
    else:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for name, data in sorted(files.items()):
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    info.mode = 0o755 if name in executables else 0o644
                    archive.addfile(info, io.BytesIO(data))
    return raw.getvalue()

def stage(target: str, binary_dir: Path, output: Path, root=ROOT) -> Path:
    native_target(target)
    metadata = TARGETS[target]
    package_version = version(root)
    files = resource_files(root)
    executables = binary_names(target)
    binaries = {}
    for name in executables:
        path = binary_dir / name
        data = path.read_bytes()
        details = inspect_binary(data, target)
        actual = subprocess.check_output([str(path), "--version"], text=True, encoding="utf-8", timeout=10).strip()
        if actual.rsplit(" ", 1)[-1] != package_version:
            raise ValueError(f"Binary version mismatch: {name}")
        if name == "usagestat-service.exe" and details["subsystem"] != 2:
            raise ValueError("Windows service launcher must use the GUI subsystem")
        if metadata["os"] == "linux":
            versions = subprocess.check_output(["readelf", "--version-info", str(path)], text=True)
            glibc = sorted({tuple(map(int, value.split("."))) for value in re.findall(r"GLIBC_(\d+\.\d+(?:\.\d+)?)", versions)})
            if glibc and glibc[-1] > (2, 39):
                raise ValueError(f"Binary exceeds retained Linux glibc floor: {name}")
            details["requiredGlibc"] = ".".join(map(str, glibc[-1])) if glibc else None
        files[name] = data
        binaries[name] = dict(version=package_version, **details)
    extension = ".zip" if metadata["os"] == "win32" else ".tar.gz"
    archive_name = metadata["asset"] + extension
    archive = archive_bytes(files, executables, metadata["os"] == "win32")
    manifest = {
        "schemaVersion": 1, "package": "usagestat", "version": package_version,
        "sourceCommit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "sourceDirty": bool(subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=root, text=True).strip()),
        "target": target, "os": metadata["os"], "arch": metadata["arch"], "libc": metadata["libc"],
        "minimumSystem": metadata["minimum"], "minimumSystemQualified": metadata["os"] == "linux",
        "buildSystem": platform.platform(), "toolchain": subprocess.check_output(["rustc", "-vV"], text=True).strip(),
        "signing": "unsigned", "archive": {"name": archive_name, "sha256": digest(archive), "size": len(archive)},
        "executables": binaries,
        "files": [{"path": name, "sha256": digest(data), "size": len(data), "executable": name in executables} for name, data in sorted(files.items())],
        "resourcesSha256": digest(json.dumps({name: digest(data) for name, data in sorted(files.items()) if name not in executables}, sort_keys=True).encode()),
        "dependencies": [{"name": package["name"], "version": package["version"]}
                         for package in tomllib.loads((root / "Cargo.lock").read_text())["package"]
                         if package["name"] in {"rquickjs", "rquickjs-sys", "rusqlite", "libsqlite3-sys", "ring", "rustls", "reqwest", "process-wrap", "windows"}],
    }
    output.mkdir(parents=True, exist_ok=True)
    artifacts = {archive_name: archive, archive_name + ".sha256": f"{digest(archive)}  {archive_name}\n".encode()}
    # Preserve existing Linux standalone CLI assets and every legacy basename.
    if metadata["os"] == "linux":
        name = metadata["asset"]
        artifacts[name] = files["usagestat"]
        artifacts[name + ".sha256"] = f"{digest(artifacts[name])}  {name}\n".encode()
    manifest_name = metadata["asset"] + ".manifest.json"
    artifacts[manifest_name] = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    artifacts[manifest_name + ".sha256"] = f"{digest(artifacts[manifest_name])}  {manifest_name}\n".encode()
    for name, data in artifacts.items():
        path = output / name
        if path.exists() and path.read_bytes() != data:
            raise ValueError(f"Refusing to overwrite a different staged artifact: {name}")
    for name, data in artifacts.items():
        path = output / name
        path.write_bytes(data)
        if metadata["os"] == "linux" and name == metadata["asset"]:
            path.chmod(0o755)
    return output / manifest_name

def read_checked(path: Path) -> bytes:
    data = path.read_bytes()
    if path.with_name(path.name + ".sha256").read_text().split() != [digest(data), path.name]:
        raise ValueError(f"Checksum mismatch: {path.name}")
    return data

def unpack_checked(manifest: dict, archive_path: Path, destination: Path) -> None:
    archive_data = read_checked(archive_path)
    if (digest(archive_data), len(archive_data)) != (manifest["archive"]["sha256"], manifest["archive"]["size"]):
        raise ValueError("Archive differs from its versioned manifest")
    expected = {item["path"]: item for item in manifest["files"]}
    if len(expected) != len(manifest["files"]):
        raise ValueError("Duplicate manifest paths")
    seen = set()
    def write(name, data, mode):
        path = PurePosixPath(name)
        if (path.is_absolute() or not path.parts or any(p in ("", ".", "..") for p in name.split("/"))
                or "\\" in name or ":" in name or name.casefold() in seen or name not in expected):
            raise ValueError(f"Unsafe or unexpected archive path: {name}")
        item = expected[name]
        if (len(data), digest(data)) != (item["size"], item["sha256"]) or bool(mode & 0o111) != item["executable"]:
            raise ValueError(f"Archive payload differs from manifest: {name}")
        seen.add(name.casefold())
        output = destination.joinpath(*path.parts)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(data)
        output.chmod(0o755 if item["executable"] else 0o644)
    if archive_path.suffix == ".zip":
        with zipfile.ZipFile(io.BytesIO(archive_data)) as archive:
            for member in archive.infolist():
                if member.is_dir() or member.file_size > 256 * 1024 * 1024 or (member.external_attr >> 16) & 0o170000 != 0o100000:
                    raise ValueError("ZIP members must be bounded regular files")
                write(member.filename, archive.read(member), member.external_attr >> 16)
    else:
        with tarfile.open(fileobj=io.BytesIO(archive_data), mode="r:gz") as archive:
            for member in archive:
                if not member.isfile() or member.size > 256 * 1024 * 1024:
                    raise ValueError("Tar members must be bounded regular files")
                write(member.name, archive.extractfile(member).read(), member.mode)
    if len(seen) != len(expected):
        raise ValueError("Archive is missing manifest files")

def verify(manifest_path: Path, *, smoke_temp: Path | None = None) -> dict:
    manifest = json.loads(read_checked(manifest_path))
    if manifest["schemaVersion"] != 1 or manifest["version"] != version():
        raise ValueError("Manifest schema or source version mismatch")
    target = manifest["target"]
    native_target(target)
    metadata = TARGETS[target]
    if (manifest["os"], manifest["arch"], manifest["libc"]) != (metadata["os"], metadata["arch"], metadata["libc"]):
        raise ValueError("Manifest platform metadata disagrees with target")
    archive_name = manifest["archive"]["name"]
    expected_archive = metadata["asset"] + (".zip" if metadata["os"] == "win32" else ".tar.gz")
    if archive_name != expected_archive:
        raise ValueError("Unexpected native archive name")
    sys.path.insert(0, str(ROOT / "tools/portability"))
    from native_smoke import smoke, isolated_env, run
    with tempfile.TemporaryDirectory(prefix="usagestat extracted archive 使用 ", dir=smoke_temp) as directory:
        root = Path(directory)
        extracted = root / "installation"
        extracted.mkdir()
        unpack_checked(manifest, manifest_path.parent / archive_name, extracted)
        for name in binary_names(target):
            data = (extracted / name).read_bytes()
            inspect_binary(data, target)
            actual = subprocess.check_output([str(extracted / name), "--version"], text=True, encoding="utf-8", timeout=10).strip()
            if actual.rsplit(" ", 1)[-1] != manifest["version"]:
                raise ValueError("Extracted binary version mismatch")
        resources = resource_files()
        for name, data in resources.items():
            if (extracted / name).read_bytes() != data:
                raise ValueError(f"Extracted resource differs from source inventory: {name}")
        if set(item["path"] for item in manifest["files"]) != set(resources) | set(binary_names(target)):
            raise ValueError("Archive resource inventory is incomplete")
        env = isolated_env(root / "profile")
        cwd = root / "outside source and installation"
        cwd.mkdir()
        expected_ids = {json.loads(data)["id"] for name, data in resources.items() if name.endswith("/plugin.json")}
        cli = extracted / binary_names(target)[0]
        providers = json.loads(run(cli, ["--json", "list"], cwd, env))
        if {provider["id"] for provider in providers} != expected_ids:
            raise ValueError("Installed archive plugin discovery does not match its inventory")
        for provider in providers:
            icon = (provider.get("icon") or {}).get("path")
            if icon and (not Path(icon).is_absolute() or not Path(icon).is_file()):
                raise ValueError("Extracted provider icon is not accessible")
        # The smoke helper copies these exact binaries into its synthetic-only
        # profile. Bundled real providers are listed above, never credential-probed.
        report = smoke(extracted, temp_dir=smoke_temp)
        report["archiveSha256"] = manifest["archive"]["sha256"]
        report["manifest"] = manifest_path.name
        return report

def merge(directory: Path) -> Path:
    manifests = [json.loads(read_checked(directory / (metadata["asset"] + ".manifest.json"))) for metadata in TARGETS.values()]
    if {manifest["target"] for manifest in manifests} != set(TARGETS):
        raise ValueError("Release is missing or duplicates a native target")
    if any(manifest["sourceDirty"] for manifest in manifests):
        raise ValueError("A release cannot contain binaries from dirty source trees")
    for field in ["version", "sourceCommit", "resourcesSha256", "dependencies"]:
        if any(manifest[field] != manifests[0][field] for manifest in manifests):
            raise ValueError(f"Native archives disagree on {field}")
    for manifest in manifests:
        archive = directory / manifest["archive"]["name"]
        if digest(read_checked(archive)) != manifest["archive"]["sha256"]:
            raise ValueError("Native archive digest mismatch during aggregation")
    result = directory / "usagestat-artifacts.json"
    installer_name = "Install-Usagestat.ps1"
    installer = (ROOT / "tools/install" / installer_name).read_bytes()
    (directory / installer_name).write_bytes(installer)
    (directory / (installer_name + ".sha256")).write_text(f"{digest(installer)}  {installer_name}\n")
    data = (json.dumps({"schemaVersion": 1, "version": manifests[0]["version"], "sourceCommit": manifests[0]["sourceCommit"],
        "targets": manifests, "installers": {"windows": {"name": installer_name, "sha256": digest(installer)}}}, sort_keys=True, indent=2) + "\n").encode()
    result.write_bytes(data)
    result.with_name(result.name + ".sha256").write_text(f"{digest(data)}  {result.name}\n")
    return result

def prepare_publication(directory: Path, output: Path, channel: str) -> Path:
    """Stable Linux remains available while new desktop floors are qualified."""
    complete = json.loads(read_checked(directory / "usagestat-artifacts.json"))
    selected = [manifest for manifest in complete["targets"]
                if channel == "prerelease" or manifest["minimumSystemQualified"]]
    if not selected:
        raise ValueError("No qualified artifacts for this publication channel")
    output.mkdir(parents=True, exist_ok=False)
    for manifest in selected:
        prefix = TARGETS[manifest["target"]]["asset"]
        names = [manifest["archive"]["name"], prefix + ".manifest.json"]
        if manifest["os"] == "linux":
            names.append(prefix)
        for name in names:
            read_checked(directory / name)
            for path in [directory / name, directory / (name + ".sha256")]:
                shutil.copyfile(path, output / path.name)
    if any(manifest["os"] == "win32" for manifest in selected):
        installer = complete["installers"]["windows"]
        name = installer["name"]
        if name != "Install-Usagestat.ps1" or digest(read_checked(directory / name)) != installer["sha256"]:
            raise ValueError("Windows installer differs from its aggregate manifest")
        if (directory / name).read_bytes() != (ROOT / "tools/install" / name).read_bytes():
            raise ValueError("Windows installer differs from the release source")
        for filename in [name, name + ".sha256"]:
            shutil.copyfile(directory / filename, output / filename)
    else:
        complete.pop("installers", None)
    complete["targets"] = selected
    data = (json.dumps(complete, sort_keys=True, indent=2) + "\n").encode()
    result = output / "usagestat-artifacts.json"
    result.write_bytes(data)
    result.with_name(result.name + ".sha256").write_text(f"{digest(data)}  {result.name}\n")
    return result

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--binary-dir", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--verify", type=Path, metavar="MANIFEST")
    parser.add_argument("--merge", type=Path, metavar="ASSET_DIRECTORY")
    parser.add_argument("--prepare-publication", type=Path, metavar="ASSET_DIRECTORY")
    parser.add_argument("--channel", choices=["stable", "prerelease"], default="stable")
    parser.add_argument("--smoke-temp-dir", type=Path)
    args = parser.parse_args()
    if args.verify:
        print(json.dumps(verify(args.verify.resolve(), smoke_temp=args.smoke_temp_dir), indent=2))
    elif args.merge:
        print(merge(args.merge.resolve()))
    elif args.prepare_publication and args.output:
        print(prepare_publication(args.prepare_publication.resolve(), args.output.resolve(), args.channel))
    elif args.target and args.binary_dir and args.output:
        print(stage(args.target, args.binary_dir.resolve(), args.output.resolve()))
    else:
        parser.error("provide --target/--binary-dir/--output, --verify, or --merge")

if __name__ == "__main__":
    main()
