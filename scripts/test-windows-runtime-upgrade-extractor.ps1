param(
    [string]$RuntimeArchive = "target/public-cli-artifacts/ctx-onnxruntime-windows-x64.zip"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This contract test must run on Windows"
}
if (
    $PSVersionTable.PSVersion.Major -ne 5 -or
    $PSVersionTable.PSVersion.Minor -ne 1
) {
    throw "This contract test requires Windows PowerShell 5.1"
}

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Get-EmbeddedScript {
    param(
        [string]$SourcePath,
        [string]$ConstantName
    )

    $source = [System.IO.File]::ReadAllText((Resolve-Path -LiteralPath $SourcePath).Path)
    $pattern = 'const ' + [regex]::Escape($ConstantName) + ': &str = r#"\r?\n(?<script>.*?)\r?\n"#;'
    $scriptMatches = [regex]::Matches(
        $source,
        $pattern,
        [System.Text.RegularExpressions.RegexOptions]::Singleline
    )
    if ($scriptMatches.Count -ne 1) {
        throw "Expected exactly one embedded $ConstantName script in $SourcePath"
    }
    return $scriptMatches[0].Groups["script"].Value
}

function Invoke-EmbeddedExtractorProcess {
    param(
        [string]$PowerShellPath,
        [string]$ScriptPath,
        [string[]]$ArgumentList
    )

    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $outputLines = @(
            & $PowerShellPath -NoProfile -ExecutionPolicy Bypass -File $ScriptPath @ArgumentList 2>&1
        )
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        Output = $outputLines -join [Environment]::NewLine
    }
}

function Assert-EmbeddedExtractorSuccess {
    param(
        [string]$PowerShellPath,
        [string]$ScriptPath,
        [string[]]$ArgumentList,
        [string]$Label
    )

    $result = Invoke-EmbeddedExtractorProcess $PowerShellPath $ScriptPath $ArgumentList
    if ($result.ExitCode -ne 0) {
        throw "$Label failed with status $($result.ExitCode)`n$($result.Output)"
    }
}

function Assert-EmbeddedExtractorFailure {
    param(
        [string]$PowerShellPath,
        [string]$ScriptPath,
        [string[]]$ArgumentList,
        [string]$ExpectedText,
        [string]$Label
    )

    $result = Invoke-EmbeddedExtractorProcess $PowerShellPath $ScriptPath $ArgumentList
    if ($result.ExitCode -eq 0) {
        throw "$Label unexpectedly succeeded"
    }
    if (-not $result.Output.Contains($ExpectedText)) {
        throw "$Label did not report '$ExpectedText'`n$($result.Output)"
    }
}

function Get-SignedArchiveRecords {
    param(
        [string]$ArchivePath,
        [string[]]$ExpectedFiles
    )

    $archiveStream = [System.IO.FileStream]::new(
        $ArchivePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::ReadWrite
    )
    $archive = $null
    $records = [System.Collections.Generic.List[object]]::new()
    try {
        $archive = [System.IO.Compression.ZipArchive]::new(
            $archiveStream,
            [System.IO.Compression.ZipArchiveMode]::Read,
            $true
        )
        foreach ($relativePath in $ExpectedFiles) {
            $entry = $archive.GetEntry($relativePath)
            if ($null -eq $entry) {
                throw "Runtime archive entry is missing: $relativePath"
            }
            $entryStream = $entry.Open()
            $sha256 = [System.Security.Cryptography.SHA256]::Create()
            try {
                $entryHash = [System.BitConverter]::ToString($sha256.ComputeHash($entryStream)).Replace("-", "").ToLowerInvariant()
            } finally {
                $sha256.Dispose()
                $entryStream.Dispose()
            }
            $records.Add([pscustomobject][ordered]@{
                path = $relativePath
                size = [long]$entry.Length
                sha256 = $entryHash
            })
        }
    } finally {
        try {
            if ($null -ne $archive) {
                $archive.Dispose()
            }
        } finally {
            $archiveStream.Dispose()
        }
    }
    return $records.ToArray()
}

function Write-SemanticContract {
    param(
        [string]$Path,
        [object[]]$Records,
        [long]$MaxExpandedBytes
    )

    $contract = [ordered]@{
        prefix = ""
        max_expanded_bytes = $MaxExpandedBytes
        files = @($Records)
    }
    [System.IO.File]::WriteAllText(
        $Path,
        ($contract | ConvertTo-Json -Depth 6 -Compress),
        [System.Text.UTF8Encoding]::new($false)
    )
}

function Assert-SignedTree {
    param(
        [string]$Destination,
        [object[]]$Records,
        [string]$Label
    )

    $expectedFiles = @($Records | ForEach-Object { [string]$_.path } | Sort-Object)
    $actualFiles = @(
        Get-ChildItem -LiteralPath $Destination -Recurse -File | ForEach-Object {
            $_.FullName.Substring($Destination.Length + 1).Replace("\", "/")
        } | Sort-Object
    )
    if ($actualFiles.Count -ne $expectedFiles.Count) {
        throw "$Label produced $($actualFiles.Count) files, expected $($expectedFiles.Count)"
    }
    for ($index = 0; $index -lt $expectedFiles.Count; $index++) {
        if ($actualFiles[$index] -cne $expectedFiles[$index]) {
            throw "$Label file $index was '$($actualFiles[$index])', expected '$($expectedFiles[$index])'"
        }
    }
    foreach ($record in $Records) {
        $target = Join-Path $Destination ([string]$record.path).Replace("/", "\")
        $metadata = Get-Item -LiteralPath $target
        if ([long]$metadata.Length -ne [long]$record.size) {
            throw "$Label file size does not match its signed record: $($record.path)"
        }
        $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $target).Hash.ToLowerInvariant()
        if ($actualHash -cne [string]$record.sha256) {
            throw "$Label file hash does not match its signed record: $($record.path)"
        }
    }
}

$runtimeArchivePath = (Resolve-Path -LiteralPath $RuntimeArchive).Path
$installerSource = Join-Path $PSScriptRoot "..\crates\ctx-upgrade-engine\src\upgrade\install.rs"
$archiveSource = Join-Path $PSScriptRoot "..\crates\ctx-upgrade-engine\src\upgrade\install\archive.rs"
$runtimeScript = Get-EmbeddedScript $installerSource "EXTRACT_SCRIPT"
$semanticScript = Get-EmbeddedScript $archiveSource "WINDOWS_SEMANTIC_ZIP_EXTRACT_SCRIPT"
$windowsPowerShell = Join-Path $PSHOME "powershell.exe"
$expectedFiles = @(
    "GIT_COMMIT_ID",
    "LICENSE",
    "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "lib/msvcp140.dll",
    "lib/msvcp140_1.dll",
    "lib/onnxruntime.dll",
    "lib/vcruntime140.dll",
    "lib/vcruntime140_1.dll"
) | Sort-Object

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("ctx-upgrade-extractor-" + [Guid]::NewGuid().ToString("n"))
$archivePath = Join-Path $root "runtime.zip"
$runtimeDestination = Join-Path $root "runtime"
$semanticDestination = Join-Path $root "semantic"
$hashFailureDestination = Join-Path $root "semantic-hash-failure"
$fileSetFailureDestination = Join-Path $root "semantic-file-set-failure"
$runtimeExtractor = Join-Path $root "runtime-extract.ps1"
$semanticExtractor = Join-Path $root "semantic-extract.ps1"
$semanticContractPath = Join-Path $root "semantic-contract.json"
$hashFailureContractPath = Join-Path $root "semantic-hash-failure.json"
$fileSetFailureContractPath = Join-Path $root "semantic-file-set-failure.json"
$parentArchive = $null

New-Item -ItemType Directory -Path $root -Force | Out-Null
try {
    Copy-Item -LiteralPath $runtimeArchivePath -Destination $archivePath
    [System.IO.File]::WriteAllText(
        $runtimeExtractor,
        $runtimeScript,
        [System.Text.UTF8Encoding]::new($false)
    )
    [System.IO.File]::WriteAllText(
        $semanticExtractor,
        $semanticScript,
        [System.Text.UTF8Encoding]::new($false)
    )

    $signedRecords = @(Get-SignedArchiveRecords $archivePath $expectedFiles)
    $maxExpandedBytes = [long](($signedRecords | Measure-Object -Property size -Sum).Sum)
    Write-SemanticContract $semanticContractPath $signedRecords $maxExpandedBytes

    $hashFailureRecords = @()
    for ($index = 0; $index -lt $signedRecords.Count; $index++) {
        $record = $signedRecords[$index]
        $recordHash = if ($index -eq 0) { (("0" * 64) -join "") } else { [string]$record.sha256 }
        $hashFailureRecords += [pscustomobject][ordered]@{
            path = [string]$record.path
            size = [long]$record.size
            sha256 = $recordHash
        }
    }
    Write-SemanticContract $hashFailureContractPath $hashFailureRecords $maxExpandedBytes

    $fileSetFailureRecords = @($signedRecords | Select-Object -First ($signedRecords.Count - 1))
    Write-SemanticContract $fileSetFailureContractPath $fileSetFailureRecords $maxExpandedBytes

    foreach ($destination in @(
        $runtimeDestination,
        $semanticDestination,
        $hashFailureDestination,
        $fileSetFailureDestination
    )) {
        New-Item -ItemType Directory -Path $destination -Force | Out-Null
    }

    # Exercise the publisher/identity-pin sharing contract with read/write
    # access and read sharing, deliberately excluding write and delete
    # sharing. Every child extraction below runs while this handle remains live.
    $parentArchive = [System.IO.FileStream]::new(
        $archivePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::ReadWrite,
        [System.IO.FileShare]::Read
    )
    try {
        Assert-EmbeddedExtractorSuccess `
            $windowsPowerShell `
            $runtimeExtractor `
            @(
                "-ArchivePath", $archivePath,
                "-Destination", $runtimeDestination,
                "-ExpectedVersion", "1.27.0",
                "-MaxExpandedBytes", "1073741824"
            ) `
            "Embedded Windows runtime extractor"
        Assert-SignedTree $runtimeDestination $signedRecords "Runtime extractor"

        Assert-EmbeddedExtractorSuccess `
            $windowsPowerShell `
            $semanticExtractor `
            @(
                "-ArchivePath", $archivePath,
                "-Destination", $semanticDestination,
                "-ContractPath", $semanticContractPath
            ) `
            "Embedded Windows Semantic zip extractor"
        Assert-SignedTree $semanticDestination $signedRecords "Semantic extractor"

        Assert-EmbeddedExtractorFailure `
            $windowsPowerShell `
            $semanticExtractor `
            @(
                "-ArchivePath", $archivePath,
                "-Destination", $hashFailureDestination,
                "-ContractPath", $hashFailureContractPath
            ) `
            "Semantic zip file verification failed" `
            "Semantic hash contract"
        Remove-Item -LiteralPath $hashFailureDestination -Recurse -Force -ErrorAction Stop

        Assert-EmbeddedExtractorFailure `
            $windowsPowerShell `
            $semanticExtractor `
            @(
                "-ArchivePath", $archivePath,
                "-Destination", $fileSetFailureDestination,
                "-ContractPath", $fileSetFailureContractPath
            ) `
            "unexpected or non-regular Semantic zip file" `
            "Semantic file-set contract"
        Remove-Item -LiteralPath $fileSetFailureDestination -Recurse -Force -ErrorAction Stop

        foreach ($temporaryHelper in @(
            $runtimeExtractor,
            $semanticExtractor,
            $semanticContractPath,
            $hashFailureContractPath,
            $fileSetFailureContractPath
        )) {
            Remove-Item -LiteralPath $temporaryHelper -Force -ErrorAction Stop
            if (Test-Path -LiteralPath $temporaryHelper) {
                throw "Temporary extractor input was retained: $temporaryHelper"
            }
        }
    } finally {
        $parentArchive.Dispose()
        $parentArchive = $null
    }

    # No child or verifier may retain the archive once the identity pin closes.
    $exclusiveArchive = [System.IO.FileStream]::new(
        $archivePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::ReadWrite,
        [System.IO.FileShare]::None
    )
    try {
        $exclusiveArchive.Flush($true)
    } finally {
        $exclusiveArchive.Dispose()
    }
    Remove-Item -LiteralPath $archivePath -Force -ErrorAction Stop
    if (Test-Path -LiteralPath $archivePath) {
        throw "Extractor archive remained after every handle was disposed"
    }
} finally {
    if ($null -ne $parentArchive) {
        $parentArchive.Dispose()
    }
    if (Test-Path -LiteralPath $root) {
        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction Stop
    }
}

if (Test-Path -LiteralPath $root) {
    throw "Windows extractor contract retained its temporary root"
}
Write-Host "Windows runtime and Semantic upgrade extractor contracts passed under PowerShell 5.1"
