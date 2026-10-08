# FlareDispatch binds this reviewed-head receipt to the API job's fixed executor revision and conclusion.
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$out = Join-Path $env:GITHUB_WORKSPACE 'native-output'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$exitCode = 1
$failure = $null
$request = $null
$command = @()
$started = [DateTimeOffset]::UtcNow.ToString('o')
try {
    $request = $env:NATIVE_REQUEST | ConvertFrom-Json
    $keys = @($request.PSObject.Properties.Name | Sort-Object)
    $expected = @('base','command_sha256','executor_ref','head','mode','nonce','profile','repo','target')
    if (($keys -join ',') -cne ($expected -join ',')) { throw 'NativeRequestFields' }
    foreach ($key in @('base','head','executor_ref')) {
        if ($request.$key -cnotmatch '^[0-9a-f]{40}$') { throw 'NativeRevisionInvalid' }
    }
    if ($request.nonce -cnotmatch '^[a-z0-9-]{16,64}$' -or $request.command_sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'NativeBindingInvalid' }
    if ($request.repo -cne $env:GITHUB_REPOSITORY -or $request.executor_ref -cne $env:GITHUB_SHA) { throw 'NativeExecutorMismatch' }
    if ($env:RUNNER_OS -cne 'Windows' -or $request.target -cne $env:NATIVE_TARGET) { throw 'NativeTargetMismatch' }
    $arch = switch ($request.target) {
        'x86_64-pc-windows-msvc' { 'X64' }
        'aarch64-pc-windows-msvc' { 'ARM64' }
        default { throw 'NativeTargetInvalid' }
    }
    if ($env:RUNNER_ARCH -cne $arch) { throw 'NativeRunnerArchitectureMismatch' }
    $executor = & git -C (Join-Path $env:GITHUB_WORKSPACE 'executor') rev-parse HEAD
    $head = & git -C (Join-Path $env:GITHUB_WORKSPACE 'workload') rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $executor.Trim() -cne $request.executor_ref -or $head.Trim() -cne $request.head) { throw 'NativeCheckoutMismatch' }
    Set-Location (Join-Path $env:GITHUB_WORKSPACE 'workload')
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    & rustup toolchain install 1.97.0 --profile minimal
    if ($LASTEXITCODE -ne 0) { throw 'NativeToolchainUnavailable' }
    $compilerHost = (& rustc +1.97.0 -vV | Select-String '^host: ').ToString().Substring(6)
    if ($LASTEXITCODE -ne 0 -or $compilerHost -cne $request.target) { throw 'NativeCompilerHostMismatch' }
    $nativeArgs = @('+1.97.0','run','--locked','-q','-p','contextful-ci','--')
    if ($request.mode -ceq 'gate' -and $request.profile -ceq '') {
        $part = if ($arch -ceq 'X64') { 'windows.x86_64-msvc' } else { 'windows.aarch64-msvc' }
        $nativeArgs += @('gate','--stage',$part,'--base',$request.base)
    } elseif ($request.mode -ceq 'release' -and $request.profile -cin @('contextful-edge','contextful-full')) {
        $nativeArgs += @('release','--target',$request.target,'--profile',$request.profile,'--out','../native-output/dist')
    } else { throw 'NativeModeInvalid' }
    $command = @('cargo') + $nativeArgs
    $bytes = [Text.Encoding]::UTF8.GetBytes((ConvertTo-Json -InputObject $command -Compress))
    $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
    if ($digest -cne $request.command_sha256) { throw 'NativeCommandMismatch' }
    & cargo @nativeArgs 2>&1 | Tee-Object -FilePath (Join-Path $out 'command.log')
    $exitCode = $LASTEXITCODE
    if ($null -eq $exitCode) { throw 'NativeExitMissing' }
} catch {
    $failure = $_.Exception.Message
    $exitCode = 1
} finally {
    $artifacts = @(Get-ChildItem -LiteralPath $out -File -Recurse | Where-Object { $_.FullName -ne (Join-Path $out 'receipt.json') } | ForEach-Object {
        @{ path = [IO.Path]::GetRelativePath($out, $_.FullName).Replace('\','/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = $_.Length }
    })
    $receipt = @{
        version = 1; repo = $env:GITHUB_REPOSITORY; head = $request.head; base = $request.base;
        nonce = $request.nonce; executor_ref = $env:GITHUB_SHA; command_sha256 = $request.command_sha256;
        command = $command; target = $env:NATIVE_TARGET; runner_os = $env:RUNNER_OS; runner_arch = $env:RUNNER_ARCH;
        run_id = $env:GITHUB_RUN_ID; run_attempt = $env:GITHUB_RUN_ATTEMPT; job = $env:GITHUB_JOB;
        started_at = $started; completed_at = [DateTimeOffset]::UtcNow.ToString('o');
        exit_code = $exitCode; failure = $failure; artifacts = $artifacts
    }
    $receipt | ConvertTo-Json -Depth 12 | Set-Content -Encoding utf8 -LiteralPath (Join-Path $out 'receipt.json')
}
exit $exitCode
