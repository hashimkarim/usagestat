param(
    [Parameter(Mandatory)][ValidateSet('scoop', 'winget', 'chocolatey')][string]$Channel,
    [Parameter(Mandatory)][string]$RecipeDirectory,
    [Parameter(Mandatory)][string]$UpstreamVersion
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Feed installation tests require a disposable hosted Windows runner.' }
$recipes = (Resolve-Path -LiteralPath $RecipeDirectory).Path
$profileRoot = Join-Path $env:RUNNER_TEMP ('usagestat-feed-' + [guid]::NewGuid().ToString('N'))
$env:USAGESTAT_CONFIG_DIR = Join-Path $profileRoot 'config'
$env:USAGESTAT_DATA_DIR = Join-Path $profileRoot 'data'
New-Item -ItemType Directory -Path $env:USAGESTAT_CONFIG_DIR, $env:USAGESTAT_DATA_DIR -Force | Out-Null
Set-Content -LiteralPath (Join-Path $env:USAGESTAT_CONFIG_DIR 'config.toml') -Value 'providers = []'
Set-Content -LiteralPath (Join-Path $env:USAGESTAT_DATA_DIR 'retained-fixture') -Value 'synthetic history'
$checks = [Collections.Generic.List[string]]::new()

function Check-Exit([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed with exit code $LASTEXITCODE" }
}
function Refresh-Path {
    $env:PATH = [Environment]::GetEnvironmentVariable('PATH', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('PATH', 'User')
}
function Check-Backend {
    Refresh-Path
    $cli = (Get-Command usagestat.exe -ErrorAction Stop).Source
    if ((& $cli --version) -ne "usagestat $UpstreamVersion") { throw 'Installed CLI version mismatch.' }
    Check-Exit 'CLI version'
    $daemon = (Get-Command usagestatd.exe -ErrorAction Stop).Source
    if ((& $daemon --version) -ne "usagestatd $UpstreamVersion") { throw 'Installed daemon version mismatch.' }
    Check-Exit 'Daemon version'
    $providers = (& $cli --json list | ConvertFrom-Json)
    Check-Exit 'Installed provider discovery'
    if ($providers.Count -ne 61) { throw 'Installed provider inventory is incomplete.' }
    foreach ($provider in $providers) {
        if ($provider.icon.path -and -not (Test-Path -LiteralPath $provider.icon.path)) { throw 'Installed provider icon is missing.' }
    }
    $status = (& $cli daemon status --json | ConvertFrom-Json)
    Check-Exit 'Daemon status'
    if ($status.configured -or $status.registered -or $status.running) { throw 'Package installation implicitly configured a daemon.' }
    $checks.Add('installed-shims-versions-resources-no-implicit-startup')
}

try {
    switch ($Channel) {
        'chocolatey' {
            choco install chocolatey-community-validation.extension --version 0.2.0 --yes --no-progress
            Check-Exit 'Install Chocolatey community metadata validator'
            $nuspec = Get-ChildItem -LiteralPath $recipes -Filter '*.nuspec' | Select-Object -First 1
            [xml]$metadata = Get-Content -LiteralPath $nuspec.FullName -Raw
            $packageId = $metadata.package.metadata.id
            $packageVersion = $metadata.package.metadata.version
            if ($packageId -ne 'usagestat') { throw 'Chocolatey requires a release-neutral package ID.' }
            $packed = Join-Path $recipes 'packed'
            New-Item -ItemType Directory -Path $packed | Out-Null
            choco pack $nuspec.FullName --outputdirectory $packed
            Check-Exit 'Chocolatey pack'
            choco install $packageId --version $packageVersion --pre --source $packed --yes --no-progress
            Check-Exit 'Chocolatey installation'
            Check-Backend
            if (Test-Path -LiteralPath (Join-Path $env:ChocolateyInstall 'bin/usagestat-service.exe')) { throw 'Service supervisor must not have a public command shim.' }
            choco uninstall $packageId --yes --no-progress
            Check-Exit 'Chocolatey removal'
        }
        'scoop' {
            $installer = Join-Path $profileRoot 'install-scoop.ps1'
            Invoke-WebRequest -Uri 'https://get.scoop.sh' -OutFile $installer
            & $installer -RunAsAdmin -ScoopDir (Join-Path $profileRoot 'scoop')
            Refresh-Path
            $scoop = Join-Path $profileRoot 'scoop/shims/scoop.ps1'
            & $scoop install (Join-Path $recipes 'bucket/usagestat-alpha.json')
            Check-Exit 'Scoop installation'
            Check-Backend
            & $scoop reset usagestat-alpha
            Check-Exit 'Scoop shim reset'
            Check-Backend
            & $scoop uninstall usagestat-alpha
            Check-Exit 'Scoop removal'
        }
        'winget' {
            if (-not (Get-Command winget.exe -ErrorAction SilentlyContinue)) {
                Install-Module -Name Microsoft.WinGet.Client -Repository PSGallery -Scope CurrentUser -Force
                Repair-WinGetPackageManager -AllUsers
                Refresh-Path
            }
            $versionManifest = Get-ChildItem -LiteralPath $recipes -Filter 'HashimKarim.UsageStat.Alpha.yaml' -Recurse | Select-Object -First 1
            $manifestDirectory = $versionManifest.Directory.FullName
            winget validate --manifest $manifestDirectory
            Check-Exit 'WinGet manifest validation'
            winget settings --enable LocalManifestFiles
            Check-Exit 'Enable local manifests on the disposable runner'
            winget install --manifest $manifestDirectory --scope user --accept-package-agreements --accept-source-agreements --disable-interactivity
            Check-Exit 'WinGet installation'
            Check-Backend
            # Before upstream publication, WinGet cannot correlate this local
            # manifest's identifier with a source entry. Match its unique ARP name.
            winget uninstall --name usagestat-alpha --exact --disable-interactivity --accept-source-agreements --purge
            Check-Exit 'WinGet removal'
        }
    }
    if ((Get-Content -LiteralPath (Join-Path $env:USAGESTAT_DATA_DIR 'retained-fixture')).Trim() -ne 'synthetic history') { throw 'Package removal deleted user data.' }
    $checks.Add('uninstall-retains-user-state')
    @{ channel = $Channel; version = $UpstreamVersion; checks = @($checks); desktopAcceptance = 'pending' } |
        ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path (Split-Path $recipes) "$Channel-result.json")
} finally {
    # Keep logs, recipes and this isolated profile available for failure diagnosis.
    Write-Output "Feed test profile: $profileRoot"
}
