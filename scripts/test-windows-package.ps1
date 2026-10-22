param([string]$ZipPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (!$ZipPath) {
    $ZipPath = (Get-ChildItem -LiteralPath (Join-Path $repo 'target\windows-release') -Filter '*-windows-x86_64.zip' | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1).FullName
}
$ZipPath = (Resolve-Path -LiteralPath $ZipPath).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ('Codex-package-' + [Guid]::NewGuid().ToString('N'))
$extract = Join-Path $work '中文路径 with spaces & test'
[IO.Directory]::CreateDirectory($extract) | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::OpenRead($ZipPath)
try {
    $expected = @('codex-zectrix-dashboard.exe', 'codex-zectrix-companion.exe', 'NOTE4控制台.exe', 'LICENSE', 'THIRD-PARTY-NOTICES.txt', '快速入门.txt', 'package.json')
    $actual = @($archive.Entries | ForEach-Object FullName)
    if ($actual.Count -ne 7 -or @(Compare-Object $expected $actual).Count -ne 0) { throw 'ZIP contains unexpected files' }
} finally { $archive.Dispose() }
[IO.Compression.ZipFile]::ExtractToDirectory($ZipPath, $extract)
$exe = Join-Path $extract 'codex-zectrix-dashboard.exe'
& $exe verify-package $extract
if ($LASTEXITCODE -ne 0) { throw 'Extracted package validation failed' }
$png = Join-Path $work 'preview.png'
& $exe preview --output $png
if ($LASTEXITCODE -ne 0) { throw 'Packaged preview failed' }
$bytes = [IO.File]::ReadAllBytes($png)
if ($bytes.Length -lt 33 -or [BitConverter]::ToString($bytes[0..7]) -ne '89-50-4E-47-0D-0A-1A-0A' -or [BitConverter]::ToString($bytes[16..23]) -ne '00-00-01-90-00-00-01-2C' -or $bytes[24] -ne 1) { throw 'Expected 400x300 one-bit PNG' }
Write-Output "PASS: seven ZIP files, hashes, all three binary versions, Unicode path and offline 400x300 preview. Work directory: $work"
