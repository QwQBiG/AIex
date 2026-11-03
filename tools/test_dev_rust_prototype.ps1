[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$FakeToolCompiler,
    [string[]]$FakeCompilerArguments = @()
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Prototype regression checks require Windows.' }
$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$fixture = Join-Path $repository ('target/maintenance/dev-prototype-' + [guid]::NewGuid().ToString('N'))
$tools = Join-Path $fixture 'tools'
$working = Join-Path $fixture '中文 working directory'
[void][IO.Directory]::CreateDirectory($tools)
[void][IO.Directory]::CreateDirectory($working)
$encoding = [Text.UTF8Encoding]::new($false)
$prototype = Join-Path $tools 'dev_rust_prototype.ps1'
$development = Join-Path $tools 'dev.ps1'
$harness = Join-Path $fixture 'driver.ps1'
$source = Join-Path $fixture 'fake.rs'
$fake = Join-Path $fixture 'fake.exe'
$log = Join-Path $fixture 'calls.log'
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'dev_rust_prototype.ps1') -Destination $prototype
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'dev.ps1') -Destination $development

# This executable checks arguments and lock handoff without invoking Cargo.
$fakeSource = @'
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;

fn run() -> i32 {
    let Some(root) = std::env::var_os("DEV_TEST_ROOT") else { return 91; };
    let lock = PathBuf::from(&root).join("target/.development.lock");
    if OpenOptions::new().read(true).write(true).share_mode(0).open(lock).is_err() { return 92; }
    let Some(expected) = std::env::var_os("DEV_TEST_INHERITED") else { return 93; };
    if std::env::var_os("CARGO_TARGET_DIR") != Some(expected) { return 94; }
    if std::env::var("CARGO_INCREMENTAL").ok().as_deref() != Some("caller-incremental") { return 101; }
    if std::env::var("CARGO_BUILD_JOBS").ok().as_deref() != Some("7") { return 102; }
    let actual: Vec<_> = std::env::args_os().skip(1).collect();
    let mut wanted: Vec<OsString> = ["dev", "--project"].into_iter().map(Into::into).collect();
    wanted.push(root);
    wanted.extend([
        "--workspace", "native", "--task", "test", "--jobs", "3", "--minimum-free-gib", "1",
        "--package", "ai-ex-domain", "--test-filter", "中文 test filter", "--keep-incremental",
        "--release", "--offline",
    ].into_iter().map(OsString::from));
    if actual != wanted { return 95; }
    if std::env::var("DEV_TEST_TOOL_FAIL").ok().as_deref() == Some("1") { return 23; }
    println!("fake development tool");
    0
}
fn main() { std::process::exit(run()); }
'@
[IO.File]::WriteAllText($source, $fakeSource, $encoding)
& $FakeToolCompiler --edition 2024 $source -o $fake @FakeCompilerArguments
if ($LASTEXITCODE -ne 0) { throw 'Cannot build isolated fake tool.' }

$child = @'
Set-Location -LiteralPath $env:DEV_TEST_WORKING
$env:CARGO_TARGET_DIR = $env:DEV_TEST_INHERITED
$env:CARGO_INCREMENTAL = 'caller-incremental'
$env:CARGO_BUILD_JOBS = '7'
function global:cargo {
    [IO.File]::AppendAllText($env:DEV_TEST_LOG, "build`n", [Text.UTF8Encoding]::new($false))
    if ($env:CARGO_TARGET_DIR -cne (Join-Path $env:DEV_TEST_ROOT 'target') -or $env:CARGO_INCREMENTAL -cne '0' -or $env:CARGO_BUILD_JOBS -cne '3') { throw 'incorrect build environment' }
    try {
        $handle = [IO.File]::Open((Join-Path $env:CARGO_TARGET_DIR '.development.lock'), 'Open', 'ReadWrite', 'None')
        $handle.Dispose()
        throw 'bootstrap did not hold the target lock'
    } catch [IO.IOException] { }
    if ($env:DEV_TEST_THROW -eq '1') { throw 'synthetic Cargo launch error' }
    if ($env:DEV_TEST_BUILD_FAIL -eq '1') { $global:LASTEXITCODE = 37; return }
    $debug = Join-Path $env:CARGO_TARGET_DIR 'debug'
    [void][IO.Directory]::CreateDirectory($debug)
    Copy-Item -LiteralPath $env:DEV_TEST_FAKE -Destination (Join-Path $debug 'xtask.exe')
    $global:LASTEXITCODE = 0
}
$options = @{ Workspace = 'native'; Task = 'test'; Package = @('ai-ex-domain'); TestFilter = '中文 test filter'; Jobs = 3; MinimumFreeGiB = 1; Offline = $true; Release = $true; KeepIncremental = $true }
if ($env:DEV_TEST_PLAN -eq '1') { $options.Plan = $true }
if ($env:DEV_TEST_INVALID -eq '1') { $options.TestFilter = '--nocapture' }
& $env:DEV_TEST_PROTOTYPE @options
exit $LASTEXITCODE
'@
[IO.File]::WriteAllText($harness, $child, $encoding)

function Invoke-Fixture([hashtable]$Overrides, [int]$ExpectedExit, [string]$ExpectedError = '') {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = (Get-Process -Id $PID).Path
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = $encoding
    $info.StandardErrorEncoding = $encoding
    foreach ($argument in @('-NoProfile', '-File', $harness)) { $info.ArgumentList.Add($argument) }
    foreach ($entry in @{
        DEV_TEST_ROOT = $fixture; DEV_TEST_WORKING = $working; DEV_TEST_INHERITED = (Join-Path $working 'inherited target')
        DEV_TEST_LOG = $log; DEV_TEST_FAKE = $fake; DEV_TEST_PROTOTYPE = $prototype
        DEV_TEST_PLAN = '0'; DEV_TEST_BUILD_FAIL = '0'; DEV_TEST_TOOL_FAIL = '0'; DEV_TEST_INVALID = '0'; DEV_TEST_THROW = '0'
        CARGO_BUILD_TARGET = ''
    }.GetEnumerator()) { $info.Environment[$entry.Key] = $entry.Value }
    foreach ($entry in $Overrides.GetEnumerator()) { $info.Environment[$entry.Key] = [string]$entry.Value }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(20000)) { $process.Kill(); $process.WaitForExit(); throw 'Prototype fixture timed out.' }
        $output = $stdout.GetAwaiter().GetResult()
        $errors = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne $ExpectedExit) { throw "Unexpected fixture exit $($process.ExitCode), expected $ExpectedExit. $errors" }
        if ($ExpectedError -and !$errors.Contains($ExpectedError)) { throw "Fixture did not report the expected error: $ExpectedError" }
        return $output
    } finally { $process.Dispose() }
}

$plan = (Invoke-Fixture @{ DEV_TEST_PLAN = '1' } 0 | ConvertFrom-Json)
if ($plan.target -cne (Join-Path $fixture 'target') -or (Test-Path -LiteralPath $log) -or (Test-Path -LiteralPath (Join-Path $fixture 'target'))) { throw 'Plan wrote files or selected the wrong target.' }
$lockDirectory = Join-Path $fixture 'target'
[void](Invoke-Fixture @{ CARGO_BUILD_TARGET = 'x86_64-pc-windows-msvc' } 1 'default build target')
[void](Invoke-Fixture @{ CARGO_BUILD_TARGET = 'x86_64-pc-windows-msvc'; DEV_TEST_PLAN = '1' } 0)
if ((Test-Path -LiteralPath $log) -or (Test-Path -LiteralPath $lockDirectory)) { throw 'Configured target started Cargo or created a target directory.' }

$junctionDestination = Join-Path $fixture 'junction-destination'
$junctionSentinel = Join-Path $junctionDestination 'fixture.txt'
[void][IO.Directory]::CreateDirectory($junctionDestination)
[IO.File]::WriteAllText($junctionSentinel, 'fixture sentinel', $encoding)
[void](New-Item -ItemType Junction -Path $lockDirectory -Target $junctionDestination)
$junction = Get-Item -LiteralPath $lockDirectory
if (-not ($junction.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
    [IO.Path]::GetFullPath($junction.Target) -cne [IO.Path]::GetFullPath($junctionDestination)) { throw 'Fixture junction does not point to its own directory.' }
[void](Invoke-Fixture @{} 1 'linked target directory')
if ((Test-Path -LiteralPath $log) -or [IO.File]::ReadAllText($junctionSentinel, $encoding) -cne 'fixture sentinel' -or
    @(Get-ChildItem -LiteralPath $junctionDestination -Force).Count -ne 1) { throw 'Linked target was modified or Cargo started.' }
[IO.Directory]::Delete($lockDirectory)
if ([IO.File]::ReadAllText($junctionSentinel, $encoding) -cne 'fixture sentinel') { throw 'Junction cleanup modified its target.' }
Remove-Item -LiteralPath $junctionSentinel
Remove-Item -LiteralPath $junctionDestination

[void][IO.Directory]::CreateDirectory($lockDirectory)
$lockPath = Join-Path $lockDirectory '.development.lock'
$lease = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
try {
    [void](Invoke-Fixture @{ DEV_TEST_PLAN = '1' } 0)
    [void](Invoke-Fixture @{} 1)
    if (Test-Path -LiteralPath $log) { throw 'Busy target started Cargo.' }
} finally { $lease.Dispose() }
[void](Invoke-Fixture @{} 0)
[void](Invoke-Fixture @{ DEV_TEST_BUILD_FAIL = '1' } 37)
[void](Invoke-Fixture @{ DEV_TEST_TOOL_FAIL = '1' } 23)
[void](Invoke-Fixture @{ DEV_TEST_THROW = '1' } 1)
$before = [IO.File]::ReadAllText($log, $encoding)
[void](Invoke-Fixture @{ DEV_TEST_INVALID = '1' } 1)
if ([IO.File]::ReadAllText($log, $encoding) -cne $before) { throw 'Invalid input started Cargo.' }
$copies = @(Get-ChildItem -LiteralPath $lockDirectory -Filter 'xtask-dev-*.exe' -File)
if ($copies.Count -ne 0) { throw 'Prototype left its temporary executable.' }
$retry = [IO.File]::Open($lockPath, 'Open', 'ReadWrite', 'None')
$retry.Dispose()

# Delete only exact paths created by this fixture.
foreach ($file in @($prototype, $development, $harness, $source, $fake, $log, (Join-Path $lockDirectory 'debug/xtask.exe'), $lockPath)) { Remove-Item -LiteralPath $file }
foreach ($directory in @($tools, $working, (Join-Path $lockDirectory 'debug'), $lockDirectory, $fixture)) { [IO.Directory]::Delete($directory) }
[pscustomobject]@{ status = 'passed'; checked_cases = 9; real_cargo_executed = $false } | ConvertTo-Json -Compress
