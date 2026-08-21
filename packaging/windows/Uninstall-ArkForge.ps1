[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'High')]
param(
    [string]$InstallRoot = (Join-Path $env:ProgramFiles 'ArkForge')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$destination = Join-Path $InstallRoot 'current'
if (-not (Test-Path -LiteralPath $destination -PathType Container)) {
    Write-Output 'ArkForge is not installed.'
    exit 0
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Uninstallation requires an elevated PowerShell session.'
}
$installReceiptPath = Join-Path $destination 'install-receipt.json'
if (-not (Test-Path -LiteralPath $installReceiptPath -PathType Leaf)) {
    throw 'The installation receipt is missing; refusing an unscoped removal.'
}
$installReceipt = Get-Content -LiteralPath $installReceiptPath -Raw | ConvertFrom-Json
if ($installReceipt.schema -ne 'arkforge.windows-install-receipt/v1') {
    throw "Unsupported installation receipt schema: $($installReceipt.schema)"
}

if ($PSCmdlet.ShouldProcess($installReceipt.publishedDriver, 'Uninstall ArkForge WinUSB driver')) {
    & "$env:SystemRoot\System32\pnputil.exe" /delete-driver $installReceipt.publishedDriver /uninstall /force
    if ($LASTEXITCODE -ne 0) {
        throw "pnputil failed with exit code $LASTEXITCODE; installed files were preserved."
    }
}
if ($PSCmdlet.ShouldProcess($destination, 'Remove ArkForge installed files')) {
    Remove-Item -LiteralPath $destination -Recurse -Force
}
Write-Output 'ArkForge was uninstalled. Per-user runtime journals were preserved.'
