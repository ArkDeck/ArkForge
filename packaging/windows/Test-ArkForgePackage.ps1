[CmdletBinding()]
param(
    [string]$InstallRoot = (Join-Path $env:ProgramFiles 'ArkForge'),
    [switch]$SkipDevice,
    [System.Management.Automation.PSCredential]$DeniedCredential,
    [switch]$SkipCrossAccount,
    [string]$EvidencePath = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = Join-Path $InstallRoot 'current'
$manifest = Get-Content -LiteralPath (Join-Path $root 'arkforge-runtime.json') -Raw | ConvertFrom-Json
$receipt = Get-Content -LiteralPath (Join-Path $root 'package-receipt.json') -Raw | ConvertFrom-Json
$installReceipt = Get-Content -LiteralPath (Join-Path $root 'install-receipt.json') -Raw | ConvertFrom-Json
$trustedManifestPath = Join-Path $root 'ArkForge.PackageManifest.ps1'
$selfSignature = Get-AuthenticodeSignature -LiteralPath $MyInvocation.MyCommand.Path
$trustedSignature = Get-AuthenticodeSignature -LiteralPath $trustedManifestPath
if ($selfSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
    $trustedSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
    $selfSignature.SignerCertificate.Thumbprint -ine $trustedSignature.SignerCertificate.Thumbprint) {
    throw 'Acceptance script and trusted manifest do not share one trusted release identity.'
}
$trustedManifest = & $trustedManifestPath
if ($manifest.schema -ne 'arkforge.windows-runtime/v1' -or
    $receipt.schema -ne 'arkforge.windows-package-receipt/v1' -or
    $installReceipt.schema -ne 'arkforge.windows-install-receipt/v1' -or
    $trustedManifest.schema -ne 'arkforge.windows-trusted-manifest/v1') {
    throw 'One or more Windows package schemas are unsupported.'
}
foreach ($fact in $trustedManifest.files) {
    $path = Join-Path $root ($fact.path.Replace('/', '\'))
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $fact.sha256) {
        throw "Installed digest mismatch for $($fact.path): $actual"
    }
}
foreach ($relative in @($manifest.arkforge, $manifest.arkforged, $manifest.hdc.path, $manifest.driver.catalog)) {
    $path = Join-Path $root ($relative.Replace('/', '\'))
    $signature = Get-AuthenticodeSignature -LiteralPath $path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode is not trusted for $relative ($($signature.Status))."
    }
    if ($relative -ne $manifest.driver.catalog -and
        $signature.SignerCertificate.Thumbprint -ine $trustedManifest.certificateThumbprint) {
        throw "Release signer mismatch for ${relative}: $($signature.SignerCertificate.Thumbprint)"
    }
}

$published = Get-WindowsDriver -Online | Where-Object { $_.Driver -eq $installReceipt.publishedDriver }
if ($null -eq $published) {
    throw "Published driver $($installReceipt.publishedDriver) is missing."
}
$device = $null
$deviceInstanceDigest = ''
if (-not $SkipDevice) {
    $devices = @(Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'USB\VID_2207&PID_350A*' })
    if ($devices.Count -ne 1) {
        throw "Expected exactly one DAYU200 in Loader mode, observed $($devices.Count); use -SkipDevice only for software acceptance."
    }
    $device = $devices[0]
    $service = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_Service').Data
    if ($service -ine 'WinUSB') {
        throw "DAYU200 Loader is bound to $service, not WinUSB."
    }
    $instanceBytes = [Text.Encoding]::UTF8.GetBytes($device.InstanceId)
    $deviceInstanceDigest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($instanceBytes)).ToLowerInvariant()
}

$runtime = Join-Path $env:LOCALAPPDATA ('ArkForge-Acceptance-' + [guid]::NewGuid().ToString('N'))
$arkforge = Join-Path $root ($manifest.arkforge.Replace('/', '\'))
$hdc = Join-Path $root ($manifest.hdc.path.Replace('/', '\'))
$crossAccount = 'skipped'
$runtimeAclDigest = ''
try {
    $hdcProbe = Start-Process -FilePath $hdc -ArgumentList @('-v') -PassThru -WindowStyle Hidden
    if (-not $hdcProbe.WaitForExit(10000)) {
        Stop-Process -Id $hdcProbe.Id -Force -ErrorAction SilentlyContinue
        throw 'The signed HDC executable did not complete its version self-test within 10 seconds.'
    }
    if ($hdcProbe.ExitCode -ne 0) {
        throw "The signed HDC executable failed its version self-test with exit code $($hdcProbe.ExitCode)."
    }
    & $arkforge --runtime-dir $runtime daemon start --hdc $hdc --expect-hdc-sha256 $manifest.hdc.sha256 --require-release-signing
    if ($LASTEXITCODE -ne 0) { throw "daemon start failed with exit code $LASTEXITCODE" }
    & $arkforge --runtime-dir $runtime status
    if ($LASTEXITCODE -ne 0) { throw "status failed with exit code $LASTEXITCODE" }
    if (-not $SkipDevice) {
        & $arkforge --runtime-dir $runtime device list
        if ($LASTEXITCODE -ne 0) { throw "WinUSB device discovery failed with exit code $LASTEXITCODE" }
    }
    $acl = Get-Acl -LiteralPath $runtime
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if (-not $acl.Sddl.Contains($currentSid) -or $acl.Sddl -match ';;;(WD|BU|AU)\)') {
        throw "Runtime ACL is not owner-only: $($acl.Sddl)"
    }
    $aclBytes = [Text.Encoding]::UTF8.GetBytes($acl.Sddl)
    $runtimeAclDigest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($aclBytes)).ToLowerInvariant()

    if ($SkipCrossAccount) {
        $crossAccount = 'skipped by explicit software-only option'
    }
    elseif ($null -eq $DeniedCredential) {
        throw 'Full ACL acceptance requires -DeniedCredential for a different Windows account; use -SkipCrossAccount only for software CI.'
    }
    else {
        $deniedOutput = Join-Path $env:TEMP ('arkforge-denied-' + [guid]::NewGuid().ToString('N') + '.out')
        $deniedError = "$deniedOutput.err"
        try {
            $denied = Start-Process -FilePath $arkforge `
                -ArgumentList @('--runtime-dir', ('"' + $runtime + '"'), 'daemon', 'status') `
                -Credential $DeniedCredential -Wait -PassThru -WindowStyle Hidden `
                -RedirectStandardOutput $deniedOutput -RedirectStandardError $deniedError
            if ($denied.ExitCode -eq 0) {
                throw 'A different Windows account connected to the owner-only ArkForge runtime.'
            }
            $deniedErrorText = if (Test-Path -LiteralPath $deniedError) {
                Get-Content -LiteralPath $deniedError -Raw
            }
            else {
                ''
            }
            if ($deniedErrorText -notmatch '(?i)\bos error 5\b') {
                throw "The different-account probe failed for an unexpected reason instead of ERROR_ACCESS_DENIED: $deniedErrorText"
            }
            $crossAccount = 'different-account named pipe access denied (Win32 error 5)'
        }
        finally {
            Remove-Item -LiteralPath $deniedOutput, $deniedError -Force -ErrorAction SilentlyContinue
        }
    }
}
finally {
    & $arkforge --runtime-dir $runtime daemon stop 2>$null | Out-Null
    if (Test-Path -LiteralPath $runtime) {
        Remove-Item -LiteralPath $runtime -Recurse -Force
    }
}

$result = [ordered]@{
    schema = 'arkforge.windows-acceptance/v1'
    acceptedAtUtc = [DateTime]::UtcNow.ToString('o')
    osVersion = [Environment]::OSVersion.VersionString
    architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    software = 'passed'
    authenticode = 'passed'
    certificateThumbprint = $trustedManifest.certificateThumbprint
    packageReceiptSha256 = $installReceipt.packageReceiptSha256
    arkforgeSha256 = (Get-FileHash -LiteralPath $arkforge -Algorithm SHA256).Hash.ToLowerInvariant()
    arkforgedSha256 = (Get-FileHash -LiteralPath (Join-Path $root ($manifest.arkforged.Replace('/', '\'))) -Algorithm SHA256).Hash.ToLowerInvariant()
    hdcSha256 = $manifest.hdc.sha256
    hdcSelfTest = 'passed'
    publishedDriver = $installReceipt.publishedDriver
    driver = if ($SkipDevice) { 'published; physical device skipped' } else { 'published and DAYU200 Loader bound to WinUSB' }
    deviceCount = if ($SkipDevice) { $null } else { 1 }
    deviceInstanceSha256 = $deviceInstanceDigest
    runtimeAclSha256 = $runtimeAclDigest
    runtimeAcl = 'owner-only'
    namedPipe = 'same-user start/status/stop passed'
    crossAccount = $crossAccount
    destructiveFlash = 'not run'
}
$json = $result | ConvertTo-Json
if ($EvidencePath) {
    $evidenceFullPath = [System.IO.Path]::GetFullPath($EvidencePath)
    $evidenceParent = Split-Path -Parent $evidenceFullPath
    if ($evidenceParent) {
        New-Item -ItemType Directory -Path $evidenceParent -Force | Out-Null
    }
    $json | Set-Content -LiteralPath $evidenceFullPath -Encoding UTF8
}
$json
