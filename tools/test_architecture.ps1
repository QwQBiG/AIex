$ErrorActionPreference = 'Stop'
$OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

& (Join-Path $PSScriptRoot 'dev.ps1') -Workspace core -Task test -Package xtask -TestFilter 'architecture::tests'
exit $LASTEXITCODE
