#!/usr/bin/env python3
"""Prepare and publish UsageStat alpha Windows feeds from verified release bytes.

GitHub/recipe helpers adapted from the user's windows-deploy toolkit.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import urllib.parse
import urllib.request
import urllib.error
import zipfile
import xml.etree.ElementTree as ET
from xml.sax.saxutils import escape
from native_artifacts import read_checked, unpack_checked, inspect_binary
from release_channel import alpha_version, alpha_order

ARCH = {"x64": "64bit"}
SECRET_NAMES = {"winget": "WINDOWS_WINGET_TOKEN", "scoop": "WINDOWS_SCOOP_TOKEN", "chocolatey": "WINDOWS_CHOCO_API_KEY"}
ROOT = Path(__file__).resolve().parents[3]

def version(tag):
    return alpha_version(tag if tag.startswith("v") else "v" + tag)


def chocolatey_version(upstream):
    numbers = alpha_order(upstream)
    require(all(n <= 2147483647 for n in numbers[:3]) and numbers[3] <= 999999,
            'Version exceeds the supported Chocolatey community version range')
    # Community still uses SemVer 1; fixed-width numbering keeps alpha.10 > alpha.2.
    return upstream.split('-')[0] + f'-alpha{numbers[3]:06d}'


def target():
    t = json.loads((ROOT / 'packaging/windows-alpha.json').read_text())
    require(t['repo'] == 'hashimkarim/usagestat' and t['name'] == 'usagestat-alpha', 'Unexpected alpha application identity')
    require(t['channels'] == {
        'scoop': {'name': 'usagestat-alpha', 'bucket': 'hashimkarim/scoop-bucket', 'branch': 'main'},
        'winget': {'id': 'HashimKarim.UsageStat.Alpha', 'fork': 'hashimkarim/winget-pkgs'},
        'chocolatey': {'id': 'usagestat-alpha', 'source': 'https://push.chocolatey.org/'}},
        'Alpha feed destinations differ from the reviewed channel identities')
    return t


def prepare(directory, tag, channel):
    t = target()
    complete = json.loads(read_checked(directory / 'usagestat-artifacts.json'))
    require(complete['version'] == version(tag), 'Requested version differs from release inputs')
    candidates = [m for m in complete['targets'] if m['target'] == 'x86_64-pc-windows-msvc']
    require(len(candidates) == 1, 'Expected exactly one Windows x64 manifest')
    m = candidates[0]
    require(m['version'] == complete['version'] and m['sourceCommit'] == complete['sourceCommit'] and not m['sourceDirty'], 'Windows source/version differs from the aggregate')
    require(m['os'] == 'win32' and m['arch'] == 'x64' and m['archive']['name'] == 'usagestat-windows-x86_64.zip', 'Wrong native Windows payload')
    require(json.loads(read_checked(directory / 'usagestat-windows-x86_64.manifest.json')) == m, 'Windows manifests disagree')
    with tempfile.TemporaryDirectory(prefix='usagestat-windows-feed-') as temporary:
        extracted = Path(temporary)
        unpack_checked(m, directory / m['archive']['name'], extracted)
        for name in ['usagestat.exe', 'usagestatd.exe', 'usagestat-service.exe']:
            inspect_binary((extracted / name).read_bytes(), m['target'])
    records = [{'arch': 'x64', 'type': 'zip', 'bin': ['usagestat.exe', 'usagestatd.exe'],
        'scope': 'user', 'url': f"https://github.com/{t['repo']}/releases/download/{tag}/{m['archive']['name']}",
        'sha256': m['archive']['sha256']}]
    return recipes(t, tag, records, channel)

class GitHubError(RuntimeError):
    def __init__(self, status):
        self.status = status
        super().__init__(f"GitHub operation failed (HTTP {status or 'unknown'}); check access locally")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def gh_api(endpoint, method="GET", payload=None, token=None):
    env = os.environ.copy()
    if token is not None:
        require(bool(token), "Selected publishing credential is missing")
        env["GH_TOKEN"] = token
    args = ["gh", "api", "--method", method, endpoint]
    if payload is not None:
        args += ["--input", "-"]
    try:
        p = subprocess.run(args, input=json.dumps(payload) if payload is not None else None,
                           capture_output=True, text=True, env=env, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        raise RuntimeError("GitHub operation unavailable/timed out; inspect state before retrying") from None
    if p.returncode:
        # Never echo server/CLI responses: credential errors can contain values.
        code = re.search(r"HTTP (\d{3})", p.stderr)
        raise GitHubError(int(code[1]) if code else None)
    try:
        return json.loads(p.stdout) if p.stdout.strip() else None
    except ValueError:
        raise RuntimeError("Invalid GitHub response (content suppressed)") from None


def yaml(value, indent=0):
    """Small YAML emitter for JSON-compatible recipe objects, quoting all scalars."""
    prefix = " " * indent
    if isinstance(value, dict):
        lines = []
        for k, v in value.items():
            if isinstance(v, (dict, list)):
                lines.append(f"{prefix}{k}:\n{yaml(v, indent + 2)}")
            else:
                lines.append(f"{prefix}{k}: {json.dumps(v, ensure_ascii=True)}")
        return "\n".join(lines)
    if isinstance(value, list):
        return "\n".join(f"{prefix}-\n{yaml(v, indent + 2)}" if isinstance(v, (dict, list))
                         else f"{prefix}- {json.dumps(v)}" for v in value)
    raise ValueError("Expected YAML object/list")


def ps(value):
    return "'" + value.replace("'", "''") + "'"


def recipes(t, tag, records, channel):
    v = version(tag)
    c = t["channels"][channel]
    if channel == "scoop":
        manifest = {"version": v, "description": t["description"], "homepage": t["homepage"],
                    "license": t["license"], "architecture": {
                        ARCH[a["arch"]]: {"url": a["url"], "hash": a["sha256"]} for a in records},
                    "bin": records[0]["bin"],
                    "notes": ["Unsigned backend alpha. Run usagestat --version and usagestat doctor.",
                              "Startup is explicit: usagestat daemon enable. Before upgrade: usagestat daemon disable.",
                              "After upgrade: usagestat daemon relocate, then restore the previous running/autostart state.",
                              "Before uninstall: usagestat daemon unregister. User settings/history are retained."]}
        return {f"bucket/{c['name']}.json": json.dumps(manifest, indent=2) + "\n"}
    if channel == "winget":
        identity = c["id"]
        base = f"manifests/{identity[0].lower()}/{identity.replace('.', '/')}/{v}/{identity}"
        common = {"PackageIdentifier": identity, "PackageVersion": v}
        installers = []
        for a in records:
            item = {"Architecture": a["arch"], "InstallerType": a["type"],
                    "InstallerUrl": a["url"], "InstallerSha256": a["sha256"].upper()}
            if a["type"] == "zip":
                item.update(NestedInstallerType="portable", NestedInstallerFiles=[
                    {"RelativeFilePath": p, "PortableCommandAlias": Path(p).stem} for p in a["bin"]])
                item['ArchiveBinariesDependOnPath'] = True
            if "silent_args" in a:
                item["InstallerSwitches"] = {"Silent": a["silent_args"], "SilentWithProgress": a["silent_args"]}
            if "scope" in a and a['type'] != 'zip':
                item["Scope"] = a["scope"]
            installers.append(item)
        documents = {
            f"{base}.yaml": {**common, "DefaultLocale": "en-US", "ManifestType": "version", "ManifestVersion": "1.10.0"},
            f"{base}.locale.en-US.yaml": {**common, "PackageLocale": "en-US", "Publisher": t["publisher"],
                                         "PackageName": t["name"], "License": t["license"], "ShortDescription": t["description"],
                                         "PackageUrl": t["homepage"], "LicenseUrl": f"{t['homepage']}/blob/{tag}/LICENSE",
                                         "ReleaseNotesUrl": f"{t['homepage']}/releases/tag/{tag}",
                                         "ManifestType": "defaultLocale", "ManifestVersion": "1.10.0"},
            f"{base}.installer.yaml": {**common, "Installers": installers, "ManifestType": "installer", "ManifestVersion": "1.10.0"}}
        schema_types = {'version': 'version', 'defaultLocale': 'defaultLocale', 'installer': 'installer'}
        return {p: '# yaml-language-server: $schema=https://aka.ms/winget-manifest.' +
                schema_types[d['ManifestType']] + '.1.10.0.schema.json\n' + yaml(d) + "\n" for p, d in documents.items()}
    package = c["id"]
    fields = {"id": package, "version": chocolatey_version(v), "title": t["name"], "authors": t["publisher"],
              "description": t["description"], "projectUrl": t["homepage"],
              "packageSourceUrl": f"https://github.com/{t['repo']}", "requireLicenseAcceptance": "false",
              "licenseUrl": f"{t['homepage']}/blob/{tag}/LICENSE", "releaseNotes": f"{t['homepage']}/releases/tag/{tag}",
              "tags": "usagestat ai cli usage alpha"}
    nuspec = '<?xml version="1.0"?>\n<package xmlns="http://schemas.microsoft.com/packaging/2015/06/nuspec.xsd">\n<metadata>\n'
    nuspec += "\n".join(f"  <{k}>{escape(val)}</{k}>" for k, val in fields.items())
    nuspec += '\n</metadata>\n<files><file src="tools\\**" target="tools" /></files>\n</package>\n'
    install = ["$ErrorActionPreference = 'Stop'", "$toolsDir = Split-Path -Parent $MyInvocation.MyCommand.Definition"]
    if len(records) == 1 and records[0]["arch"] == "x64":
        install += ["if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64' -and $env:PROCESSOR_ARCHITEW6432 -ne 'AMD64') { throw 'This alpha package requires x64 Windows' }"]
    for i, a in enumerate(sorted(records, key=lambda x: x["arch"])):
        if len(records) > 1:
            condition = "(Get-OSArchitectureWidth) -eq 64 -and $env:ChocolateyForceX86 -ne 'true'"
            install.append((f"if ({condition}) {{" if a["arch"] == "x64" else "else {") if i else f"if ({condition}) {{")
        values = {"packageName": package, "url": a["url"], "checksum": a["sha256"], "checksumType": "sha256"}
        if a["type"] == "zip":
            cmd = "Install-ChocolateyZipPackage"
            values["unzipLocation"] = None
        else:
            cmd = "Install-ChocolateyPackage"
            values.update(fileType="msi" if a["type"] == "msi" else "exe", silentArgs=a.get("silent_args", {
                "msi": "/qn /norestart", "inno": "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-", "nullsoft": "/S"}.get(a["type"], "")))
        install.append("$packageArgs = @{")
        install.extend(f"  {k} = {ps(val) if val is not None else '$toolsDir'}" for k, val in values.items())
        if a["type"] != "zip":
            install.append("  validExitCodes = @(0, 1641, 3010)")
        install += ["}", f"{cmd} @packageArgs"]
        if len(records) > 1:
            install.append("}")
    install += ["# Keep the GUI service supervisor beside the daemon without creating a CLI shim.",
                "New-Item -ItemType File -Path (Join-Path $toolsDir 'usagestat-service.exe.ignore') -Force | Out-Null"]
    return {f"{package}.nuspec": nuspec, "tools/chocolateyinstall.ps1": "\n".join(install) + "\n",
            "tools/VERIFICATION.txt": f"Downloads are the upstream {tag} Windows x64 ZIP, verified with SHA-256.\n" +
                '\n'.join(a['url'] + '\nSHA256: ' + a['sha256'] for a in records) + '\n',
            "tools/LICENSE.txt": (ROOT / 'LICENSE').read_text()}


def write_files(directory, files):
    for path in files:
        require(not (directory / path).exists(), "Generated file exists; use a fresh output directory")
    for path, content in files.items():
        dest = directory / path
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(content, encoding="utf-8")


def chocolatey_preflight(package, files, tag):
    """Check the actual tested nupkg and refuse a duplicate community upload."""
    expected_version = chocolatey_version(version(tag))
    require(package.name == f'usagestat-alpha.{expected_version}.nupkg', 'Unexpected Chocolatey package filename')
    with zipfile.ZipFile(package) as archive:
        names = archive.namelist()
        require(len(names) == len(set(names)), 'Duplicate Chocolatey archive entries')
        for name, content in files.items():
            require(name in names, 'Chocolatey package is missing a reviewed file')
            if name.endswith('.nuspec'):
                metadata = ET.fromstring(archive.read(name))
                fields = {e.tag.rsplit('}', 1)[-1]: e.text for e in metadata.iter()}
                require(fields.get('id') == 'usagestat-alpha' and fields.get('version') == expected_version,
                        'Chocolatey package identity differs from the reviewed recipe')
            else:
                require(archive.read(name).decode('utf-8-sig').replace('\r\n', '\n') == content,
                        'Chocolatey package content changed after preparation')
        for name in names:
            require(name in files or name in ('[Content_Types].xml', '_rels/.rels') or
                    re.fullmatch(r'package/services/metadata/core-properties/[A-Za-z0-9]+\.psmdcp', name),
                    'Unexpected file in Chocolatey package')
    url = "https://community.chocolatey.org/api/v2/Packages(Id='usagestat-alpha',Version='" + expected_version + "')"
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            response.read(1)
    except urllib.error.HTTPError as error:
        require(error.code == 404, f'Chocolatey availability check failed with HTTP {error.code}')
    else:
        raise ValueError('Chocolatey version already exists; inspect publication/moderation before retrying')


def publish_scoop(t, files, token):
    c = t["channels"]["scoop"]
    path, content = next(iter(files.items()))
    repo = c["bucket"]
    metadata = gh_api(f"repos/{repo}", token=token)
    require(not metadata.get("private", True) and metadata.get("permissions", {}).get("push"), "Scoop bucket must be public and writable")
    branch = urllib.parse.quote(c["branch"], safe="")
    # Tree lookup distinguishes an absent file from permission/API failures.
    tree = gh_api(f"repos/{repo}/git/trees/{branch}?recursive=1", token=token)
    require(not tree.get("truncated"), "Bucket tree is truncated; inspect destination manually")
    found = next((f for f in tree["tree"] if f["path"] == path), None)
    encoded = base64.b64encode(content.encode()).decode()
    if found:
        old = gh_api(f"repos/{repo}/contents/{path}?ref={branch}", token=token)
        old_content = base64.b64decode(old["content"]).decode()
        if old_content == content:
            return {"state": "unchanged", "destination": repo}
        previous = json.loads(old_content)
        upcoming = json.loads(content)
        require(alpha_order(version(previous["version"])) < alpha_order(version(upcoming["version"])),
                "Scoop update would downgrade or replace an existing version; reconcile deliberately")
    payload = {"message": f"Update {c['name']} Windows package", "content": encoded, "branch": c["branch"]}
    if found:
        payload["sha"] = found["sha"]
    gh_api(f"repos/{repo}/contents/{path}", "PUT", payload, token)
    return {"state": "published", "destination": repo}


def publish_winget(t, tag, files, token):
    c = t["channels"]["winget"]
    fork = c["fork"]
    metadata = gh_api(f"repos/{fork}", token=token)
    require(metadata.get("fork") and metadata.get("parent", {}).get("full_name") == "microsoft/winget-pkgs"
            and metadata.get("permissions", {}).get("push"), "WinGet destination must be a writable fork of microsoft/winget-pkgs")
    owner = fork.split("/")[0]
    branch = f"windows-deploy/{c['id']}/{version(tag)}"
    upstream = "repos/microsoft/winget-pkgs"
    directory = next(iter(files)).rsplit('/', 1)[0]
    try:
        gh_api(f"{upstream}/contents/{directory}", token=token)
    except GitHubError as exc:
        if exc.status != 404:
            raise
    else:
        raise ValueError("This WinGet version already exists upstream; do not resubmit or replace it")
    prs = gh_api(f"{upstream}/pulls?state=open&head={owner}:{branch}", token=token)
    require(not prs, "A WinGet submission already exists for this version; review it before retrying")
    base = gh_api(f"{upstream}/git/ref/heads/master", token=token)["object"]["sha"]
    base_tree = gh_api(f"{upstream}/git/commits/{base}", token=token)["tree"]["sha"]
    tree = gh_api(f"repos/{fork}/git/trees", "POST", {"base_tree": base_tree, "tree": [
        {"path": p, "mode": "100644", "type": "blob", "content": content} for p, content in files.items()]}, token)
    commit = gh_api(f"repos/{fork}/git/commits", "POST", {"message": f"New version: {c['id']} {version(tag)}", "tree": tree["sha"], "parents": [base]}, token)
    # Creating a ref fails on collisions; never force or overwrite an existing submission branch.
    gh_api(f"repos/{fork}/git/refs", "POST", {"ref": f"refs/heads/{branch}", "sha": commit["sha"]}, token)
    pr = gh_api(f"{upstream}/pulls", "POST", {"title": f"New version: {c['id']} {version(tag)}",
                "head": f"{owner}:{branch}", "base": "master",
                "body": f"Release: https://github.com/{t['repo']}/releases/tag/{tag}\n\nGenerated from exact release assets and SHA-256 checksums."}, token)
    return {"state": "submitted", "url": pr["html_url"], "availability": "pending upstream review"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--channel', choices=SECRET_NAMES, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--publish', action='store_true')
    parser.add_argument('--check-chocolatey-package', type=Path)
    args = parser.parse_args()
    files = prepare(args.artifacts, args.tag, args.channel)
    if args.check_chocolatey_package:
        require(args.channel == 'chocolatey' and not args.publish, 'Package preflight is only for Chocolatey')
        from release_channel import check
        check(args.tag, 'alpha')
        chocolatey_preflight(args.check_chocolatey_package, files, args.tag)
        result = {'state': 'checked', 'package': args.check_chocolatey_package.name}
    elif args.publish:
        from release_channel import check
        check(args.tag, 'alpha')
        # Re-render from the verified release and compare all staged recipes.
        actual = {str(p.relative_to(args.output)).replace('\\', '/'): p.read_text(encoding='utf-8')
                  for p in args.output.rglob('*') if p.is_file()}
        require(actual == files, 'Staged Windows recipes changed after validation')
        require(args.channel != 'chocolatey', 'Chocolatey upload runs after choco pack and native installation tests')
        token = os.environ.get(SECRET_NAMES[args.channel])
        require(token, 'Selected Windows publishing credential is missing')
        result = publish_scoop(target(), files, token) if args.channel == 'scoop' else publish_winget(target(), args.tag, files, token)
    else:
        write_files(args.output, files)
        result = {'state': 'prepared', 'channel': args.channel, 'upstreamVersion': version(args.tag),
                  'packageVersion': chocolatey_version(version(args.tag)) if args.channel == 'chocolatey' else version(args.tag)}
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
