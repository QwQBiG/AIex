[CmdletBinding()]
param()

# Each child replaces Cargo with a recording function and excludes compiler tools
# from PATH. Fixtures contain reports only; no Rust build or device is started.
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Development regression checks require PowerShell 7 on Windows.' }
$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$fixture = Join-Path $repository ('target/development-tests-' + [guid]::NewGuid().ToString('N'))
$working = Join-Path $fixture '运行 中文 空格'
[void][IO.Directory]::CreateDirectory($working)
$harness = Join-Path $fixture 'child.ps1'
$utf8 = [Text.UTF8Encoding]::new($false)
$cases = [Collections.Generic.List[object]]::new()

function Assert-Development([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Write-DevelopmentJson([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, (($Value | ConvertTo-Json -Depth 12) + "`n"), $utf8)
}

$child = @'
param([Parameter(Mandatory)][string]$RequestPath)
$ErrorActionPreference = 'Stop'
$request = Get-Content -LiteralPath $RequestPath -Encoding UTF8 -Raw | ConvertFrom-Json
$global:DevelopmentFake = @{ request = $request; calls = [Collections.Generic.List[object]]::new() }
function global:cargo {
    $values = @($args | ForEach-Object { [string]$_ })
    $global:DevelopmentFake.calls.Add([ordered]@{
        arguments = $values
        target = $env:CARGO_TARGET_DIR
        incremental = $env:CARGO_INCREMENTAL
        jobs = $env:CARGO_BUILD_JOBS
        cwd = (Get-Location).Path
    })
    if ($global:DevelopmentFake.request.fail_step -eq ('throw:' + $values[0])) { throw 'Synthetic Cargo launch failure' }
    $global:LASTEXITCODE = if ($values[0] -eq $global:DevelopmentFake.request.fail_step) { 37 } else { 0 }
}
$env:CARGO_TARGET_DIR = Join-Path $request.working 'inherited-target'
$env:CARGO_INCREMENTAL = [string]$request.inherited_incremental
$env:CARGO_BUILD_JOBS = '7'
Set-Location -LiteralPath $request.working
$before = @{ target = $env:CARGO_TARGET_DIR; incremental = $env:CARGO_INCREMENTAL; jobs = $env:CARGO_BUILD_JOBS; cwd = (Get-Location).Path }
$options = @{}
foreach ($property in $request.options.PSObject.Properties) { $options[$property.Name] = $property.Value }
$output = @()
$errorMessage = $null
$code = 0
try {
    $global:LASTEXITCODE = 0
    $output = @(& $request.development @options)
    $code = $global:LASTEXITCODE
} catch {
    $errorMessage = $_.Exception.Message
    $code = 1
} finally {
    $after = @{ target = $env:CARGO_TARGET_DIR; incremental = $env:CARGO_INCREMENTAL; jobs = $env:CARGO_BUILD_JOBS; cwd = (Get-Location).Path }
    $report = @{ before = $before; after = $after; calls = @($global:DevelopmentFake.calls); output = $output; error = $errorMessage }
    [IO.File]::WriteAllText($request.result, ($report | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
}
exit $code
'@
[IO.File]::WriteAllText($harness, $child.Replace("`r`n", "`n").Replace("`n", "`r`n"), $utf8)

function Invoke-Development([hashtable]$Options, [int]$ExpectedExit = 0, [string]$FailStep = '', [string]$InheritedIncremental = '1') {
    # Fake commands write tiny reports, so CI needs no real-build space reserve.
    $Options = $Options.Clone()
    if (-not $Options.ContainsKey('MinimumFreeGiB')) { $Options.MinimumFreeGiB = 1 }
    $id = [guid]::NewGuid().ToString('N')
    $requestPath = Join-Path $fixture ($id + '.request.json')
    $resultPath = Join-Path $fixture ($id + '.result.json')
    Write-DevelopmentJson $requestPath @{
        development = Join-Path $PSScriptRoot 'dev.ps1'; working = $working
        result = $resultPath; options = $Options; fail_step = $FailStep; inherited_incremental = $InheritedIncremental
    }
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = (Get-Process -Id $PID).Path
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = $utf8
    $info.StandardErrorEncoding = $utf8
    $info.Environment['PATH'] = Join-Path $env:SystemRoot 'System32'
    foreach ($argument in @('-NoProfile', '-File', $harness, '-RequestPath', $requestPath)) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(20000)) { $process.Kill(); $process.WaitForExit(); throw 'Fake development invocation timed out.' }
        $errors = $stderr.GetAwaiter().GetResult()
        [IO.File]::WriteAllText((Join-Path $fixture ($id + '.stdout.log')), $stdout.GetAwaiter().GetResult(), $utf8)
        [IO.File]::WriteAllText((Join-Path $fixture ($id + '.stderr.log')), $errors, $utf8)
        Assert-Development ($process.ExitCode -eq $ExpectedExit) "Unexpected development exit $($process.ExitCode), expected $ExpectedExit. $errors"
        $result = Get-Content -LiteralPath $resultPath -Encoding UTF8 -Raw | ConvertFrom-Json
    } finally { $process.Dispose() }
    foreach ($name in @('target', 'incremental', 'jobs', 'cwd')) {
        Assert-Development ($result.before.$name -ceq $result.after.$name) "Caller $name was not restored."
    }
    return $result
}

function Add-DevelopmentCase([string]$Name) {
    $cases.Add([ordered]@{ name = $Name; status = 'passed' })
}

try {
    $coreTarget = Join-Path $repository 'target'
    $desktopTarget = Join-Path $repository 'crates/ai-ex-desktop/target'
    $nativeExtra = Join-Path $repository 'native-workers/target'
    $nativeExtraExisted = Test-Path -LiteralPath $nativeExtra
    foreach ($workspace in @('core', 'native', 'desktop')) {
        $result = Invoke-Development @{ Workspace = $workspace; Task = 'check'; Plan = $true; Offline = $true }
        $plan = ($result.output -join "`n") | ConvertFrom-Json
        $expected = if ($workspace -eq 'desktop') { $desktopTarget } else { $coreTarget }
        Assert-Development ($plan.target -ceq $expected) 'Workspace did not select its stable target.'
        Assert-Development (@($result.calls).Count -eq 0) 'Plan invoked Cargo.'
    }
    Assert-Development ((Test-Path -LiteralPath $nativeExtra) -eq $nativeExtraExisted) 'Plan created a native workspace build directory.'
    Add-DevelopmentCase 'read_only_plans_use_two_stable_targets'

    $release = Invoke-Development @{ Task = 'build'; Release = $true }
    $override = Invoke-Development @{ Task = 'build'; Release = $true; KeepIncremental = $true }
    $inherited = Invoke-Development @{ Task = 'check' } -InheritedIncremental '0'
    Assert-Development ($release.calls[0].incremental -ceq '0') 'Release enabled incremental caches by default.'
    Assert-Development ($release.calls[0].jobs -ceq '2') 'Default builds exceeded the bounded job policy.'
    Assert-Development ($override.calls[0].incremental -ceq '1') 'Explicit incremental override was lost.'
    Assert-Development ($inherited.calls[0].incremental -ceq '0') 'Daily check ignored the inherited incremental preference.'
    Add-DevelopmentCase 'release_incremental_is_opt_in'

    $filter = '中文 regression with spaces'
    $filtered = Invoke-Development @{ Task = 'test'; Package = @('ai-ex-domain'); TestFilter = $filter; Jobs = 3; Offline = $true }
    $call = $filtered.calls[0]
    Assert-Development (@($filtered.calls).Count -eq 1) 'Package test started extra Cargo commands.'
    Assert-Development (($call.arguments -contains '--package') -and ($call.arguments -contains 'ai-ex-domain') -and ($call.arguments -notcontains '--workspace')) 'Package selection broadened to the workspace.'
    Assert-Development ($call.arguments[-2] -ceq '--' -and $call.arguments[-1] -ceq $filter) 'Test filter was split or passed as a Cargo option.'
    Assert-Development ($call.jobs -ceq '3' -and $call.incremental -ceq '0' -and $call.cwd -ceq $repository -and $call.target -ceq $coreTarget) 'Cargo did not receive the intended execution context.'
    Add-DevelopmentCase 'package_filter_and_unicode_arguments_are_preserved'

    $verified = Invoke-Development @{ Task = 'verify'; Workspace = 'desktop'; Offline = $true }
    Assert-Development ((@($verified.calls | ForEach-Object { $_.arguments[0] }) -join ',') -ceq 'fmt,test,clippy') 'Verify did not run its ordered checks.'
    Assert-Development (@($verified.calls | Where-Object { $_.target -cne $desktopTarget }).Count -eq 0) 'Verify leaked the caller target override.'
    Add-DevelopmentCase 'successful_checks_restore_the_calling_environment'

    $failed = Invoke-Development @{ Task = 'verify'; Offline = $true } -ExpectedExit 37 -FailStep 'test'
    Assert-Development ((@($failed.calls | ForEach-Object { $_.arguments[0] }) -join ',') -ceq 'fmt,test') 'Verify continued after a failed check.'
    Add-DevelopmentCase 'failed_checks_stop_and_restore_the_calling_environment'

    [void][IO.Directory]::CreateDirectory($coreTarget)
    $leasePath = Join-Path $coreTarget '.development.lock'
    $lease = [IO.File]::Open($leasePath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        $busy = Invoke-Development @{ Task = 'check'; Workspace = 'native' } -ExpectedExit 1
        Assert-Development (@($busy.calls).Count -eq 0 -and $busy.error -like '*already in use*') 'A busy shared target reached Cargo.'
        $readOnly = Invoke-Development @{ Task = 'check'; Workspace = 'native'; Plan = $true }
        Assert-Development (@($readOnly.calls).Count -eq 0) 'Busy-target plan invoked Cargo.'
    } finally { $lease.Dispose() }
    Add-DevelopmentCase 'busy_shared_target_refuses_before_cargo'

    foreach ($options in @(
        @{ Task = 'check'; TestFilter = 'regression' },
        @{ Task = 'test'; TestFilter = '' },
        @{ Task = 'test'; TestFilter = ' ' },
        @{ Task = 'test'; TestFilter = '--nocapture' },
        @{ Task = 'test'; TestFilter = "line`nother" }
    )) {
        $invalid = Invoke-Development $options -ExpectedExit 1
        Assert-Development (@($invalid.calls).Count -eq 0 -and $invalid.error -like '*TestFilter*') 'Invalid filter was not rejected before Cargo.'
    }
    Add-DevelopmentCase 'invalid_filters_cannot_become_cargo_options'

    $exception = Invoke-Development @{ Task = 'check' } -ExpectedExit 1 -FailStep 'throw:check'
    Assert-Development (@($exception.calls).Count -eq 1 -and $exception.error -like '*Synthetic Cargo launch failure*') 'Cargo launch exception was not observed.'
    $retry = Invoke-Development @{ Task = 'check'; Workspace = 'native' }
    Assert-Development (@($retry.calls).Count -eq 1 -and $retry.calls[0].target -ceq $coreTarget) 'A settled failure retained the target lease.'
    Add-DevelopmentCase 'cargo_launch_exceptions_restore_environment_and_release_lease'
} catch {
    Write-DevelopmentJson (Join-Path $fixture 'failure.json') @{ status = 'failed'; error = $_.Exception.Message; cases = @($cases) }
    throw
}

$report = [ordered]@{ status = 'passed'; checked_cases = $cases.Count; cases = @($cases); real_cargo_executed = $false; evidence_directory = $fixture }
Write-DevelopmentJson (Join-Path $fixture 'verification.json') $report
$report | ConvertTo-Json -Depth 6
