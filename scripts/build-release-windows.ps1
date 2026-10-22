param([string]$OutputDirectory, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (!$OutputDirectory) { $OutputDirectory = Join-Path $repo 'target\windows-release' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
if ($env:OS -ne 'Windows_NT' -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'Build requires Windows x64.' }
Push-Location $repo
try {
    $metadata = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $package = ($metadata | ConvertFrom-Json).packages | Where-Object name -eq 'codex-zectrix-dashboard'
    $version = $package.version
    if ($version -notmatch '^\d[0-9A-Za-z.-]{0,47}$' -or $version.Contains('..')) { throw 'Invalid package version' }
    if (!$SkipBuild) {
        & cargo build --locked --release --features desktop --bin codex-zectrix-dashboard --bin codex-zectrix-companion --bin codex-zectrix-desktop
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    [IO.Directory]::CreateDirectory($output) | Out-Null
    $stage = Join-Path $output ('package-' + $version + '-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($stage) | Out-Null
    $files = [ordered]@{}
    foreach ($name in @('codex-zectrix-dashboard.exe', 'codex-zectrix-companion.exe')) {
        $source = Join-Path $repo ('target\release\' + $name)
        # verify-package below captures both binaries through redirected process handles;
        # Windows PowerShell does not reliably capture a GUI-subsystem executable with &.
        Copy-Item -LiteralPath $source -Destination (Join-Path $stage $name)
    }
    Copy-Item -LiteralPath (Join-Path $repo 'target\release\codex-zectrix-desktop.exe') -Destination (Join-Path $stage 'NOTE4控制台.exe')
    Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination (Join-Path $stage 'LICENSE')
    Copy-Item -LiteralPath (Join-Path $repo 'THIRD-PARTY-NOTICES.txt') -Destination (Join-Path $stage 'THIRD-PARTY-NOTICES.txt')
    Copy-Item -LiteralPath (Join-Path $repo 'docs\windows-quickstart.txt') -Destination (Join-Path $stage '快速入门.txt')
    foreach ($name in @('codex-zectrix-dashboard.exe', 'codex-zectrix-companion.exe', 'NOTE4控制台.exe', 'LICENSE', 'THIRD-PARTY-NOTICES.txt', '快速入门.txt')) {
        if ((Get-Item -LiteralPath (Join-Path $stage $name)).Length -gt 64MB) { throw "Package file exceeds 64 MiB: $name" }
        $files[$name] = (Get-FileHash -LiteralPath (Join-Path $stage $name) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $manifest = [ordered]@{schemaVersion=2; version=$version; platform='windows-x86_64'; files=$files}
    [IO.File]::WriteAllText((Join-Path $stage 'package.json'), ($manifest | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))
    & (Join-Path $stage 'codex-zectrix-dashboard.exe') verify-package $stage
    if ($LASTEXITCODE -ne 0) { throw 'Staged package validation failed' }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zipName = "codex-zectrix-dashboard-$version-windows-x86_64.zip"
    $temporaryZip = Join-Path $output ([Guid]::NewGuid().ToString('N') + '.zip')
    [IO.Compression.ZipFile]::CreateFromDirectory($stage, $temporaryZip)
    $zip = Join-Path $output $zipName
    Move-Item -LiteralPath $temporaryZip -Destination $zip -Force
    $sum = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText((Join-Path $output 'SHA256SUMS'), "$sum  $zipName`n", [Text.UTF8Encoding]::new($false))
    Write-Output "Windows package: $zip"
} finally { Pop-Location }
