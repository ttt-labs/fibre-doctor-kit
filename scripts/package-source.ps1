param([string]$Destination)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (!$Destination) { $Destination = Join-Path $projectRoot 'dist\doctor-fibre-kit-source.zip' }
if (Test-Path -LiteralPath $Destination) { throw 'Source archive already exists; choose an unused path.' }

$sourceFiles = @('.gitattributes', '.gitignore', 'Cargo.toml', 'Cargo.lock', 'LICENSE', 'README.md', 'collect.sh')
$sourceDirectories = @('.github', 'assets', 'docs', 'scripts', 'src', 'tests')
$allowedExtensions = @('.yml', '.yaml', '.html', '.json', '.md', '.ps1', '.rs', '.py', '.toml', '.sh', '.png')
$files = @($sourceFiles | ForEach-Object { Get-Item -LiteralPath (Join-Path $projectRoot $_) })
foreach ($directory in $sourceDirectories) {
    $files += Get-ChildItem -LiteralPath (Join-Path $projectRoot $directory) -Recurse -File |
        Where-Object { $allowedExtensions -contains $_.Extension -and $_.FullName -notmatch '[\\/]__pycache__[\\/]' }
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression
$destinationPath = [IO.Path]::GetFullPath($Destination)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destinationPath)) | Out-Null
$zip = [IO.Compression.ZipFile]::Open($destinationPath, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($file in ($files | Sort-Object FullName -Unique)) {
        $relative = $file.FullName.Substring($projectRoot.Length + 1).Replace('\', '/')
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $file.FullName, 'doctor-fibre-kit/' + $relative, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
    }
} finally { $zip.Dispose() }
$hash = (Get-FileHash -LiteralPath $destinationPath -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText($destinationPath + '.sha256', $hash + '  ' + [IO.Path]::GetFileName($destinationPath) + "`n", [Text.UTF8Encoding]::new($false))
Write-Output $destinationPath
