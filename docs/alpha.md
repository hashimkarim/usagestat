# Try the v2 backend alpha

[v2.0.0-alpha.1 downloads](https://github.com/hashimkarim/usagestat/releases/tag/v2.0.0-alpha.1)
contain the CLI, daemon, local web dashboard/API and provider plugins for Windows
x64, macOS Intel/Apple Silicon and Linux x64/ARM64. This is an early backend
release; the Windows/macOS bar frontends are not included. Choose alpha for testing;
existing stable Linux packages remain available separately.

Windows/macOS binaries are unsigned and the Mac binaries are not notarized.
Use the exact repository release and matching checksums. Signing, normal desktop
login/reboot and real provider authentication still need qualification; see the
[release notes](releases/v2.0.0-alpha.1.md). Native CI runs on macOS 15 and Windows
Server 2025. Older candidate OS floors remain unverified. Linux requires glibc
2.39+; Windows ARM64 and musl/Alpine have no alpha payload.

## Windows: portable first run

Download `usagestat-windows-x86_64.zip` and its `.sha256` sidecar into the same
directory. In Windows PowerShell, from that directory:

```powershell
$archive = 'usagestat-windows-x86_64.zip'
$checksum = (Get-Content -Raw ($archive + '.sha256')).Trim() -split '\s+'
if ($checksum.Count -ne 2 -or $checksum[1] -cne $archive -or
    $checksum[0] -notmatch '^[0-9a-f]{64}$' -or
    (Get-FileHash $archive -Algorithm SHA256).Hash -ine $checksum[0]) {
    throw 'Archive checksum mismatch.'
}
$destination = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\usagestat-v2-alpha.1'
if (Test-Path -LiteralPath $destination) { throw 'Choose a fresh extraction directory.' }
Expand-Archive -LiteralPath $archive -DestinationPath $destination
& (Join-Path $destination 'usagestat.exe') --version
& (Join-Path $destination 'usagestat.exe') --json list
& (Join-Path $destination 'usagestat.exe') doctor
```

Expect version `2.0.0-alpha.1` and provider manifests with usable icon paths.
These commands do not register startup or probe provider accounts. Keep both
executables, the service supervisor and plugin directory together. PATH is not
changed. A Windows policy prompt is possible for this unsigned alpha.

For managed installation and subsequent replacement/recovery, download these six
assets together: the ZIP and checksum, `usagestat-windows-x86_64.manifest.json`
and checksum, and `Install-Usagestat.ps1` and checksum. Verify the script checksum
as well, then follow [the per-user installer guide](windows-distribution.md#transactional-per-user-candidate-installer).
Use a fresh destination when switching from a manually extracted archive.
The installer supports `-BackendProfile dev` for the separate development identity;
its executable names end in `-dev.exe`.

## macOS and Linux: portable first run

Choose the archive matching your CPU. On macOS, `uname -m` reports `arm64` for
Apple Silicon or `x86_64` for Intel. Linux ARM64 uses the `aarch64` asset. Download
the archive and matching `.sha256` sidecar from the release page. For example,
on Apple Silicon:

```sh
shasum -a 256 -c usagestat-macos-aarch64.tar.gz.sha256
mkdir usagestat-v2-alpha.1
tar -xzf usagestat-macos-aarch64.tar.gz -C usagestat-v2-alpha.1
./usagestat-v2-alpha.1/usagestat --version
./usagestat-v2-alpha.1/usagestat --json list
./usagestat-v2-alpha.1/usagestat doctor
```

For Intel macOS substitute `usagestat-macos-x86_64.tar.gz`. On Linux use the
matching `usagestat-linux-x86_64.tar.gz` or `usagestat-linux-aarch64.tar.gz` and
`sha256sum -c` to verify it. Proceed with extraction only when checksum verification
succeeds, and use a fresh directory. Keep the entire extracted directory together;
invoke the CLI by its full path or add that directory to your shell's PATH.

The unsigned macOS download may be blocked by local security policy. This alpha
does not include a signed/notarized installer; do not disable Gatekeeper or strip
quarantine to treat the download as qualified. A source build from the tagged
checkout is another testing route, with the documented Rust/native build tools.

## Dashboard and providers

Run the extracted `usagestatd` (`usagestatd.exe` on Windows) with
`--bind 127.0.0.1:6737`, then open <http://127.0.0.1:6737/dashboard>. Stop foreground
execution with Ctrl+C. Choose another unused port if needed. For a first alpha
test this avoids the stable daemon's default port 6736.

To keep writable alpha settings/history separate, set `USAGESTAT_CONFIG_DIR` and
`USAGESTAT_DATA_DIR` to dedicated absolute alpha directories in the shell used
for both commands. Those overrides are full application paths. They do not move
provider-owned credentials, browser profiles or IDE data. Use a separate OS test
user for login-service acceptance if another installation already owns that
profile's service identity. A different port alone does not create a new profile.

Provider access is separate from installing the backend. Configure or sign into
the selected provider, then explicitly try `usagestat usage PROVIDER_ID`.
Check the [provider/source matrix](provider-compatibility.md) before choosing an
authentication method; available manifests are not proof of a verified account.

Login startup is optional and explicit: run the matching CLI's
`daemon enable --bind 127.0.0.1:6737`, then `daemon status`. The CLI reports an
existing owner/conflict instead of replacing another installation silently.
Use [Windows](windows-daemon.md), [macOS](macos-daemon.md) or
[shared daemon instructions](daemon-lifecycle.md) for ownership and T3 settings.

## Update or remove

Retain the previous version and a copy of any alpha state you need until a new
build has been verified. Stop and unregister an owned login daemon with that
installation's `daemon unregister` before removing its executable directory.
This retains settings, history and keys. A portable installation can then be
removed by deleting only its extraction directory. The Windows installer instead
uses its documented `Install`, `Recover` and `Uninstall` actions.

Same-build replacement, Homebrew revision relocation and interruption recovery
have native fixtures. Upgrading between distinct released backend versions is
still an alpha test case. Do not overwrite a running Windows executable, assume
an older backend understands new alpha state, or delete provider-owned credentials
as part of removing the backend.

npm remains a planned distribution channel with passing native installation
rehearsals. `@hashimkarim/usagestat` has not yet been published; use these GitHub
downloads for this alpha. AUR/Homebrew/COPR/PPA continue to serve their stable
packages. WinGet/Scoop/Chocolatey, desktop stores and signed desktop bundles are
outside this alpha's published channels.
