Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$smoke = Join-Path $repoRoot "scripts\run-native-candidate-smoke.ps1"
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-native-smoke-test-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $root | Out-Null
$savedCI = $env:CI
$env:CI = "true"
$unrelated = $null

try {
    $fake = Join-Path $root "ctx.cmd"
    @'
@echo off
if not "%CTX_ANALYTICS_ENABLED%"=="false" exit /b 91
if not "%CTX_UPGRADE_AUTO%"=="off" exit /b 92
if not "%CTX_DAEMON_AUTOSTART_OFF%"=="1" exit /b 93
if "%HOME%"=="" exit /b 94
if "%USERPROFILE%"=="" exit /b 95
if not "%CI%"=="" exit /b 97
set "CTX_FAKE_VERSION=0.25.0"
if /I "%~n0"=="ctx-v1" set "CTX_FAKE_VERSION=1.0.0"
echo %* | findstr /c:"--backend semantic" >nul
if not errorlevel 1 (
  if not "%CTX_SEARCH_SEMANTIC%"=="1" exit /b 96
  if not "%CTX_DAEMON_ENABLED%"=="true" exit /b 98
  1>&2 echo semantic-only search will not initialize or download intfloat/multilingual-e5-small during search
  exit /b 1
)
if "%1"=="--version" (
  echo ctx %CTX_FAKE_VERSION%
  exit /b 0
)
if "%1"=="setup" exit /b 0
if "%1"=="import" (
  for /L %%I in (1,1,2048) do (
    echo ordinary-stdout-%%I-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
    1>&2 echo ordinary-stderr-%%I-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
  )
  if "%CTX_FAKE_VERSION%"=="1.0.0" (
    mkdir "%CTX_DATA_ROOT%\search\lexical\ctx-generations" >nul
    mkdir "%CTX_DATA_ROOT%\search\lexical\index-generations\generation-11111111111111111111111111111111" >nul
    > "%CTX_DATA_ROOT%\search\lexical\active-generation.json" echo {"version":1,"active":{"generation_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","directory":"generation-11111111111111111111111111111111"},"previous":null}
    type nul > "%CTX_DATA_ROOT%\search\lexical\index-generations\generation-11111111111111111111111111111111\meta.json"
    type nul > "%CTX_DATA_ROOT%\search\lexical\ctx-generations\aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"
    echo {"totals":{"current_source_count":1,"current_indexed_documents":2},"sources":[{"published_generation":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}
    exit /b 0
  )
  echo {"totals":{"imported_events":2}}
  exit /b 0
)
if "%1"=="search" (
  echo {"retrieval":{"requested_mode":"lexical","effective_mode":"lexical"},"results":[{"text":"Add a parser test."}]}
  exit /b 0
)
if "%1"=="status" (
  if not "%CTX_SEARCH_SEMANTIC%"=="" exit /b 89
  if not "%CTX_DAEMON_ENABLED%"=="" exit /b 90
  echo {"read_only":true,"semantic":{"config_source":"default","enabled":false,"reason":"semantic_disabled","embed_policy":{"source":"dynamic_quiet"}}}
  exit /b 0
)
exit /b 99
'@ | Set-Content -LiteralPath $fake -Encoding Ascii

    $fixture = Join-Path $root "fixture.jsonl"
    '{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}' |
        Set-Content -LiteralPath $fixture -Encoding Ascii
    $result = Join-Path $root "result.json"
    $expectedVersionFile = Join-Path $root "expected-version"
    "0.25.0`n" | Set-Content -LiteralPath $expectedVersionFile -NoNewline -Encoding Ascii

    & $smoke -Binary $fake -Fixture $fixture -ExpectedVersionFile $expectedVersionFile -ResultPath $result | Out-Null
    if ($env:CI -ne "true") {
        throw "candidate smoke mutated parent CI"
    }
    $parsed = Get-Content -LiteralPath $result -Raw | ConvertFrom-Json
    if ($parsed.schema_version -ne 1 -or
        $parsed.kind -ne "ctx-native-candidate-smoke" -or
        $parsed.status -ne "passed") {
        throw "unexpected candidate smoke result envelope"
    }
    $topKeys = @($parsed.PSObject.Properties.Name)
    if (($topKeys -join ",") -ne "schema_version,kind,status,steps") {
        throw "candidate smoke result contains unexpected top-level keys"
    }
    $stepKeys = @($parsed.steps.PSObject.Properties.Name)
    if (($stepKeys -join ",") -ne "version,setup,import,search,read_only,semantic_offline_fail_closed") {
        throw "candidate smoke result contains unexpected step keys"
    }
    foreach ($key in $stepKeys) {
        if ($parsed.steps.$key -ne "passed") {
            throw "candidate smoke step did not pass: $key"
        }
    }

    $freshEpochFake = Join-Path $root "ctx-v1.cmd"
    Copy-Item -LiteralPath $fake -Destination $freshEpochFake
    $freshEpochResult = Join-Path $root "fresh-epoch-result.json"
    & $smoke -Binary $freshEpochFake -Fixture $fixture -ExpectedVersion 1.0.0 -ResultPath $freshEpochResult | Out-Null
    $freshEpochParsed = Get-Content -LiteralPath $freshEpochResult -Raw | ConvertFrom-Json
    if ($freshEpochParsed.status -ne "passed") {
        throw "fresh-epoch candidate smoke did not pass"
    }

    $hung = Join-Path $root "ctx-hang.cmd"
    "@echo off`r`nif defined CI exit /b 97`r`nping -n 30 127.0.0.1 >nul`r`n" |
        Set-Content -LiteralPath $hung -Encoding Ascii
    $hungResult = Join-Path $root "hung-result.json"
    $savedTimeout = $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = "1"
    $started = Get-Date
    try {
        & $smoke -Binary $hung -Fixture $fixture -ExpectedVersion 0.25.0 -ResultPath $hungResult 2>$null | Out-Null
        throw "candidate smoke accepted a hung command"
    } catch {
        if ($_.Exception.Message -notmatch
            "exceeded 1 seconds during process exit; candidate cleanup completed; final drain completed") {
            throw
        }
    } finally {
        $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = $savedTimeout
    }
    if (((Get-Date) - $started).TotalSeconds -ge 10) {
        throw "candidate smoke timeout was not bounded"
    }
    if (Test-Path -LiteralPath $hungResult) {
        throw "candidate smoke wrote evidence after a hung command"
    }

    $pipeOwner = Join-Path $root "ctx-pipe-owner.exe"
    $pipeOwnerSource = @'
using System;
using System.Diagnostics;
using System.IO;
using System.Threading;

public static class CtxPipeOwner {
    public static int Main(string[] args) {
        string mode = args.Length == 0 ? "" : args[0];
        if (mode == "--unrelated") {
            Thread.Sleep(30000);
            return 0;
        }
        if (mode == "--pipe-owner") {
            string pidPath = Environment.GetEnvironmentVariable("CTX_NATIVE_CANDIDATE_TEST_PIPE_OWNER_PID");
            if (!String.IsNullOrEmpty(pidPath)) {
                File.WriteAllText(pidPath, Process.GetCurrentProcess().Id.ToString());
            }
            Thread.Sleep(30000);
            return 0;
        }
        if (mode == "--version") {
            ProcessStartInfo start = new ProcessStartInfo(
                Process.GetCurrentProcess().MainModule.FileName,
                "--pipe-owner");
            start.UseShellExecute = false;
            Process.Start(start);
            Console.WriteLine("ctx 0.25.0");
            return 0;
        }
        return 99;
    }
}
'@
    Add-Type -TypeDefinition $pipeOwnerSource -Language CSharp `
        -OutputAssembly $pipeOwner -OutputType ConsoleApplication

    # This process has the exact candidate image but predates the harness
    # baseline. Cleanup must leave it alone while killing the later pipe owner.
    $unrelated = Start-Process -FilePath $pipeOwner -ArgumentList "--unrelated" -PassThru
    Start-Sleep -Milliseconds 200
    if ($unrelated.HasExited) {
        throw "unrelated candidate fixture exited before the timeout test"
    }

    $pipeOwnerPidPath = Join-Path $root "pipe-owner.pid"
    $pipeOwnerResult = Join-Path $root "pipe-owner-result.json"
    $savedPipeOwnerPidPath = $env:CTX_NATIVE_CANDIDATE_TEST_PIPE_OWNER_PID
    $savedTimeout = $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS
    $env:CTX_NATIVE_CANDIDATE_TEST_PIPE_OWNER_PID = $pipeOwnerPidPath
    $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = "1"
    $started = Get-Date
    try {
        & $smoke -Binary $pipeOwner -Fixture $fixture -ExpectedVersion 0.25.0 `
            -ResultPath $pipeOwnerResult 2>$null | Out-Null
        throw "candidate smoke accepted a stuck redirected stream"
    } catch {
        if ($_.Exception.Message -notmatch
            "exceeded 1 seconds during stdout/stderr drain after process exit; candidate cleanup completed; final drain completed") {
            throw
        }
    } finally {
        $env:CTX_NATIVE_CANDIDATE_TEST_PIPE_OWNER_PID = $savedPipeOwnerPidPath
        $env:CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS = $savedTimeout
    }
    if (((Get-Date) - $started).TotalSeconds -ge 10) {
        throw "candidate smoke redirected-stream timeout was not bounded"
    }
    if (-not (Test-Path -LiteralPath $pipeOwnerPidPath -PathType Leaf)) {
        throw "candidate smoke fixture did not create the redirected pipe owner"
    }
    $pipeOwnerPid = [int](Get-Content -LiteralPath $pipeOwnerPidPath -Raw)
    if ($null -ne (Get-Process -Id $pipeOwnerPid -ErrorAction SilentlyContinue)) {
        throw "candidate smoke left the redirected pipe owner running"
    }
    if ($unrelated.HasExited) {
        throw "candidate smoke killed a candidate process that predated its baseline"
    }
    if (Test-Path -LiteralPath $pipeOwnerResult) {
        throw "candidate smoke wrote evidence after a stuck redirected stream"
    }

    Write-Host "Windows native candidate smoke tests passed"
} finally {
    if ($null -ne $unrelated -and -not $unrelated.HasExited) {
        Stop-Process -Id $unrelated.Id -Force -ErrorAction SilentlyContinue
        [void]$unrelated.WaitForExit(5000)
    }
    if ($null -ne $unrelated) {
        $unrelated.Dispose()
    }
    $env:CI = $savedCI
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
