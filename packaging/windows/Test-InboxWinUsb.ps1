#Requires -Version 7.2
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'InboxWinUsb.psm1') -Force
Initialize-InboxWinUsb
# Only pure guards and host file records run here. Do not call Inspect, Select,
# Bind, any installation API or a USB device, even on a hardware CI runner.
$script:passed = 0
function Check([string]$Name, [scriptblock]$Body) {
    & $Body
    $script:passed++
    Write-Output "PASS $Name"
}
function Refuses([scriptblock]$Body) {
    $refused = $false
    try { & $Body | Out-Null } catch { $refused = $true }
    if (-not $refused) { throw 'Expected a refusal.' }
}
function Snapshot {
    $value = [ArkForge.WindowsSetup.BindingSnapshot]::new()
    $value.InstanceId = 'USB\VID_2207&PID_350A\FIXTURE'
    $value.HardwareIds = @('USB\VID_2207&PID_350A')
    $value.CompatibleIds = @('USB\Class_FF&SubClass_06&Prot_05')
    $value.LocationPaths = @('FIXTURE-PORT-1')
    $value.UpperFilters = @(); $value.LowerFilters = @()
    $value.Service = 'Rockusb'; $value.ClassGuid = '{79DFC2A8-2574-493C-AE71-52958E41DD00}'
    $value.DriverKey = 'fixture\0000'; $value.Inf = 'oem99.inf'
    $value.Section = 'Rockusb_DDI.NT'; $value.Version = '4.90.0.0'
    $value.InterfaceGuids = @(); $value.InterfaceGuidsPresent = $false
    $value.Status = 0; $value.Problem = 0
    return $value
}
$now = [DateTimeOffset]::Parse('2026-10-14T00:00:00Z')
$candidate = [ordered]@{ Inf = 'C:\fixture-system\winusb.inf'; Section = 'WINUSB'; HardwareId = 'USB\MS_COMP_WINUSB'; Provider = 'Microsoft'; Version = 1 }
$inbox = [ordered]@{ 'INF\winusb.inf' = @{ sha256 = 'a' * 64 }; 'System32\drivers\winusb.sys' = @{ sha256 = 'b' * 64 } }
$sources = [ordered]@{ 'Bind-InboxWinUsb.ps1' = 'c' * 64; 'InboxWinUsb.psm1' = 'd' * 64; 'InboxWinUsb.cs' = 'e' * 64 }
function Plan {
    return ([ordered]@{
        schema = 'arkforge.windows-inbox-binding-plan/v1'; action = 'bind-system-winusb'
        hardwareId = 'USB\VID_2207&PID_350A'; interfaceGuid = '{6A4E21F0-50A4-4D7A-B71B-9E945B3F6B7B}'
        machineDigest = 'f' * 64; createdAtUtc = $now.ToString('o'); sourceDigests = $sources
        inboxFiles = $inbox; candidate = $candidate; before = Snapshot
    } | ConvertTo-Json -Depth 12 | ConvertFrom-Json)
}
function Validate($plan, $current = (Snapshot), $when = $now) {
    return Assert-BindingPlan $plan $current $candidate $inbox ('f' * 64) $sources $when
}

Check 'exact plan round trip retains typed device identity' {
    $expected = Validate (Plan)
    [ArkForge.WindowsSetup.InboxWinUsb]::AssertUnchanged($expected, (Snapshot))
}
Check 'different instance location driver and GUID state refuse before installation' {
    foreach ($field in @('InstanceId', 'Service', 'ClassGuid', 'DriverKey', 'Inf', 'Section', 'Version')) {
        $current = Snapshot; $current.$field = 'changed'
        Refuses { Validate (Plan) $current }
    }
    foreach ($field in @('HardwareIds', 'CompatibleIds', 'LocationPaths', 'UpperFilters', 'LowerFilters', 'InterfaceGuids')) {
        $current = Snapshot; $current.$field = @('changed')
        Refuses { Validate (Plan) $current }
    }
    $current = Snapshot; $current.InterfaceGuidsPresent = $true; Refuses { Validate (Plan) $current }
    $current = Snapshot; $current.Problem = 22; Refuses { Validate (Plan) $current }
}
Check 'expired future foreign-machine and changed-action plans refuse' {
    Refuses { Validate (Plan) (Snapshot) $now.AddMinutes(11) }
    Refuses { Validate (Plan) (Snapshot) $now.AddSeconds(-1) }
    foreach ($field in @('machineDigest', 'hardwareId', 'interfaceGuid', 'schema', 'action')) {
        $plan = Plan; $plan.$field = 'changed'; Refuses { Validate $plan }
    }
}
Check 'changed helper signed-file facts or selected driver refuse' {
    $plan = Plan; $plan.sourceDigests.'InboxWinUsb.cs' = '0' * 64; Refuses { Validate $plan }
    $plan = Plan; $plan.inboxFiles.'INF\winusb.inf'.sha256 = '0' * 64; Refuses { Validate $plan }
    $plan = Plan; $plan.candidate.Section = 'BILLBOARD'; Refuses { Validate $plan }
}
Check 'elevation path casing accepts the same file but other paths and versions refuse' {
    $plan = Plan; $plan.candidate.Inf = $candidate.Inf.ToUpperInvariant()
    [void](Validate $plan)
    foreach ($otherPath in @('C:\other-system\winusb.inf', 'fixture-system\winusb.inf', 'C:\fixture-system\other.inf')) {
        $plan = Plan; $plan.candidate.Inf = $otherPath; Refuses { Validate $plan }
    }
    $plan = Plan; $plan.candidate.Version = 2; Refuses { Validate $plan }
    $plan = Plan; $plan.candidate.Version = '1'; Refuses { Validate $plan }
    $plan = Plan; $plan.candidate.Provider = 'other'; Refuses { Validate $plan }
}
Check 'missing or malformed snapshot data refuses' {
    $plan = Plan; $plan.before.PSObject.Properties.Remove('LocationPaths'); Refuses { Validate $plan }
    $plan = Plan; $plan.before.HardwareIds = @(); Refuses { Validate $plan }
    $plan = Plan; $plan.before.Problem = 'unknown'; Refuses { Validate $plan }
}
Check 'only observed exact WinUSB binding is accepted' {
    $before = Snapshot; $after = Snapshot
    $after.Service = 'WinUSB'; $after.Inf = 'winusb.inf'; $after.Section = 'WINUSB'
    $after.ClassGuid = '{88BAE032-5A81-49f0-BC3D-A4FF138216D6}'
    $after.InterfaceGuids = @('{6A4E21F0-50A4-4D7A-B71B-9E945B3F6B7B}')
    Assert-SystemWinUsbBinding $before $after
    $after.Service = 'Rockusb'; Refuses { Assert-SystemWinUsbBinding $before $after }
    $after.Service = 'WinUSB'; $after.Inf = 'oem99.inf'; Refuses { Assert-SystemWinUsbBinding $before $after }
    $after.Inf = 'winusb.inf'; $after.LocationPaths = @('OTHER-PORT'); Refuses { Assert-SystemWinUsbBinding $before $after }
}

$fixture = Join-Path ([IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))) ('.inbox-winusb-fixture-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($fixture)
try {
    Check 'durable intent cannot overwrite an existing record' {
        $path = Join-Path $fixture 'intent.json'
        Write-NewBindingRecord $path @{ outcome = 'unknown'; planSha256 = 'a' * 64 }
        $hash = (Get-FileHash -LiteralPath $path).Hash
        Refuses { Write-NewBindingRecord $path @{ outcome = 'success' } }
        if ((Get-FileHash -LiteralPath $path).Hash -cne $hash) { throw 'Existing intent changed.' }
    }
    Check 'UNC alternate streams directory targets and missing parents refuse' {
        Refuses { Assert-OrdinaryBindingPath '\\localhost\fixture\record.json' }
        Refuses { Assert-OrdinaryBindingPath (Join-Path $fixture 'record.json:stream') }
        Refuses { Assert-OrdinaryBindingPath $fixture -Existing }
        Refuses { Write-NewBindingRecord (Join-Path $fixture 'missing\record.json') @{} }
    }
}
finally {
    $resolved = [IO.Path]::GetFullPath($fixture)
    $root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
    if ([IO.Path]::GetDirectoryName($resolved) -ine $root -or [IO.Path]::GetFileName($resolved) -cnotmatch '^\.inbox-winusb-fixture-[0-9a-f]{32}$') { throw 'Cleanup target refused.' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
Write-Output "Passed $script:passed host-only WinUSB binding checks; no device installation or transfer."
