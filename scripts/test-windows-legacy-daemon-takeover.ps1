[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Candidate,
    [switch]$KeepRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'This contract test must run on Windows'
}

$script:V025Url = 'https://github.com/ctxrs/ctx/releases/download/v0.25.0/ctx-windows-x64.exe'
$script:V025Sha256 = '32aa550cc5c56d4d2989d0f929bbc1e634d8b730219feb8e4a4ba770b02a9867'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class CtxLegacyProcessImageQuery {
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr OpenProcess(uint access, bool inherit, uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool QueryFullProcessImageName(
        IntPtr process, uint flags, StringBuilder path, ref uint size);

    [DllImport("kernel32.dll")]
    public static extern bool CloseHandle(IntPtr handle);
}
'@

function Get-Sha256([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-QueryFullProcessImageName([uint32]$ProcessId) {
    $handle = [CtxLegacyProcessImageQuery]::OpenProcess(0x1000, $false, $ProcessId)
    if ($handle -eq [IntPtr]::Zero) {
        throw "OpenProcess failed for ${ProcessId}: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    try {
        $path = New-Object Text.StringBuilder 32768
        [uint32]$length = $path.Capacity
        if (-not [CtxLegacyProcessImageQuery]::QueryFullProcessImageName(
            $handle, 0, $path, [ref]$length
        )) {
            throw "QueryFullProcessImageNameW failed for ${ProcessId}: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
        }
        return $path.ToString()
    } finally {
        [CtxLegacyProcessImageQuery]::CloseHandle($handle) | Out-Null
    }
}

function Wait-For {
    param(
        [string]$Description,
        [int]$Seconds,
        [scriptblock]$Condition
    )
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $value = & $Condition
        if ($value) {
            return $value
        }
        Start-Sleep -Milliseconds 25
    }
    throw "timed out waiting for $Description"
}

function Use-Environment {
    param(
        [System.Collections.IDictionary]$Values,
        [scriptblock]$Body
    )
    $prior = @{}
    foreach ($entry in $Values.GetEnumerator()) {
        $prior[$entry.Key] = [Environment]::GetEnvironmentVariable($entry.Key, 'Process')
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
    try {
        & $Body
    } finally {
        foreach ($entry in $prior.GetEnumerator()) {
            [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
        }
    }
}

function Invoke-Captured {
    param(
        [string]$Executable,
        [string[]]$Arguments,
        [string]$Root,
        [string]$Label
    )
    $stdout = Join-Path $Root ("stdout-{0}.txt" -f [Guid]::NewGuid().ToString('N'))
    $stderr = Join-Path $Root ("stderr-{0}.txt" -f [Guid]::NewGuid().ToString('N'))
    try {
        $priorErrorActionPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            & $Executable @Arguments 1> $stdout 2> $stderr
            $exitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $priorErrorActionPreference
        }
        $out = if (Test-Path -LiteralPath $stdout) {
            [IO.File]::ReadAllText($stdout).Trim()
        } else { '' }
        $err = if (Test-Path -LiteralPath $stderr) {
            [IO.File]::ReadAllText($stderr).Trim()
        } else { '' }
        if ($exitCode -ne 0) {
            throw "$Label failed with status ${exitCode}: $err $out"
        }
        return [ordered]@{ stdout = $out; stderr = $err; exit_code = $exitCode }
    } finally {
        Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-CtxJson {
    param(
        [string]$Executable,
        [string[]]$Arguments,
        [string]$Root,
        [string]$Label
    )
    $run = Invoke-Captured -Executable $Executable -Arguments $Arguments -Root $Root -Label $Label
    try {
        return $run.stdout | ConvertFrom-Json
    } catch {
        throw "$Label did not return JSON: $($run.stdout)"
    }
}

function Assert-PrepareReceipt([object]$Receipt, [string]$Requested, [string]$Canonical) {
    if ($Receipt.schema_version -ne 1 -or
        $Receipt.command -cne 'daemon_prepare_uninstall' -or
        $Receipt.ok -ne $true -or
        $Receipt.scope -cne 'installation' -or
        $Receipt.installation_quiescent -ne $true -or
        $Receipt.daemon_running -ne $false -or
        $Receipt.owner_lock_released -ne $true -or
        $Receipt.supervisor_removed -ne $true -or
        $Receipt.coordination_state_removed -ne $true -or
        $Receipt.binary_retained -ne $true -or
        $Receipt.retry_safe -ne $true) {
        throw 'prepare-uninstall receipt did not prove installation quiescence'
    }
    $roots = @($Receipt.quiesced_roots | ForEach-Object { [IO.Path]::GetFullPath([string]$_) })
    foreach ($expected in @($Requested, $Canonical)) {
        if ($roots -cnotcontains [IO.Path]::GetFullPath($expected)) {
            throw "prepare-uninstall receipt omitted root: $expected"
        }
    }
    if ([int]$Receipt.quiesced_root_count -ne $roots.Count) {
        throw 'prepare-uninstall receipt root count is inconsistent'
    }
}

$candidatePath = (Resolve-Path -LiteralPath $Candidate).Path
$candidateSha256 = Get-Sha256 -Path $candidatePath
$runRoot = Join-Path ([IO.Path]::GetTempPath()) (
    'ctx-legacy-daemon-takeover-' + [Guid]::NewGuid().ToString('N')
)
$profileRoot = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
$roamingRoot = [Environment]::GetFolderPath([Environment+SpecialFolder]::ApplicationData)
$localRoot = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
foreach ($knownRoot in @($profileRoot, $roamingRoot, $localRoot)) {
    if ([string]::IsNullOrWhiteSpace($knownRoot) -or
        -not (Test-Path -LiteralPath $knownRoot -PathType Container)) {
        throw "Windows known-folder profile is unavailable: $knownRoot"
    }
}
$binRoot = Join-Path $runRoot 'bin'
$dataRoot = Join-Path $runRoot 'requested-data'
$canonicalRoot = Join-Path $profileRoot '.ctx'
$tempRoot = Join-Path $runRoot 'temp'
$active = Join-Path $binRoot 'ctx.exe'
$runningImage = Join-Path $binRoot 'ctx.v025-running.exe'
$download = Join-Path $runRoot 'ctx-v0.25.0-windows-x64.exe'
$daemonStdout = Join-Path $runRoot 'daemon.stdout'
$daemonStderr = Join-Path $runRoot 'daemon.stderr'
$daemon = $null
$completed = $false
$canonicalRootCreated = $false

foreach ($path in @(
    $runRoot, $binRoot, $dataRoot, $tempRoot
)) {
    New-Item -ItemType Directory -Path $path -Force | Out-Null
}

$environment = [ordered]@{
    TEMP = $tempRoot
    TMP = $tempRoot
    CTX_DATA_ROOT = $dataRoot
    CTX_ANALYTICS_ENABLED = 'false'
    CTX_LOCAL_USAGE_ENABLED = 'false'
    CTX_UPGRADE_OFF = '1'
    CTX_DISABLE_AUTO_UPGRADE = '1'
    CI = $null
    CTX_DAEMON_AUTOSTART_OFF = $null
    CTX_DAEMON_BACKGROUND_CHILD = $null
}

try {
    if (Test-Path -LiteralPath $canonicalRoot) {
        throw "isolated Windows profile already contains canonical ctx state: $canonicalRoot"
    }
    New-Item -ItemType Directory -Path $canonicalRoot | Out-Null
    $canonicalRootCreated = $true
    Invoke-WebRequest -UseBasicParsing -Uri $script:V025Url -OutFile $download
    $oldSha256 = Get-Sha256 -Path $download
    if ($oldSha256 -cne $script:V025Sha256) {
        throw "official v0.25.0 SHA-256 mismatch: $oldSha256"
    }
    Copy-Item -LiteralPath $download -Destination $active

    Use-Environment -Values $environment -Body {
        $script:daemon = Start-Process -FilePath $active -ArgumentList @(
            '--data-root', $dataRoot, 'daemon', 'run', '--foreground', '--force',
            '--loop-interval-seconds', '300', '--json'
        ) -WindowStyle Hidden -PassThru -RedirectStandardOutput $daemonStdout `
            -RedirectStandardError $daemonStderr

        $lockPath = Join-Path $dataRoot 'daemon\daemon.lock'
        $legacyLock = Wait-For -Description 'exact live v0.25 advisory lock' -Seconds 30 -Condition {
            if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf)) {
                return $false
            }
            try {
                $lock = [IO.File]::ReadAllText($lockPath) | ConvertFrom-Json
                $names = @($lock.PSObject.Properties.Name | Sort-Object)
                $expected = @(
                    'binary', 'data_root', 'lock_protocol', 'owner_id', 'pid', 'released',
                    'started_at_ms'
                ) | Sort-Object
                if (($names -join "`n") -cne ($expected -join "`n") -or
                    $lock.lock_protocol -cne 'advisory-v1' -or
                    $lock.released -ne $false -or
                    [uint32]$lock.pid -ne [uint32]$script:daemon.Id -or
                    [string]::IsNullOrWhiteSpace([string]$lock.owner_id)) {
                    return $false
                }
                return $lock
            } catch {
                return $false
            }
        }

        Move-Item -LiteralPath $active -Destination $runningImage
        Copy-Item -LiteralPath $candidatePath -Destination $active
        if ((Get-Sha256 -Path $active) -cne $candidateSha256) {
            throw 'same-path candidate copy changed bytes'
        }
        $queryProcessPath = Get-QueryFullProcessImageName -ProcessId $script:daemon.Id
        if ([IO.Path]::GetFullPath($queryProcessPath) -cne [IO.Path]::GetFullPath($runningImage)) {
            throw "QueryFullProcessImageNameW did not report the renamed old image: $queryProcessPath"
        }
        $processPath = [string](Get-CimInstance Win32_Process -Filter (
            "ProcessId = {0}" -f $script:daemon.Id
        )).ExecutablePath
        if ([IO.Path]::GetFullPath($processPath) -cne [IO.Path]::GetFullPath($active)) {
            throw "Windows did not report the replaced same path for the old daemon: $processPath"
        }

        $arguments = @(
            '--data-root', $dataRoot, 'daemon', 'disable', '--prepare-uninstall',
            '--format=json'
        )
        $first = Invoke-CtxJson -Executable $active -Arguments $arguments -Root $runRoot `
            -Label 'first legacy prepare-uninstall'
        Assert-PrepareReceipt -Receipt $first -Requested $dataRoot -Canonical $canonicalRoot

        $script:daemon.Refresh()
        if (-not $script:daemon.HasExited) {
            throw "legacy daemon remained live after first prepare-uninstall receipt: $($script:daemon.Id)"
        }
        if (Get-Process -Id $script:daemon.Id -ErrorAction SilentlyContinue) {
            throw "legacy daemon PID remained live after first prepare-uninstall receipt: $($script:daemon.Id)"
        }

        $retry = Invoke-CtxJson -Executable $active -Arguments $arguments -Root $runRoot `
            -Label 'immediate legacy prepare-uninstall retry'
        Assert-PrepareReceipt -Receipt $retry -Requested $dataRoot -Canonical $canonicalRoot
        foreach ($root in @($dataRoot, $canonicalRoot)) {
            foreach ($relative in @(
                'daemon\daemon.lock', 'daemon\daemon.guard',
                'daemon\lifecycle-control.lock', 'daemon\lifecycle-transition.lock',
                'daemon\supervisor.json'
            )) {
                $residual = Join-Path $root $relative
                if (Test-Path -LiteralPath $residual) {
                    throw "prepare-uninstall retained daemon coordination: $residual"
                }
            }
        }

        [ordered]@{
            schema_version = 1
            kind = 'ctx-windows-legacy-daemon-takeover-proof'
            status = 'passed'
            official_v025_sha256 = $oldSha256
            candidate_sha256 = $candidateSha256
            legacy_pid = [uint32]$legacyLock.pid
            legacy_lock_protocol = [string]$legacyLock.lock_protocol
            legacy_lock_released = [bool]$legacyLock.released
            legacy_lock_has_binary_sha256 = $false
            cim_original_path_observed = $true
            query_full_process_image_renamed_path = $queryProcessPath
            first_prepare_uninstall = $first
            immediate_retry = $retry
            coordination_residuals = 0
        } | ConvertTo-Json -Depth 8 -Compress
    }
    $completed = $true
} finally {
    if ($null -ne $daemon) {
        try {
            $daemon.Refresh()
            if (-not $daemon.HasExited) {
                Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
                $daemon.WaitForExit(5000) | Out-Null
            }
        } catch {}
        $daemon.Dispose()
    }
    if ($canonicalRootCreated) {
        Remove-Item -LiteralPath $canonicalRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (-not $KeepRoot -and ($completed -or (Test-Path -LiteralPath $runRoot))) {
        Remove-Item -LiteralPath $runRoot -Recurse -Force -ErrorAction SilentlyContinue
    } elseif (Test-Path -LiteralPath $runRoot) {
        Write-Warning "retained legacy takeover root: $runRoot"
    }
}
