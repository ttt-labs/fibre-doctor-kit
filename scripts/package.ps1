param([string]$Version = '0.2.0', [string]$ExampleReport)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$releaseDir = Join-Path $projectRoot ('dist\doctor-fibre-kit-v' + $Version)
if (Test-Path -LiteralPath $releaseDir) { throw 'Release output already exists; choose a new version.' }
$windowsExe = Join-Path $projectRoot 'target\release\doctor-fibre-kit.exe'
$linuxExe = Join-Path $projectRoot 'target\x86_64-unknown-linux-musl\release\doctor-fibre-kit'
if (!(Test-Path -LiteralPath $windowsExe) -or !(Test-Path -LiteralPath $linuxExe)) { throw 'Build both release targets first.' }
New-Item -ItemType Directory -Path $releaseDir | Out-Null
Copy-Item -LiteralPath $windowsExe -Destination (Join-Path $releaseDir 'doctor-fibre-kit-windows-x86_64.exe')
Copy-Item -LiteralPath $linuxExe -Destination (Join-Path $releaseDir 'doctor-fibre-kit-linux-x86_64-musl')
foreach ($name in @('README.md','LICENSE','collect.sh','Cargo.lock')) { Copy-Item -LiteralPath (Join-Path $projectRoot $name) -Destination $releaseDir }
Copy-Item -LiteralPath (Join-Path $projectRoot 'docs') -Destination $releaseDir -Recurse
if ($ExampleReport) {
    $reportPath = (Resolve-Path -LiteralPath $ExampleReport).Path
    $reportJson = [IO.Path]::ChangeExtension($reportPath, '.json')
    if ((Get-Content -Raw -LiteralPath $reportJson | ConvertFrom-Json).report_schema_version -ne 1) { throw 'Example must be a public report, not a raw snapshot.' }
    Copy-Item -LiteralPath $reportPath -Destination (Join-Path $releaseDir 'example-report.html')
    Copy-Item -LiteralPath $reportJson -Destination (Join-Path $releaseDir 'example-report.json')
}
$hashes = Get-ChildItem -LiteralPath $releaseDir -File -Recurse | Sort-Object FullName | ForEach-Object { '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.FullName.Substring($releaseDir.Length + 1).Replace('\','/') }
[IO.File]::WriteAllLines((Join-Path $releaseDir 'SHA256SUMS'), $hashes, [Text.UTF8Encoding]::new($false))
$archive = $releaseDir + '.zip'
& (Join-Path $PSScriptRoot 'archive-release.ps1') -Directory $releaseDir -Destination $archive
$archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() + '  ' + [IO.Path]::GetFileName($archive)
[IO.File]::WriteAllText($archive + '.sha256', $archiveHash + "`n", [Text.UTF8Encoding]::new($false))
Write-Output $releaseDir
