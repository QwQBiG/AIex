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
if ([IO.Path]::GetFileName($archive) -cnotmatch '^AIex-Windows-x64-\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\.zip$') {
    throw 'Archive name must be AIex-Windows-x64-VERSION.zip.'
}
if (-not $EvidenceDirectory) { $EvidenceDirectory = Join-Path $repository 'target' }
$evidenceParent = [IO.Path]::GetFullPath($EvidenceDirectory)
[IO.Directory]::CreateDirectory($evidenceParent) | Out-Null

& (Join-Path $PSScriptRoot 'dev.ps1') -Workspace core -Task build -Package xtask
if ($LASTEXITCODE -ne 0) { throw 'Portable verification tool build failed.' }
& (Join-Path $repository 'target/debug/xtask.exe') portable-verify --archive $archive --evidence-directory $evidenceParent
exit $LASTEXITCODE
