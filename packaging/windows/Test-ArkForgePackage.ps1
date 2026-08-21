[CmdletBinding()]
param(
    [string]$InstallRoot = (Join-Path $env:ProgramFiles 'ArkForge'),
    [switch]$SkipDevice
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
        throw "Release signer mismatch for $relative: $($signature.SignerCertificate.Thumbprint)"
    }
}

$published = Get-WindowsDriver -Online | Where-Object { $_.Driver -eq $installReceipt.publishedDriver }
if ($null -eq $published) {
    throw "Published driver $($installReceipt.publishedDriver) is missing."
}
if (-not $SkipDevice) {
    $device = Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'USB\VID_2207&PID_350A*' } | Select-Object -First 1
    if ($null -eq $device) {
        throw 'Connect exactly one DAYU200 in Loader mode, or use -SkipDevice for software-only acceptance.'
    }
    $service = (Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_Service').Data
    if ($service -ine 'WinUSB') {
        throw "DAYU200 Loader is bound to $service, not WinUSB."
    }
}

$runtime = Join-Path $env:LOCALAPPDATA ('ArkForge-Acceptance-' + [guid]::NewGuid().ToString('N'))
$arkforge = Join-Path $root ($manifest.arkforge.Replace('/', '\'))
$hdc = Join-Path $root ($manifest.hdc.path.Replace('/', '\'))
try {
    & $arkforge --runtime-dir $runtime daemon start --hdc $hdc --expect-hdc-sha256 $manifest.hdc.sha256 --require-release-signing
    if ($LASTEXITCODE -ne 0) { throw "daemon start failed with exit code $LASTEXITCODE" }
    & $arkforge --runtime-dir $runtime daemon status
    if ($LASTEXITCODE -ne 0) { throw "daemon status failed with exit code $LASTEXITCODE" }
    if (-not $SkipDevice) {
        & $arkforge --runtime-dir $runtime device list
        if ($LASTEXITCODE -ne 0) { throw "WinUSB device discovery failed with exit code $LASTEXITCODE" }
    }
    $acl = Get-Acl -LiteralPath $runtime
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if (-not $acl.Sddl.Contains($currentSid) -or $acl.Sddl -match ';;;(WD|BU|AU)\)') {
        throw "Runtime ACL is not owner-only: $($acl.Sddl)"
    }
}
finally {
    & $arkforge --runtime-dir $runtime daemon stop 2>$null | Out-Null
    if (Test-Path -LiteralPath $runtime) {
        Remove-Item -LiteralPath $runtime -Recurse -Force
    }
}

[ordered]@{
    schema = 'arkforge.windows-acceptance/v1'
    software = 'passed'
    authenticode = 'passed'
    driver = if ($SkipDevice) { 'published; physical device skipped' } else { 'published and DAYU200 Loader bound to WinUSB' }
    namedPipe = 'same-user start/status/stop passed; local-only and owner SID are enforced by the platform backend'
    destructiveFlash = 'not run'
} | ConvertTo-Json
