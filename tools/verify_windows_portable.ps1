[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ArchivePath,
    [string]$EvidenceDirectory
)

$ErrorActionPreference = 'Stop'
$OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
if (-not $IsWindows) { throw 'Portable verification requires PowerShell 7 on Windows.' }
$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf)) { throw "Archive does not exist: $ArchivePath" }
$archive = [IO.Path]::GetFullPath($ArchivePath)
$name = [IO.Path]::GetFileNameWithoutExtension($archive)
if ([IO.Path]::GetFileName($archive) -cnotmatch '^AIex-Windows-x64-(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)\.zip$') {
    throw 'Archive name must be AIex-Windows-x64-VERSION.zip.'
}
$version = $Matches.version
# Prerelease archives use the layout of their numeric release line.
$requiresMigrator = [version]($version -replace '[-+].*$', '') -ge [version]'0.8.0'
if (-not $EvidenceDirectory) { $EvidenceDirectory = Join-Path $repository 'target' }
$evidenceParent = [IO.Path]::GetFullPath($EvidenceDirectory)
$evidence = Join-Path $evidenceParent ('portable-verification-' + [guid]::NewGuid().ToString('N'))
if (Test-Path -LiteralPath $evidence) { throw 'Evidence directory already exists.' }
[IO.Directory]::CreateDirectory($evidence) | Out-Null

function Resolve-PackageChild([string]$Root, [string]$Child) {
    $parent = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $path = [IO.Path]::GetFullPath((Join-Path $parent $Child))
    if (-not $path.StartsWith($parent + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Archive path is outside its destination: $Child"
    }
    return $path
}

function Invoke-HeadlessCheck([string]$Executable, [string[]]$Arguments, [int]$ExpectedExit = 0) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.WorkingDirectory = $evidence
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.Encoding]::UTF8
    $info.StandardErrorEncoding = [Text.Encoding]::UTF8
    $info.Environment['PATH'] = "$env:SystemRoot/System32;$env:SystemRoot"
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            $process.WaitForExit()
            throw 'Headless package check exceeded its deadline.'
        }
        $result = [pscustomobject]@{
            code = $process.ExitCode
            stdout = $output.GetAwaiter().GetResult()
            stderr = $errors.GetAwaiter().GetResult()
        }
        if ($result.code -ne $ExpectedExit) { throw "Unexpected package check exit code $($result.code): $($result.stderr)" }
        return $result
    } finally { $process.Dispose() }
}

$relocated = Resolve-PackageChild $evidence '中文目录 with spaces'
$package = Resolve-PackageChild $relocated $name
$expected = @('AIex.exe', 'ai-ex-service.exe', 'AIex.portable', '开始使用.txt', 'data/ai-ex.local.toml')
if ($requiresMigrator) { $expected += 'ai-ex-migrate.exe' }
$files = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($archive)
try {
    foreach ($entry in $zip.Entries) {
        $entryPath = $entry.FullName.Replace('\', '/')
        if (-not $entryPath.StartsWith($name + '/', [StringComparison]::Ordinal) -or
            @($entryPath.Split('/') | Where-Object { $_ -in @('.', '..') }).Count -gt 0) {
            throw 'Archive entry is outside the expected package directory.'
        }
        [void](Resolve-PackageChild $relocated $entryPath)
        if (-not $entry.Name) {
            if ($entryPath -cnotin @("$name/", "$name/data/")) { throw 'Archive contains an unexpected directory.' }
            continue
        }
        $relative = $entryPath.Substring($name.Length + 1)
        if ($relative -cnotin $expected -or -not $files.Add($relative)) {
            throw 'Archive contains an unexpected or duplicate file.'
        }
    }
    if ($files.Count -ne $expected.Count) { throw 'Archive is missing required package files.' }
    [IO.Directory]::CreateDirectory($relocated) | Out-Null
    [IO.Compression.ZipFileExtensions]::ExtractToDirectory($zip, $relocated)
} finally { $zip.Dispose() }

$installed = Invoke-HeadlessCheck (Join-Path $package 'AIex.exe') @('--check-install')
$status = $installed.stdout | ConvertFrom-Json
if (-not $status.ready -or $status.version -cne $version) { throw 'Packaged desktop version check failed.' }
$serviceVersion = Invoke-HeadlessCheck (Join-Path $package 'ai-ex-service.exe') @('--version')
if ($serviceVersion.stdout.Trim() -cne "ai-ex-service $version") { throw 'Packaged service version check failed.' }
if ($requiresMigrator) {
    $migratorVersion = Invoke-HeadlessCheck (Join-Path $package 'ai-ex-migrate.exe') @('--version')
    if ($migratorVersion.stdout.Trim() -cne "ai-ex-migrate $version") { throw 'Packaged migrator version check failed.' }
}
$fixture = Join-Path $repository 'config_examples/stage-replay.jsonl'
$replay = Invoke-HeadlessCheck (Join-Path $package 'ai-ex-service.exe') @(
    '--config', (Join-Path $package 'data/ai-ex.local.toml'), '--replay-stage', $fixture
)
$records = @(Get-Content -LiteralPath $fixture -Encoding UTF8 | Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json })
$replayLines = @($replay.stdout -split '\r?\n')
if ("OBS dry-run replay complete: records=$($records.Count)" -cnotin $replayLines) { throw 'Packaged service replay count differs.' }
foreach ($record in $records) {
    if ("stage action #$($record.sequence): $($record.action.type)" -cnotin $replayLines) { throw 'Packaged service replay action differs.' }
}
$data = Join-Path $package 'data'
if (@(Get-ChildItem -LiteralPath $data -Recurse -Force).Count -ne 1) { throw 'Package checks unexpectedly initialized user data.' }
$configHash = (Get-FileHash -LiteralPath (Join-Path $data 'ai-ex.local.toml') -Algorithm SHA256).Hash
if ($configHash -ne (Get-FileHash -LiteralPath (Join-Path $repository 'config/ai-ex.portable.example.toml') -Algorithm SHA256).Hash) {
    throw 'Packaged configuration differs from distributed defaults.'
}

$incomplete = Resolve-PackageChild $evidence 'incomplete'
[IO.Directory]::CreateDirectory((Join-Path $incomplete 'data')) | Out-Null
foreach ($file in @('AIex.exe', 'AIex.portable', 'data/ai-ex.local.toml')) {
    [IO.File]::Copy((Join-Path $package $file), (Join-Path $incomplete $file), $false)
}
$missing = Invoke-HeadlessCheck (Join-Path $incomplete 'AIex.exe') @('--check-install') 1
if (-not $missing.stderr.Contains('ai-ex-service.exe')) { throw 'Missing service executable was not reported.' }
if ($hash -ne (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()) { throw 'Archive changed during verification.' }
$reportPath = Join-Path $evidence 'verification.json'
$report = [pscustomobject]@{
    ready = $true
    version = $version
    archive = $archive
    archiveBytes = (Get-Item -LiteralPath $archive).Length
    sha256 = $hash
    fileCount = $files.Count
    migratorIncluded = $requiresMigrator
    noDevelopmentPath = $true
    crossDirectory = $true
    unicodePath = $true
    serviceReplay = $true
    incompletePackageRejected = $true
    evidenceDirectory = $evidence
    reportPath = $reportPath
}
$json = $report | ConvertTo-Json
$stream = [IO.File]::Open($reportPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
$writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
try { $writer.WriteLine($json) } finally { $writer.Dispose() }
$json
