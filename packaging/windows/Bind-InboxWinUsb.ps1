#Requires -Version 7.2
[CmdletBinding()]
param(
    [ValidateSet('Inspect', 'Plan', 'Apply')][string]$Action = 'Inspect',
    [string]$PlanPath = '',
    [string]$PlanSha256 = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows -or -not [Environment]::Is64BitProcess) { throw 'Windows x64 PowerShell is required.' }
Import-Module (Join-Path $PSScriptRoot 'InboxWinUsb.psm1') -Force
Initialize-InboxWinUsb
$held = Open-TrustedInboxFiles
try {
    $before = [ArkForge.WindowsSetup.InboxWinUsb]::Inspect()
    $candidate = [ArkForge.WindowsSetup.InboxWinUsb]::InspectInbox($before.InstanceId)
    if ($Action -eq 'Inspect') {
        if ($PlanPath -or $PlanSha256) { throw 'Inspect does not accept a plan.' }
        [ordered]@{
            schema = 'arkforge.windows-inbox-binding-inspection/v1'; readOnly = $true
            hardwareId = [ArkForge.WindowsSetup.InboxWinUsb]::HardwareId
            service = $before.Service; inf = $before.Inf; section = $before.Section
            interfaceRegistered = @($before.InterfaceGuids) -icontains [ArkForge.WindowsSetup.InboxWinUsb]::InterfaceGuid
            systemWinUsbAvailable = $true; candidate = $candidate
        } | ConvertTo-Json -Depth 6
        return
    }
    if (-not $PlanPath) { throw 'PlanPath is required.' }
    if ($Action -eq 'Plan') {
        if ($PlanSha256) { throw 'Plan does not accept an existing digest.' }
        if ($before.Service -ieq 'WinUSB') { throw 'Already bound to WinUSB; no reinstall plan is needed.' }
        if ($before.Inf -cnotmatch '^oem[0-9]+\.inf$') { throw 'The previous published driver INF is not an ordinary OEM package.' }
        $previousInf = Join-Path (Join-Path $env:SystemRoot 'INF') $before.Inf
        $plan = [ordered]@{
            schema = 'arkforge.windows-inbox-binding-plan/v1'; action = 'bind-system-winusb'
            createdAtUtc = [DateTimeOffset]::UtcNow.ToString('o'); machineDigest = Get-BindingMachineDigest
            hardwareId = [ArkForge.WindowsSetup.InboxWinUsb]::HardwareId
            interfaceGuid = [ArkForge.WindowsSetup.InboxWinUsb]::InterfaceGuid
            sourceDigests = Get-BindingSourceDigests; inboxFiles = $held.Facts; candidate = $candidate
            before = $before; previousInfSha256 = (Get-FileHash -LiteralPath $previousInf -Algorithm SHA256).Hash.ToLowerInvariant()
            effects = @('set exact device interface GUID', 'replace exact device function-driver binding')
        }
        Write-NewBindingRecord $PlanPath $plan
        [ordered]@{
            schema = 'arkforge.windows-inbox-binding-plan-result/v1'
            planSha256 = (Get-FileHash -LiteralPath $PlanPath -Algorithm SHA256).Hash.ToLowerInvariant()
            previousService = $before.Service; previousInf = $before.Inf
            next = 'Apply this exact digest within ten minutes from an elevated PowerShell.'
        } | ConvertTo-Json
        return
    }

    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Apply requires an elevated PowerShell session.' }
    if ($PlanSha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Apply requires the exact lowercase plan SHA-256.' }
    $planFile = Assert-OrdinaryBindingPath $PlanPath -Existing
    $lockedPlan = [IO.FileStream]::new($planFile, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if ($lockedPlan.Length -gt 65536) { throw 'Binding plan is oversized.' }
        $actual = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($lockedPlan)).ToLowerInvariant()
        if ($actual -cne $PlanSha256) { throw 'Plan digest mismatch.' }
        $lockedPlan.Position = 0
        $reader = [IO.StreamReader]::new($lockedPlan, [Text.UTF8Encoding]::new($false, $true), $false, 4096, $true)
        try { $plan = $reader.ReadToEnd() | ConvertFrom-Json -Depth 12 }
        finally { $reader.Dispose() }
        $expected = Assert-BindingPlan $plan $before $candidate $held.Facts (Get-BindingMachineDigest) (Get-BindingSourceDigests) ([DateTimeOffset]::UtcNow)
        if ($before.Inf -cnotmatch '^oem[0-9]+\.inf$' -or
            (Get-FileHash -LiteralPath (Join-Path (Join-Path $env:SystemRoot 'INF') $before.Inf) -Algorithm SHA256).Hash.ToLowerInvariant() -cne $plan.previousInfSha256) {
            throw 'The retained original driver package changed.'
        }
        $intentPath = $planFile + '.intent.json'
        $resultPath = $planFile + '.result.json'
        [void](Assert-OrdinaryBindingPath $resultPath)
        # CreateNew consumes this plan once, including on an unknown installation
        # result. Never delete this intent, replay it or guess that rollback is safe.
        Write-NewBindingRecord $intentPath ([ordered]@{
            schema = 'arkforge.windows-inbox-binding-intent/v1'; planSha256 = $actual
            startedAtUtc = [DateTimeOffset]::UtcNow.ToString('o'); outcome = 'unknown'
        })
        try {
            $reboot = [ArkForge.WindowsSetup.InboxWinUsb]::Bind($expected)
            if ($reboot) { throw 'Windows requires a host restart; binding is not accepted and no restart was initiated.' }
            $after = [ArkForge.WindowsSetup.InboxWinUsb]::Inspect()
            Assert-SystemWinUsbBinding $before $after
            Write-NewBindingRecord $resultPath ([ordered]@{
                schema = 'arkforge.windows-inbox-binding-result/v1'; planSha256 = $actual
                completedAtUtc = [DateTimeOffset]::UtcNow.ToString('o'); outcome = 'binding-verified'
                before = $before; after = $after; needReboot = $false
                qualification = 'host binding only; not a flash, AF-W1 or hardware support result'
            })
            Write-Output 'Verified exact Loader binding to Microsoft system WinUSB. Run typed ArkForge discovery next.'
        }
        catch {
            if (-not [IO.File]::Exists($resultPath)) {
                Write-NewBindingRecord $resultPath ([ordered]@{
                    schema = 'arkforge.windows-inbox-binding-result/v1'; planSha256 = $actual
                    completedAtUtc = [DateTimeOffset]::UtcNow.ToString('o'); outcome = 'unknown'
                    error = $_.Exception.Message; automaticRetry = $false
                })
            }
            throw
        }
    }
    finally { $lockedPlan.Dispose() }
}
finally { foreach ($stream in $held.Streams) { $stream.Dispose() } }
