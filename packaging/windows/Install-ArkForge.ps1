[CmdletBinding()]
param(
    [string]$InstallRoot = (Join-Path $env:ProgramFiles 'ArkForge')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Installation requires an elevated PowerShell session.'
    }
}

function Assert-PackageFile([string]$Root, $Fact) {
    $path = Join-Path $Root ($Fact.path.Replace('/', '\'))
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Package file is missing: $($Fact.path)"
    }
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $Fact.sha256) {
        throw "Package digest mismatch for $($Fact.path): $actual"
    }
}

function Assert-TrustedSignature([string]$Path, [string]$ExpectedThumbprint = '') {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Authenticode is not trusted for $Path ($($signature.Status))."
    }
    if ($ExpectedThumbprint -and $signature.SignerCertificate.Thumbprint -ine $ExpectedThumbprint) {
        throw "Signer mismatch for ${Path}: $($signature.SignerCertificate.Thumbprint)"
    }
}

Assert-Administrator
$packageRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$installerSignature = Get-AuthenticodeSignature -LiteralPath $MyInvocation.MyCommand.Path
if ($installerSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw 'The installer Authenticode signature is not trusted.'
}
$trustedManifestPath = Join-Path $packageRoot 'ArkForge.PackageManifest.ps1'
$trustedSignature = Get-AuthenticodeSignature -LiteralPath $trustedManifestPath
if ($trustedSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
    $trustedSignature.SignerCertificate.Thumbprint -ine $installerSignature.SignerCertificate.Thumbprint) {
    throw 'The package manifest signature does not match the installer release identity.'
}
$trustedManifest = & $trustedManifestPath
if ($trustedManifest.schema -ne 'arkforge.windows-trusted-manifest/v1' -or
    $trustedManifest.certificateThumbprint -ine $installerSignature.SignerCertificate.Thumbprint) {
    throw 'The signed package manifest has an unsupported schema or release identity.'
}
foreach ($fact in $trustedManifest.files) {
    Assert-PackageFile $packageRoot $fact
}
foreach ($relative in @('bin\arkforge.exe', 'bin\arkforged.exe', 'tools\hdc.exe', 'Install-ArkForge.ps1', 'Uninstall-ArkForge.ps1', 'Test-ArkForgePackage.ps1')) {
    Assert-TrustedSignature (Join-Path $packageRoot $relative) $trustedManifest.certificateThumbprint
}
Assert-TrustedSignature (Join-Path $packageRoot 'driver\arkforge-rockusb.cat')
$receipt = Get-Content -LiteralPath (Join-Path $packageRoot 'package-receipt.json') -Raw | ConvertFrom-Json
if ($receipt.schema -ne 'arkforge.windows-package-receipt/v1') {
    throw "Unsupported informational receipt schema: $($receipt.schema)"
}

$destination = Join-Path $InstallRoot 'current'
if (Test-Path -LiteralPath $destination) {
    throw "ArkForge is already installed at $destination; uninstall it before replacing signed bytes."
}
$stage = Join-Path $InstallRoot ('.staging-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage -Force | Out-Null
try {
    Copy-Item -Path (Join-Path $packageRoot '*') -Destination $stage -Recurse -Force
    & "$env:SystemRoot\System32\icacls.exe" $stage '/inheritance:r' '/grant:r' '*S-1-5-18:(OI)(CI)(F)' '*S-1-5-32-544:(OI)(CI)(F)' '*S-1-5-32-545:(OI)(CI)(RX)' | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "icacls failed with exit code $LASTEXITCODE"
    }
    Move-Item -LiteralPath $stage -Destination $destination

    $pnputil = "$env:SystemRoot\System32\pnputil.exe"
    & $pnputil /add-driver (Join-Path $destination 'driver\arkforge-rockusb.inf') /install
    if ($LASTEXITCODE -ne 0) {
        throw "pnputil failed with exit code $LASTEXITCODE"
    }
    $driver = Get-WindowsDriver -Online | Where-Object {
        [System.IO.Path]::GetFileName($_.OriginalFileName) -ieq 'arkforge-rockusb.inf'
    } | Sort-Object Date -Descending | Select-Object -First 1
    if ($null -eq $driver) {
        throw 'Windows did not publish arkforge-rockusb.inf after pnputil succeeded.'
    }
    [ordered]@{
        schema = 'arkforge.windows-install-receipt/v1'
        installedAtUtc = [DateTime]::UtcNow.ToString('o')
        packageReceiptSha256 = (Get-FileHash -LiteralPath (Join-Path $destination 'package-receipt.json') -Algorithm SHA256).Hash.ToLowerInvariant()
        publishedDriver = $driver.Driver
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $destination 'install-receipt.json') -Encoding UTF8
}
catch {
    if (Test-Path -LiteralPath $stage) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    if ((Test-Path -LiteralPath $destination) -and -not (Test-Path -LiteralPath (Join-Path $destination 'install-receipt.json'))) {
        Remove-Item -LiteralPath $destination -Recurse -Force
    }
    throw
}

Write-Output "Installed ArkForge at $destination"
Write-Output "Run: powershell -File `"$(Join-Path $destination 'Test-ArkForgePackage.ps1')`""
