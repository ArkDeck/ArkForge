Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Initialize-InboxWinUsb {
    $bytes = [IO.File]::ReadAllBytes((Join-Path $PSScriptRoot 'InboxWinUsb.cs'))
    $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
    if (-not ('ArkForge.WindowsSetup.InboxWinUsb' -as [type])) {
        $source = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
        $pin = "`nnamespace ArkForge.WindowsSetup { public static class SourceIdentity { public const string Digest = `"$digest`"; } }"
        Add-Type -TypeDefinition ($source + $pin)
    }
    if (-not ('ArkForge.WindowsSetup.SourceIdentity' -as [type]) -or
        [ArkForge.WindowsSetup.SourceIdentity]::Digest -cne $digest) {
        throw 'A different native helper is already loaded. Start a new PowerShell session.'
    }
}

function Assert-OrdinaryBindingPath([string]$Path, [switch]$Existing) {
    $full = [IO.Path]::GetFullPath($Path)
    if ($full.StartsWith('\\') -or $full.Substring(2).Contains(':')) { throw 'A local ordinary path is required.' }
    $cursor = $full
    if (-not $Existing) {
        if ([IO.File]::Exists($full) -or [IO.Directory]::Exists($full)) { throw 'Output already exists.' }
        $cursor = [IO.Path]::GetDirectoryName($full)
    }
    while ($cursor) {
        $attributes = [IO.File]::GetAttributes($cursor)
        if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Links are not accepted for binding records.' }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
    if ($Existing -and -not [IO.File]::Exists($full)) { throw 'Expected an ordinary file.' }
    return $full
}

function Write-NewBindingRecord([string]$Path, $Record) {
    $full = Assert-OrdinaryBindingPath $Path
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($Record | ConvertTo-Json -Depth 12 -Compress) + "`n")
    $stream = [IO.FileStream]::new($full, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write,
        [IO.FileShare]::Read, 4096, [IO.FileOptions]::WriteThrough)
    try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) }
    finally { $stream.Dispose() }
}

function Get-BindingMachineDigest {
    $value = [string](Get-ItemPropertyValue -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Cryptography' -Name MachineGuid)
    if (-not $value) { throw 'Machine identity unavailable.' }
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($value))).ToLowerInvariant()
}

function Get-BindingSourceDigests {
    $result = [ordered]@{}
    foreach ($name in @('Bind-InboxWinUsb.ps1', 'InboxWinUsb.psm1', 'InboxWinUsb.cs')) {
        $result[$name] = (Get-FileHash -LiteralPath (Join-Path $PSScriptRoot $name) -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    return $result
}

function Open-TrustedInboxFiles {
    $held = [Collections.Generic.List[IO.FileStream]]::new()
    try {
        $facts = [ordered]@{}
        foreach ($relative in @('INF\winusb.inf', 'System32\drivers\winusb.sys')) {
            $path = Assert-OrdinaryBindingPath (Join-Path $env:SystemRoot $relative) -Existing
            # Deny writers/deletion while measuring, planning or installing.
            $stream = [IO.FileStream]::new($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
            $held.Add($stream)
            $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream)).ToLowerInvariant()
            $signature = Get-AuthenticodeSignature -LiteralPath $path
            if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
                $signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -cne 'Microsoft Windows') {
                throw "The system inbox file does not have a trusted Microsoft Windows signature: $relative"
            }
            $facts[$relative] = [ordered]@{ sha256 = $hash; bytes = $stream.Length; signer = $signature.SignerCertificate.Thumbprint.ToLowerInvariant() }
        }
        return [pscustomobject]@{ Streams = $held; Facts = $facts }
    }
    catch { foreach ($stream in $held) { $stream.Dispose() }; throw }
}

function Assert-BindingPlan($Plan, $Current, $Candidate, $Inbox, [string]$MachineDigest, $SourceDigests, [DateTimeOffset]$Now) {
    if ($Plan.schema -cne 'arkforge.windows-inbox-binding-plan/v1' -or
        $Plan.action -cne 'bind-system-winusb' -or
        $Plan.hardwareId -cne 'USB\VID_2207&PID_350A' -or
        $Plan.interfaceGuid -cne '{6A4E21F0-50A4-4D7A-B71B-9E945B3F6B7B}' -or
        $Plan.machineDigest -cne $MachineDigest) { throw 'Binding plan identity is invalid.' }
    $created = [DateTimeOffset]::Parse($Plan.createdAtUtc, [Globalization.CultureInfo]::InvariantCulture)
    if ($created -gt $Now -or ($Now - $created).TotalMinutes -gt 10) { throw 'Binding plan is expired or from the future.' }
    foreach ($pair in @(@($Plan.sourceDigests, $SourceDigests), @($Plan.inboxFiles, $Inbox))) {
        if (($pair[0] | ConvertTo-Json -Depth 6 -Compress) -cne ($pair[1] | ConvertTo-Json -Depth 6 -Compress)) {
            throw 'Binding tool or system driver changed since the plan.'
        }
    }
    # SetupAPI returns different path casing from an elevated process on the
    # same machine. Compare the absolute Windows path by Windows semantics;
    # retain exact model/provider/version and the independently pinned bytes.
    $plannedDriver = $Plan.candidate
    if (@($plannedDriver.PSObject.Properties).Count -ne 5 -or
        $plannedDriver.Inf -isnot [string] -or -not [IO.Path]::IsPathFullyQualified($plannedDriver.Inf) -or
        -not [string]::Equals([IO.Path]::GetFullPath($plannedDriver.Inf), [IO.Path]::GetFullPath($Candidate.Inf), [StringComparison]::OrdinalIgnoreCase) -or
        $plannedDriver.Section -cne $Candidate.Section -or
        $plannedDriver.HardwareId -cne $Candidate.HardwareId -or
        $plannedDriver.Provider -cne $Candidate.Provider -or
        ($plannedDriver.Version -isnot [int] -and $plannedDriver.Version -isnot [long]) -or
        $plannedDriver.Version -lt 0 -or [uint64]$plannedDriver.Version -ne $Candidate.Version) {
        throw 'The exact inbox driver model changed since the plan.'
    }
    $expected = [ArkForge.WindowsSetup.BindingSnapshot]::new()
    $fields = $expected.GetType().GetFields()
    if (@($Plan.before.PSObject.Properties).Count -ne $fields.Count) { throw 'Incomplete binding snapshot.' }
    foreach ($field in $fields) {
        $value = $Plan.before.($field.Name)
        if ($field.FieldType -eq [string[]]) {
            if ($value -isnot [array] -or @($value | Where-Object { $_ -isnot [string] }).Count -ne 0) { throw 'Malformed snapshot array.' }
        }
        elseif ($field.FieldType -eq [uint32]) {
            if (($value -isnot [int] -and $value -isnot [long]) -or $value -lt 0 -or $value -gt [uint32]::MaxValue) { throw 'Malformed snapshot integer.' }
        }
        elseif ($field.FieldType -eq [bool]) {
            if ($value -isnot [bool]) { throw 'Malformed snapshot boolean.' }
        }
        elseif ($value -isnot [string]) { throw 'Malformed snapshot string.' }
        $field.SetValue($expected, [Management.Automation.LanguagePrimitives]::ConvertTo($value, $field.FieldType))
    }
    [ArkForge.WindowsSetup.InboxWinUsb]::AssertUnchanged($expected, $Current)
    if ($Current.Service -ieq 'WinUSB') { throw 'The device is already bound to WinUSB; inspect it instead of reinstalling.' }
    return $expected
}

function Assert-SystemWinUsbBinding($Before, $After) {
    if ($After.InstanceId -ine $Before.InstanceId -or
        ($After.HardwareIds | ConvertTo-Json -Compress) -cne ($Before.HardwareIds | ConvertTo-Json -Compress) -or
        ($After.LocationPaths | ConvertTo-Json -Compress) -cne ($Before.LocationPaths | ConvertTo-Json -Compress) -or
        $After.Service -ine 'WinUSB' -or $After.Inf -ine 'winusb.inf' -or $After.Section -ine 'WINUSB' -or
        $After.ClassGuid -ine '{88BAE032-5A81-49f0-BC3D-A4FF138216D6}' -or $After.Problem -ne 0 -or
        @($After.InterfaceGuids) -inotcontains '{6A4E21F0-50A4-4D7A-B71B-9E945B3F6B7B}') {
        throw 'The exact system WinUSB binding has not been observed after installation.'
    }
}

Export-ModuleMember -Function Initialize-InboxWinUsb, Assert-OrdinaryBindingPath, Write-NewBindingRecord,
    Get-BindingMachineDigest, Get-BindingSourceDigests, Open-TrustedInboxFiles, Assert-BindingPlan, Assert-SystemWinUsbBinding
