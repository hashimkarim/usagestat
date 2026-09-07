#Requires -Version 5.1
<#
Install verified local Windows release inputs into a stable, per-user prefix.
No download, elevation, PATH change, credential probe or implicit login startup.
See docs/windows-distribution.md for the candidate trust and recovery contract.
#>
[CmdletBinding()]
param(
    [ValidateSet('Install', 'Uninstall', 'Recover')][string]$Action = 'Install',
    [string]$Manifest,
    [string]$Destination,
    [ValidateSet('stable', 'dev')][string]$BackendProfile = 'stable'
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or
    -not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64' -or
    $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { throw 'This installer requires native Windows x64.' }
Add-Type -AssemblyName System.IO.Compression.FileSystem
$app = if ($BackendProfile -eq 'dev') { 'usagestat-dev' } else { 'usagestat' }
if (-not $Destination) {
    $Destination = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) ('Programs\' + $app)
}
if (-not [IO.Path]::IsPathRooted($Destination) -or $Destination.StartsWith('\\') -or
    $Destination -match '[%"\x00-\x1f;]') { throw 'Choose an absolute local installation directory without percent signs, quotes, semicolons or control characters.' }
$prefix = [IO.Path]::GetFullPath($Destination).TrimEnd('\')
if ($prefix -eq [IO.Path]::GetPathRoot($prefix).TrimEnd('\')) { throw 'An installation cannot own a drive root.' }
$parent = [IO.Path]::GetDirectoryName($prefix)
$leaf = [IO.Path]::GetFileName($prefix)
$markerName = 'usagestat-installation.json'
$journalPath = Join-Path $parent ('.' + $leaf + '.usagestat-transaction.json')
$lockPath = Join-Path $parent ('.' + $leaf + '.usagestat-install.lock')
$utf8 = New-Object Text.UTF8Encoding($false)

function Assert-NoReparse([string]$Path) {
    $cursor = [IO.Path]::GetFullPath($Path)
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            if (((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Preserve the reparse point at $cursor; choose a regular installation directory."
            }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
}
function Read-Json([string]$Path) {
    Assert-NoReparse $Path
    if ((Get-Item -LiteralPath $Path).Length -gt 8MB) { throw 'Metadata exceeds its size limit.' }
    return ([IO.File]::ReadAllText($Path, $utf8) | ConvertFrom-Json)
}
function Write-Json([string]$Path, $Value) {
    $temporary = $Path + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
    try {
        [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 30), $utf8)
        if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary, $Path, $null) }
        else { [IO.File]::Move($temporary, $Path) }
    } finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
}
function Hash-File([string]$Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function Assert-Checksum([string]$Path) {
    Assert-NoReparse $Path
    $sidecar = $Path + '.sha256'
    Assert-NoReparse $sidecar
    if ((Get-Item -LiteralPath $sidecar).Length -gt 1024) { throw 'Invalid checksum sidecar size.' }
    $parts = [IO.File]::ReadAllText($sidecar, $utf8).Trim() -split '\s+'
    if ($parts.Count -ne 2 -or $parts[0] -cnotmatch '^[0-9a-f]{64}$' -or
        $parts[1] -cne [IO.Path]::GetFileName($Path) -or (Hash-File $Path) -cne $parts[0]) {
        throw "Checksum mismatch: $Path"
    }
}
function Assert-Relative([string]$Path) {
    if (-not $Path -or $Path.Length -gt 240 -or $Path -match '[\\:"<>|?*\x00-\x1f]' -or $Path.StartsWith('/')) {
        throw 'Unsafe payload path.'
    }
    foreach ($part in $Path.Split('/')) {
        if (-not $part -or $part -in @('.', '..') -or $part -match '[. ]$' -or
            $part -match '^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\.|$)') { throw 'Unsafe Windows payload component.' }
    }
}
function Payload-Path([string]$Path) {
    if ($Path -cin @('usagestat.exe', 'usagestatd.exe', 'usagestat-service.exe')) {
        $name = if ($BackendProfile -eq 'dev') { $Path.Replace('.exe', '-dev.exe') } else { $Path }
        return 'bin/' + $name
    }
    if ($Path -ceq 'LICENSE' -or $Path.StartsWith('plugins/', [StringComparison]::Ordinal)) {
        return 'share/' + $app + '/' + $Path
    }
    throw 'Unexpected file outside the backend payload.'
}
function Invoke-Backend([string]$Directory, [string[]]$Arguments, [string]$Binary = 'usagestat') {
    $quoted = foreach ($argument in $Arguments) {
        # CommandLineToArgvW quoting, including quotes and trailing backslashes.
        '"' + [regex]::Replace([regex]::Replace($argument, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
    }
    $info = New-Object Diagnostics.ProcessStartInfo
    $executable = $Binary + $(if ($BackendProfile -eq 'dev') { '-dev' } else { '' }) + '.exe'
    $info.FileName = Join-Path $Directory ('bin/' + $executable)
    $info.Arguments = [string]::Join(' ', [string[]]$quoted)
    $info.WorkingDirectory = $parent
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = $utf8
    $info.StandardErrorEncoding = $utf8
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    try {
        if (-not $process.Start()) { throw 'Could not start the verified backend CLI.' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(45000)) { $process.Kill(); throw 'Backend command exceeded 45 seconds; use Recover if a transaction is pending.' }
        $output = $stdout.GetAwaiter().GetResult()
        $errorOutput = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "Backend command failed ($($process.ExitCode)): $errorOutput" }
        if ($output.Length -gt 8MB) { throw 'Backend command output exceeded its size limit.' }
        return $output
    } finally { $process.Dispose() }
}
function Get-State([string]$Directory) { return ((Invoke-Backend $Directory @('--json', 'daemon', 'status')) | ConvertFrom-Json) }
function Same-Path([string]$First, [string]$Second) {
    return $First -and $Second -and [string]::Equals([IO.Path]::GetFullPath($First).TrimEnd('\'), [IO.Path]::GetFullPath($Second).TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}
function Assert-Owned([string]$Directory) {
    Assert-NoReparse $Directory
    $record = Read-Json (Join-Path $Directory $markerName)
    if ($record.schemaVersion -ne 1 -or $record.package -cne 'usagestat' -or
        $record.profile -cne $app -or -not (Same-Path $record.destination $prefix)) { throw 'This directory belongs to another installation.' }
    $expected = New-Object 'Collections.Generic.Dictionary[string,object]' ([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in $record.files) {
        Assert-Relative $file.path
        if (-not ($file.path.StartsWith('bin/') -or $file.path.StartsWith('share/' + $app + '/'))) { throw 'Invalid owned payload path.' }
        $expected.Add($file.path, $file)
    }
    $count = 0
    foreach ($file in Get-ChildItem -LiteralPath $Directory -Recurse -Force) {
        if (($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Preserve the unexpected reparse point inside this installation.' }
        if ($file.PSIsContainer) { continue }
        $relative = $file.FullName.Substring($Directory.Length + 1).Replace('\', '/')
        if ($relative -ceq $markerName) { continue }
        if (-not $expected.ContainsKey($relative)) { throw "Preserve the unowned installation file: $relative" }
        $item = $expected[$relative]
        if ($file.Length -ne $item.size -or (Hash-File $file.FullName) -cne $item.sha256) { throw "Preserve the modified installation file: $relative" }
        $count++
    }
    if ($count -ne $expected.Count) { throw 'The installed payload is incomplete; preserve it for manual repair.' }
    return $record
}
function Check-Payload([string]$Directory, $Record) {
    foreach ($binary in @('usagestat', 'usagestatd', 'usagestat-service')) {
        $version = (Invoke-Backend $Directory @('--version') $binary).Trim()
        if ($version -cne ($binary + ' ' + $Record.version)) { throw 'Installed binary version disagrees with its verified manifest.' }
    }
    $caps = (Invoke-Backend $Directory @('capabilities', '--json')) | ConvertFrom-Json
    if ($caps.os -cne 'windows' -or $caps.architecture -cne 'x86_64' -or $caps.profile -cne $app -or
        -not $caps.features.'daemon.independentControls'.implemented -or -not $caps.features.'daemon.unregister'.implemented) {
        throw 'This payload does not implement the required native installer controls.'
    }
    $providers = @((Invoke-Backend $Directory @('--json', 'list')) | ConvertFrom-Json)
    if ($providers.Count -lt 1) { throw 'Installed provider resources could not be discovered.' }
}
function Stage-Payload([string]$Stage, [string]$ManifestPath) {
    Assert-Checksum $ManifestPath
    $meta = Read-Json $ManifestPath
    if ($meta.schemaVersion -ne 1 -or $meta.package -cne 'usagestat' -or
        $meta.target -cne 'x86_64-pc-windows-msvc' -or $meta.os -cne 'win32' -or $meta.arch -cne 'x64' -or
        $meta.sourceDirty -ne $false -or $meta.sourceCommit -cnotmatch '^[0-9a-f]{40}$' -or
        $meta.version -cnotmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$' -or $meta.signing -cne 'unsigned' -or
        $meta.archive.name -cne 'usagestat-windows-x86_64.zip') { throw 'Unexpected native release identity or signing contract.' }
    $archivePath = Join-Path ([IO.Path]::GetDirectoryName($ManifestPath)) $meta.archive.name
    if ($meta.archive.size -gt 512MB -or (Get-Item -LiteralPath $archivePath).Length -ne $meta.archive.size) { throw 'Archive size does not match its bounded manifest.' }
    Assert-Checksum $archivePath
    if ((Hash-File $archivePath) -cne $meta.archive.sha256) { throw 'Archive does not match the verified manifest.' }
    $expected = New-Object 'Collections.Generic.Dictionary[string,object]' ([StringComparer]::OrdinalIgnoreCase)
    $installed = @()
    [long]$total = 0
    foreach ($file in $meta.files) {
        Assert-Relative $file.path
        $targetPath = Payload-Path $file.path
        if ($file.size -lt 0 -or $file.size -gt 512MB -or $file.sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Invalid bounded payload metadata.' }
        $total += $file.size
        $expected.Add($file.path, $file)
        $installed += @{path=$targetPath; size=$file.size; sha256=$file.sha256}
    }
    if ($total -gt 2GB -or $expected.Count -gt 2000) { throw 'Payload exceeds its total size or file limit.' }
    foreach ($required in @('usagestat.exe', 'usagestatd.exe', 'usagestat-service.exe', 'LICENSE')) {
        if (-not $expected.ContainsKey($required)) { throw "Missing required payload: $required" }
    }
    [IO.Directory]::CreateDirectory($Stage) | Out-Null
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $seen = New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
        foreach ($entry in $zip.Entries) {
            Assert-Relative $entry.FullName
            $type = ($entry.ExternalAttributes -shr 16) -band 61440
            if ($type -ne 32768 -or ($entry.ExternalAttributes -band 1024) -ne 0 -or
                -not $expected.ContainsKey($entry.FullName) -or -not $seen.Add($entry.FullName)) { throw 'Archive contains an unexpected, duplicate or non-regular entry.' }
            $item = $expected[$entry.FullName]
            if ($entry.FullName -cne $item.path -or $entry.Length -ne $item.size) { throw 'Archive entry differs from its manifest.' }
            $filePath = Join-Path $Stage (Payload-Path $item.path)
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($filePath)) | Out-Null
            $inputStream = $entry.Open()
            $outputStream = [IO.File]::Open($filePath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $buffer = New-Object byte[] 65536
                [long]$written = 0
                while (($read = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
                    $written += $read
                    if ($written -gt $item.size) { throw 'Decompressed entry exceeds its verified size.' }
                    $outputStream.Write($buffer, 0, $read)
                }
                if ($written -ne $item.size) { throw 'Truncated archive entry.' }
            } finally { $outputStream.Dispose(); $inputStream.Dispose() }
            if ((Hash-File $filePath) -cne $item.sha256) { throw 'Extracted file checksum mismatch.' }
        }
        if ($seen.Count -ne $expected.Count) { throw 'Archive is missing verified files.' }
    } finally { $zip.Dispose() }
    $record = @{schemaVersion=1; package='usagestat'; profile=$app; destination=$prefix; version=$meta.version;
        sourceCommit=$meta.sourceCommit; archiveSha256=$meta.archive.sha256; signing='unsigned'; files=$installed}
    Write-Json (Join-Path $Stage $markerName) $record
    Check-Payload $Stage $record
    return $record
}
function Restore-State($State) {
    if (-not $State) { return }
    $current = Get-State $prefix
    if (-not $current.registered -or -not $current.managerAvailable -or -not (Same-Path $current.owner $prefix)) {
        throw 'The saved task owner changed during replacement; preserve the transaction for manual recovery.'
    }
    Invoke-Backend $prefix @('daemon', 'autostart', $(if ($State.autostart) { 'on' } else { 'off' })) | Out-Null
    Invoke-Backend $prefix @('daemon', $(if ($State.running) { 'start' } else { 'stop' })) | Out-Null
    $restored = Get-State $prefix
    if ($restored.running -ne $State.running -or $restored.autostart -ne $State.autostart -or $restored.t3Mode -cne $State.t3Mode -or
        ($State.running -and -not $restored.healthy)) { throw 'The restored daemon state failed its health/intent checks.' }
}
function Recover-Transaction {
    $journal = Read-Json $journalPath
    if ($journal.schemaVersion -ne 1 -or -not (Same-Path $journal.destination $prefix) -or $journal.profile -cne $app -or
        $journal.id -cnotmatch '^[0-9a-f]{32}$') { throw 'Unknown transaction; preserve it for manual recovery.' }
    $stage = Join-Path $parent ('.' + $leaf + '.stage-' + $journal.id)
    $backup = Join-Path $parent ('.' + $leaf + '.backup-' + $journal.id)
    Assert-NoReparse $stage
    Assert-NoReparse $backup
    if ([IO.Directory]::Exists($backup)) {
        Assert-Owned $backup | Out-Null
        if ([IO.Directory]::Exists($prefix)) {
            Assert-Owned $prefix | Out-Null
            if ($journal.state) {
                $current = Get-State $prefix
                if (-not (Same-Path $current.owner $prefix)) { throw 'The task owner changed; recovery preserved both installations.' }
                Invoke-Backend $prefix @('daemon', 'autostart', 'off') | Out-Null
                Invoke-Backend $prefix @('daemon', 'stop') | Out-Null
            }
            if ([IO.Directory]::Exists($stage)) { throw 'Both replacement directories exist; preserve them for manual recovery.' }
            [IO.Directory]::Move($prefix, $stage)
        }
        [IO.Directory]::Move($backup, $prefix)
    } elseif (-not $journal.hadInstallation -and [IO.Directory]::Exists($prefix)) {
        Assert-Owned $prefix | Out-Null
        [IO.Directory]::Delete($prefix, $true)
    } elseif ($journal.hadInstallation -and -not [IO.Directory]::Exists($prefix)) {
        throw 'The previous installation is missing; keep the transaction for manual recovery.'
    }
    if ($journal.hadInstallation) { Assert-Owned $prefix | Out-Null }
    Restore-State $journal.state
    if ([IO.Directory]::Exists($stage)) { [IO.Directory]::Delete($stage, $true) }
    [IO.File]::Delete($journalPath)
}

Assert-NoReparse $prefix
Assert-NoReparse $journalPath
Assert-NoReparse $lockPath
[IO.Directory]::CreateDirectory($parent) | Out-Null
$lock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
try {
    if ($Action -eq 'Recover') {
        if ([IO.File]::Exists($journalPath)) { Recover-Transaction }
        Write-Output 'Recovery complete. User configuration, history and credentials were retained.'
        return
    }
    if ([IO.File]::Exists($journalPath)) { throw 'A prior transaction needs recovery. Run this script with -Action Recover and the same destination/profile.' }
    $existing = [IO.Directory]::Exists($prefix)
    $oldRecord = if ($existing) { Assert-Owned $prefix } else { $null }
    if ($Action -eq 'Uninstall') {
        if (-not $existing) { Write-Output 'This installation is already absent.'; return }
        $state = Get-State $prefix
        if (Same-Path $state.owner $prefix) {
            if (-not $state.managerAvailable -or ($state.running -and -not $state.registered)) { throw 'Stop the foreground daemon or restore access to the owned task before uninstalling.' }
            if ($state.registered) { Invoke-Backend $prefix @('daemon', 'unregister') | Out-Null }
        }
        $removed = Join-Path $parent ('.' + $leaf + '.removed-' + [Guid]::NewGuid().ToString('N'))
        [IO.Directory]::Move($prefix, $removed)
        try { [IO.Directory]::Delete($removed, $true) }
        catch { throw "Registration removed; locked installation files remain at $removed. Close the process holding them and remove that directory. User data was retained." }
        Write-Output 'Uninstalled owned binaries and login registration. PATH was unchanged; configuration, history, credentials and T3 intent were retained.'
        return
    }
    if (-not $Manifest) { throw 'Install requires -Manifest pointing to a local release manifest with its ZIP and both SHA256 sidecars.' }
    $manifestPath = [IO.Path]::GetFullPath($Manifest)
    $id = [Guid]::NewGuid().ToString('N')
    $stage = Join-Path $parent ('.' + $leaf + '.stage-' + $id)
    $backup = Join-Path $parent ('.' + $leaf + '.backup-' + $id)
    $transactionWritten = $false
    try {
        $record = Stage-Payload $stage $manifestPath
        $state = $null
        if ($existing) {
            $current = Get-State $prefix
            if (Same-Path $current.owner $prefix) {
                if (-not $current.managerAvailable -or ($current.running -and -not $current.registered) -or
                    $current.condition -in @('installation-owner-mismatch', 'port-conflict')) { throw 'Preserve the conflicting/foreground process or repair task access before upgrading.' }
                if ($current.registered) { $state = $current }
            }
        }
        Write-Json $journalPath @{schemaVersion=1; id=$id; destination=$prefix; profile=$app; hadInstallation=$existing; state=$state}
        $transactionWritten = $true
        if ($state) {
            Invoke-Backend $prefix @('daemon', 'autostart', 'off') | Out-Null
            Invoke-Backend $prefix @('daemon', 'stop') | Out-Null
        }
        if ($existing) { [IO.Directory]::Move($prefix, $backup) }
        [IO.Directory]::Move($stage, $prefix)
        Check-Payload $prefix $record
        Restore-State $state
        # Commit before cleanup: a locked backup is retained for explicit removal
        # and must not cause a later Recover to undo an already healthy upgrade.
        [IO.File]::Delete($journalPath)
        $transactionWritten = $false
        if ($existing) {
            try { [IO.Directory]::Delete($backup, $true) }
            catch { Write-Warning "Upgrade succeeded. Close the process locking the old files, then remove $backup." }
        }
        Write-Output "Installed $app $($record.version) at $prefix (unsigned candidate). PATH was unchanged; existing daemon state was preserved."
    } catch {
        $original = $_
        if ($transactionWritten) {
            try { Recover-Transaction }
            catch { throw "Installation failed: $original Recovery requires help: $_ Keep $journalPath and both installation directories; rerun -Action Recover after resolving the cause." }
        }
        throw $original
    } finally {
        if (-not [IO.File]::Exists($journalPath) -and [IO.Directory]::Exists($stage)) { [IO.Directory]::Delete($stage, $true) }
    }
} finally { $lock.Dispose() }
