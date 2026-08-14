param(
    [Parameter(Mandatory = $true)]
    [string]$Artifact,
    [Parameter(Mandatory = $true)]
    [string]$Evidence
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-Sha256Hex([byte[]]$Bytes) {
    $algorithm = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($Bytes))).Replace("-", "").ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

$artifactPath = [System.IO.Path]::GetFullPath($Artifact)
$evidencePath = [System.IO.Path]::GetFullPath($Evidence)
foreach ($path in @($artifactPath, $evidencePath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Windows Authenticode verification input is missing: $path"
    }
    $item = Get-Item -LiteralPath $path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Windows Authenticode verification forbids reparse points: $path"
    }
}

$policy = Get-Content -LiteralPath $evidencePath -Raw | ConvertFrom-Json
if ($policy.schema_version -ne 1 -or
    $policy.kind -cne "ctx-windows-authenticode-signing" -or
    $policy.authority -cne "microsoft-azure-artifact-signing-public-trust-v1" -or
    $policy.account -cne "ctxsignkimmy" -or
    $policy.certificate_profile -cne "ctx-public-release" -or
    $policy.code_signing_endpoint -cne "https://eus.codesigning.azure.net/" -or
    $policy.digest_algorithm -cne "SHA256") {
    throw "Windows Authenticode evidence does not match release policy"
}

$artifactHash = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($artifactHash -cne $policy.artifact_sha256) {
    throw "Windows Authenticode evidence does not bind the exact artifact bytes"
}
$signature = Get-AuthenticodeSignature -FilePath $artifactPath
if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
    $signature.SignatureType.ToString() -cne "Authenticode" -or
    $null -eq $signature.SignerCertificate -or
    $null -eq $signature.TimeStamperCertificate) {
    throw "Windows rejected the Authenticode signature: $($signature.Status)"
}
if ($signature.SignerCertificate.GetNameInfo(
        [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName,
        $false
    ) -cne "CTX ENGINEERING, INC.") {
    throw "Windows Authenticode signer identity does not match release policy"
}
$signerHash = Get-Sha256Hex $signature.SignerCertificate.RawData
$timestampHash = Get-Sha256Hex $signature.TimeStamperCertificate.RawData
if ($signerHash -cne $policy.signer_certificate_sha256 -or
    $timestampHash -cne $policy.timestamp_certificate_sha256) {
    throw "Windows native certificate identities do not match factory evidence"
}
Write-Output "Windows Authenticode verification passed: $artifactHash"
