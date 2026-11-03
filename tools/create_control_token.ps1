[CmdletBinding()]
param(
    [string]$Path = "config/control.token",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$directory = (Get-Location).ProviderPath
$fullPath = [IO.Path]::GetFullPath($Path, $directory)
if ((Test-Path -LiteralPath $fullPath) -and -not $Force) {
    throw "Token already exists at $Path. Use -Force only when rotation is intentional."
}

$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
& (Join-Path $PSScriptRoot 'dev.ps1') -Workspace core -Task build -Package xtask
if ($LASTEXITCODE -ne 0) { throw 'Control token tool build failed.' }
$arguments = @('control-token', '--path', $fullPath)
if ($Force) { $arguments += '--force' }
$result = @(& (Join-Path $repository 'target/debug/xtask.exe') @arguments)
$code = $LASTEXITCODE
if ($code -ne 0) {
    foreach ($line in $result) { [Console]::Error.WriteLine($line) }
    exit $code
}
Write-Output "Created control token: $Path"

