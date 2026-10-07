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

function Assert-TrueFact($Value, [string]$Name) {
    if ($Value -isnot [bool] -or -not $Value) { throw "Acceptance fact is not true: $Name" }
}

function Assert-NaturalFact($Value, [string]$Name, [long]$Minimum = 0) {
    if (($Value -isnot [int] -and $Value -isnot [long]) -or $Value -lt $Minimum) { throw "Acceptance fact is not a bounded integer: $Name" }
}

function Assert-DaemonStarted($Status, [string]$HdcSha256) {
    Assert-NaturalFact $Status.supervisor_pid 'supervisor_pid' 1
    Assert-NaturalFact $Status.daemon_pid 'daemon_pid' 1
    Assert-NaturalFact $Status.authority.pairing_epoch 'pairing_epoch' 1
    Assert-NaturalFact $Status.active_jobs 'active_jobs'
    if ($Status.schema -cne 'arkforge.daemon-status/v1' -or
        $Status.authority.namespace -cne 'arkforge.cli' -or
        $Status.authority.hdc.sha256 -cne $HdcSha256 -or
        $Status.supervisor_pid -le 0 -or $Status.daemon_pid -le 0 -or
        $Status.authority.pairing_epoch -le 0 -or $Status.active_jobs -ne 0 -or
        $null -ne $Status.authority.hardware_campaign) {
        throw 'The paired runtime did not prove its exact HDC, processes, epoch and idle state.'
    }
    Assert-TrueFact $Status.running 'running'
    Assert-TrueFact $Status.mechanics_ready 'mechanics_ready'
    Assert-TrueFact $Status.authority.hdc.bound 'hdc.bound'
    # AF-W1 qualifies packaging/USB/ACLs. It does not create a HardwareCampaign
    # or promote the current authority support record to production support.
}

function Assert-StatusSnapshot($Status, $Started) {
    Assert-NaturalFact $Status.runtime.pairing_epoch 'runtime.pairing_epoch' 1
    Assert-NaturalFact $Status.runtime.active_job_count 'runtime.active_job_count'
    if ($Status.schema -cne 'arkforge.status/v1' -or
        $Status.runtime.pairing_epoch -ne $Started.authority.pairing_epoch -or
        $Status.runtime.active_job_count -ne 0 -or
        @($Status.runtime.active_jobs).Count -ne 0) { throw 'The owner status snapshot changed or is not idle.' }
    Assert-TrueFact $Status.complete 'status.complete'
    Assert-TrueFact $Status.runtime.running 'runtime.running'
    Assert-TrueFact $Status.runtime.mechanics_ready 'runtime.mechanics_ready'
    Assert-TrueFact $Status.runtime.hdc_bound 'runtime.hdc_bound'
    foreach ($name in @('devices', 'artifacts', 'jobs')) {
        $section = $Status.$name
        Assert-TrueFact $section.available "$name.available"
        Assert-TrueFact $section.complete "$name.complete"
        if ($null -ne $section.reason -or $section.items -isnot [array]) { throw 'An acceptance status section is unobservable.' }
    }
    if ($Status.jobs.items.Count -ne 0) { throw 'The fresh acceptance runtime contains a Job.' }
}

function Assert-Dayu200Discovery($Discovery) {
    if ($Discovery.schema -cne 'arkforge.device-list/v1' -or
        $Discovery.deep -isnot [bool] -or $Discovery.deep -or
        $null -ne $Discovery.filtered_to -or $Discovery.observations -isnot [array] -or
        $Discovery.observations.Count -ne 1) { throw 'Typed discovery must contain exactly one unfiltered Loader observation.' }
    $observation = $Discovery.observations[0]
    if ($observation.mode -cne 'rockusb-loader' -or
        $observation.malformed_descriptor -isnot [bool] -or $observation.malformed_descriptor -or
        $observation.observation_id -isnot [string] -or -not $observation.observation_id -or
        $observation.topology_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $observation.descriptor_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $observation.identification.profile -cne 'org.openharmony.dayu200' -or
        $observation.identification.profile_resolution -cne 'inferred' -or
        $observation.identification.compatible_profiles -isnot [array] -or
        $observation.identification.compatible_profiles.Count -ne 1 -or
        $observation.identification.compatible_profiles[0] -cne 'org.openharmony.dayu200') {
        throw 'Typed discovery did not identify the published DAYU200 Loader profile.'
    }
    # Passive VID/PID inference does not prove a model or a flash qualification.
    return $observation
}

function Assert-DaemonStopped($Status, $Started) {
    Assert-NaturalFact $Status.pairing_epoch 'stop.pairing_epoch' 1
    if ($Status.schema -cne 'arkforge.daemon-stop/v1' -or
        $Status.pairing_epoch -ne $Started.authority.pairing_epoch) { throw 'The stop receipt belongs to a different runtime epoch.' }
    Assert-TrueFact $Status.stopped 'stopped'
}

function Assert-CrossAccountDenial([int]$ExitCode, [string]$ErrorText) {
    if ($ExitCode -ne 5 -or
        $ErrorText -notmatch 'No CLI authority supervisor is listening at' -or
        $ErrorText -notmatch '(?i)\bos error 5\b') {
        throw 'The different-account probe did not prove the supervisor Named Pipe ERROR_ACCESS_DENIED (error 5).'
    }
}

function Wait-AcceptanceRuntimeDrain($Started, [object[]]$Witnesses, [int]$MaximumWaitMilliseconds = 10000) {
    if ($Witnesses.Count -ne 2 -or $MaximumWaitMilliseconds -lt 0 -or $MaximumWaitMilliseconds -gt 10000) { throw 'Missing bounded native runtime drain proof.' }
    $expected = @($Started.supervisor_pid, $Started.daemon_pid)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    for ($index = 0; $index -lt 2; $index++) {
        $witness = $Witnesses[$index]
        if ($null -eq $witness -or $witness.IdentityVerified -isnot [bool] -or -not $witness.IdentityVerified -or
            $witness.ProcessId -ne $expected[$index] -or $witness.BirthFileTime -le 0) { throw 'Original native runtime identity proof is missing or stale.' }
        $remaining = [Math]::Max(0, $MaximumWaitMilliseconds - [int]$clock.ElapsedMilliseconds)
        $exited = $witness.WaitForExit($remaining)
        if ($exited -isnot [bool] -or -not $exited) { throw 'The acknowledged runtime stop did not drain its original processes within the bound; owned files retained.' }
    }
}

function Invoke-AcceptanceJson([string]$Executable, [string]$Runtime, [string[]]$Arguments) {
    $output = @(& $Executable --output json --no-auto-start --runtime-dir $Runtime @Arguments)
    if ($LASTEXITCODE -ne 0) { throw "The typed acceptance command failed with exit code $LASTEXITCODE." }
    $text = $output -join "`n"
    if ([Text.Encoding]::UTF8.GetByteCount($text) -gt 1048576) { throw 'The typed acceptance response exceeds its bound.' }
    return ($text | ConvertFrom-Json -Depth 32 -ErrorAction Stop)
}

function Get-AcceptanceFileFact([string]$Root, [string]$Relative) {
    $base = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    if (-not $base.PSIsContainer -or ($base.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Package root is not an ordinary directory.' }
    if ($Relative -cnotmatch '^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*$' -or
        @($Relative.Split('/') | Where-Object { $_ -in @('.', '..') }).Count -ne 0) { throw 'Unsafe package member path.' }
    $path = $Root
    foreach ($part in $Relative.Split('/')) {
        $path = Join-Path $path $part
        $item = Get-Item -LiteralPath $path -Force -ErrorAction Stop
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Package reparse point refused.' }
    }
    if ($item.PSIsContainer) { throw 'A package member is not a regular file.' }
    $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return [ordered]@{
            sha256 = ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
            bytes = $stream.Length
        }
    }
    finally { $sha.Dispose(); $stream.Dispose() }
}

function Assert-ReleaseBundle([string]$Root, $Receipt, $TrustedManifest) {
    $relative = 'ArkForge.release-bundle/Contents/Resources/arkforge-bundle.json'
    if ($Receipt.releaseBundle.path -cne 'ArkForge.release-bundle' -or
        $Receipt.releaseBundle.manifest -cne $relative) { throw 'The package receipt has no canonical release bundle.' }
    $manifestFact = Get-AcceptanceFileFact $Root $relative
    $trustedHeader = @($TrustedManifest.files | Where-Object { $_.path -ceq $relative })
    if ($trustedHeader.Count -ne 1 -or $manifestFact.sha256 -cne $Receipt.releaseBundle.manifestSha256 -or
        $manifestFact.sha256 -cne $trustedHeader[0].sha256 -or $manifestFact.bytes -ne $trustedHeader[0].bytes) { throw 'Release bundle manifest digest mismatch.' }
    $bundle = Get-Content -LiteralPath (Join-Path $Root $relative) -Raw | ConvertFrom-Json -Depth 16
    if ($bundle.schema -cne 'arkforge.release-bundle/v1' -or $bundle.version -cne $Receipt.version -or
        $bundle.members -isnot [array] -or $bundle.members.Count -ne 3) { throw 'Unsupported release bundle inventory.' }
    $roles = [ordered]@{
        'bin/arkforge.exe' = 'cli'
        'bin/arkforged.exe' = 'daemon'
        'Contents/Resources/profiles/dayu200.yaml' = 'profile'
    }
    $facts = [ordered]@{}
    foreach ($entry in $roles.GetEnumerator()) {
        $rows = @($bundle.members | Where-Object { $_.path -ceq $entry.Key })
        if ($rows.Count -ne 1 -or $rows[0].role -cne $entry.Value -or
            ($entry.Value -eq 'profile' -and $rows[0].profileId -cne 'org.openharmony.dayu200')) { throw 'Release bundle roles are missing or ambiguous.' }
        $member = $rows[0]
        Assert-NaturalFact $member.bytes 'bundle.member.bytes' 1
        $memberPath = 'ArkForge.release-bundle/' + $entry.Key
        $trusted = @($TrustedManifest.files | Where-Object { $_.path -ceq $memberPath })
        $actual = Get-AcceptanceFileFact $Root $memberPath
        if ($trusted.Count -ne 1 -or $actual.sha256 -cne $member.sha256 -or $actual.bytes -ne $member.bytes -or
            $actual.sha256 -cne $trusted[0].sha256 -or $actual.bytes -ne $trusted[0].bytes) { throw 'Release bundle member is not bound to the signed package.' }
        if ($entry.Value -in @('cli', 'daemon')) {
            $installed = Get-AcceptanceFileFact $Root $entry.Key
            if ($installed.sha256 -cne $actual.sha256 -or $installed.bytes -ne $actual.bytes) { throw 'Release bundle differs from the accepted executable.' }
        }
        $facts[$entry.Value] = $actual.sha256
    }
    $observed = @(Get-ChildItem -LiteralPath (Join-Path $Root 'ArkForge.release-bundle') -Recurse -Force)
    if (@($observed | Where-Object { -not $_.PSIsContainer }).Count -ne 4 -or
        @($observed | Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 }).Count -ne 0) { throw 'Undeclared release bundle files.' }
    return [ordered]@{ manifestSha256 = $manifestFact.sha256; profileSha256 = $facts.profile; cliSha256 = $facts.cli; daemonSha256 = $facts.daemon }
}

function Get-NativeRuntimePath([string]$Path) {
    # Rust hashes the native final directory spelling for its pipe endpoint.
    # Give the denied user this exact spelling too: its failed canonicalize
    # must not fall back to a different pipe name.
    if (-not ('ArkForgeAcceptance.NativePath' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using Microsoft.Win32.SafeHandles;
namespace ArkForgeAcceptance {
    public static class NativePath {
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        static extern SafeFileHandle CreateFileW(string path, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        static extern uint GetFinalPathNameByHandleW(SafeFileHandle handle, StringBuilder path, uint size, uint flags);
        [DllImport("kernel32.dll", SetLastError=true)]
        static extern SafeFileHandle OpenProcess(uint access, bool inherit, int pid);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        static extern bool QueryFullProcessImageNameW(SafeFileHandle process, uint flags, StringBuilder name, ref uint size);
        [DllImport("kernel32.dll", SetLastError=true)]
        static extern bool GetProcessTimes(SafeFileHandle process, out System.Runtime.InteropServices.ComTypes.FILETIME creation, out System.Runtime.InteropServices.ComTypes.FILETIME exit, out System.Runtime.InteropServices.ComTypes.FILETIME kernel, out System.Runtime.InteropServices.ComTypes.FILETIME user);
        [DllImport("kernel32.dll", SetLastError=true)]
        static extern uint WaitForSingleObject(SafeFileHandle handle, uint milliseconds);
        [StructLayout(LayoutKind.Sequential)]
        struct FileInfo {
            public uint attributes;
            public System.Runtime.InteropServices.ComTypes.FILETIME creation, access, write;
            public uint volume, sizeHigh, sizeLow, links, indexHigh, indexLow;
        }
        [DllImport("kernel32.dll", SetLastError=true)]
        static extern bool GetFileInformationByHandle(SafeFileHandle handle, out FileInfo info);
        static string FinalPath(SafeFileHandle handle) {
            var result = new StringBuilder(32768);
            uint count = GetFinalPathNameByHandleW(handle, result, (uint)result.Capacity, 0);
            if (count == 0 || count >= result.Capacity) throw new Win32Exception(Marshal.GetLastWin32Error());
            return result.ToString();
        }
        public static string Resolve(string path) {
            using (var handle = CreateFileW(path, 0, 1, IntPtr.Zero, 3, 0x02200000, IntPtr.Zero)) {
                if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
                return FinalPath(handle);
            }
        }
        public sealed class ProcessWitness : IDisposable {
            readonly SafeFileHandle handle;
            public int ProcessId { get; }
            public long BirthFileTime { get; }
            public string ImageSha256 { get; }
            public bool IdentityVerified { get { return !handle.IsInvalid && !handle.IsClosed; } }
            internal ProcessWitness(SafeFileHandle retained, int pid, long birth, string sha) { handle = retained; ProcessId = pid; BirthFileTime = birth; ImageSha256 = sha; }
            public bool WaitForExit(int milliseconds) {
                if (!IdentityVerified || milliseconds < 0 || milliseconds > 10000) throw new InvalidOperationException("Invalid native wait proof.");
                uint result = WaitForSingleObject(handle, (uint)milliseconds);
                if (result == 0) return true;
                if (result == 258) return false;
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
            public void Dispose() { handle.Dispose(); }
        }
        public static ProcessWitness RetainProcess(int pid, string expectedImage, string expectedSha256) {
            if (pid <= 0) throw new InvalidOperationException("Missing original runtime PID.");
            var process = OpenProcess(0x00101000, false, pid); // SYNCHRONIZE | QUERY_LIMITED_INFORMATION
            try {
                if (process.IsInvalid || WaitForSingleObject(process, 0) != 258) throw new InvalidOperationException("Original runtime is not live.");
                var image = new StringBuilder(32768);
                uint capacity = (uint)image.Capacity;
                if (!QueryFullProcessImageNameW(process, 0, image, ref capacity)) throw new Win32Exception(Marshal.GetLastWin32Error());
                System.Runtime.InteropServices.ComTypes.FILETIME birth, exit, kernel, user;
                if (!GetProcessTimes(process, out birth, out exit, out kernel, out user)) throw new Win32Exception(Marshal.GetLastWin32Error());
                long birthValue = ((long)(uint)birth.dwHighDateTime << 32) | (uint)birth.dwLowDateTime;
                if (birthValue <= 0) throw new InvalidOperationException("Missing original runtime birth.");
                using (var expected = CreateFileW(expectedImage, 0x80000000, 1, IntPtr.Zero, 3, 0x00200000, IntPtr.Zero))
                using (var actual = CreateFileW(image.ToString(), 0x80000000, 1, IntPtr.Zero, 3, 0x00200000, IntPtr.Zero)) {
                    if (expected.IsInvalid || actual.IsInvalid) throw new InvalidOperationException("Native runtime image could not be retained.");
                    FileInfo first, second;
                    if (!GetFileInformationByHandle(expected, out first) || !GetFileInformationByHandle(actual, out second)) throw new Win32Exception(Marshal.GetLastWin32Error());
                    if ((first.attributes & 0x410) != 0 || (second.attributes & 0x410) != 0 ||
                        first.volume != second.volume || first.indexHigh != second.indexHigh || first.indexLow != second.indexLow ||
                        !String.Equals(FinalPath(expected), FinalPath(actual), StringComparison.OrdinalIgnoreCase)) throw new InvalidOperationException("Native runtime image identity mismatch.");
                    using (var file = new FileStream(actual, FileAccess.Read))
                    using (var sha = SHA256.Create()) {
                        string digest = BitConverter.ToString(sha.ComputeHash(file)).Replace("-", "").ToLowerInvariant();
                        if (!String.Equals(digest, expectedSha256, StringComparison.Ordinal)) throw new InvalidOperationException("Native runtime image digest mismatch.");
                    }
                }
                if (WaitForSingleObject(process, 0) != 258) throw new InvalidOperationException("Original runtime exited during native proof.");
                return new ProcessWitness(process, pid, birthValue, expectedSha256);
            }
            catch { process.Dispose(); throw; }
        }
    }
}
'@
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'The owned runtime is not an ordinary directory.' }
    return [ArkForgeAcceptance.NativePath]::Resolve($Path)
}

function Write-AcceptanceEvidence([string]$Path, [string]$Json) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Json + "`n"); $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) }
    finally { $stream.Dispose() }
}

if ($EvidencePath) {
    $EvidencePath = [IO.Path]::GetFullPath($EvidencePath)
    if (Test-Path -LiteralPath $EvidencePath) { throw 'Refusing to replace existing acceptance evidence.' }
    $parent = Get-Item -LiteralPath ([IO.Path]::GetDirectoryName($EvidencePath)) -Force
    if (-not $parent.PSIsContainer -or ($parent.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Acceptance evidence requires an existing ordinary parent.' }
}
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
    $actual = Get-AcceptanceFileFact $root $fact.path
    if ($actual.sha256 -cne $fact.sha256 -or $actual.bytes -ne $fact.bytes) {
        throw 'An installed file does not match its signed digest and byte count.'
    }
}
$receiptFact = Get-AcceptanceFileFact $root 'package-receipt.json'
if ($receiptFact.sha256 -cne $installReceipt.packageReceiptSha256) { throw 'The installed package receipt changed.' }
$bundleFacts = Assert-ReleaseBundle $root $receipt $trustedManifest
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
$crossAccountStopUncertain = $false
$runtimeAclDigest = ''
$started = $null
$observation = $null
$witnesses = @()
try {
    $hdcProbe = Start-Process -FilePath $hdc -ArgumentList @('-v') -PassThru -WindowStyle Hidden
    if (-not $hdcProbe.WaitForExit(10000)) {
        $hdcProbe.Kill()
        throw 'The signed HDC executable did not complete its version self-test within 10 seconds.'
    }
    if ($hdcProbe.ExitCode -ne 0) {
        throw "The signed HDC executable failed its version self-test with exit code $($hdcProbe.ExitCode)."
    }
    $startResult = Invoke-AcceptanceJson $arkforge $runtime @('daemon', 'start', '--hdc', $hdc,
        '--expect-hdc-sha256', $manifest.hdc.sha256, '--require-release-signing', '--profile-file',
        (Join-Path $root 'ArkForge.release-bundle/Contents/Resources/profiles/dayu200.yaml'))
    Assert-DaemonStarted $startResult $manifest.hdc.sha256
    $nativeRuntime = Get-NativeRuntimePath $runtime
    $witnesses += [ArkForgeAcceptance.NativePath]::RetainProcess($startResult.supervisor_pid, $arkforge, $bundleFacts.cliSha256)
    $witnesses += [ArkForgeAcceptance.NativePath]::RetainProcess($startResult.daemon_pid,
        (Join-Path $root ($manifest.arkforged.Replace('/', '\'))), $bundleFacts.daemonSha256)
    $started = $startResult
    Assert-StatusSnapshot (Invoke-AcceptanceJson $arkforge $nativeRuntime @('status')) $started
    if (-not $SkipDevice) {
        $observation = Assert-Dayu200Discovery (Invoke-AcceptanceJson $arkforge $nativeRuntime @('device', 'list'))
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
        try { $deniedSid = [Security.Principal.NTAccount]::new($DeniedCredential.UserName).Translate([Security.Principal.SecurityIdentifier]).Value }
        catch { throw 'The configured different-account identity could not be resolved.' }
        if ($deniedSid -eq $currentSid) { throw 'The ACL rejection credential belongs to the runtime owner.' }
        $deniedOutput = Join-Path $env:TEMP ('arkforge-denied-' + [guid]::NewGuid().ToString('N') + '.out')
        $deniedError = "$deniedOutput.err"
        $deniedVerified = $false
        try {
            # Until native connect denial is proved, this stop may have reached
            # the owner. Retain the runtime rather than retrying that lifecycle.
            $crossAccountStopUncertain = $true
            $denied = Start-Process -FilePath $arkforge `
                -ArgumentList @('--runtime-dir', ('"' + $nativeRuntime + '"'), 'daemon', 'stop') `
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
            # daemon status is not a published command. The closed stop leaf
            # preserves the exact supervisor-connect error. A broken ACL can
            # only stop this fresh owned runtime, and must fail acceptance.
            Assert-CrossAccountDenial $denied.ExitCode $deniedErrorText
            $crossAccountStopUncertain = $false
            $deniedVerified = $true
            $crossAccount = 'different-account named pipe access denied (Win32 error 5)'
        }
        finally {
            if ($deniedVerified) {
                Remove-Item -LiteralPath $deniedOutput, $deniedError -Force -ErrorAction SilentlyContinue
            }
        }
    }
}
finally {
    try {
        if ($crossAccountStopUncertain) { throw 'Different-account stop outcome is unproven; runtime and probe evidence retained, no stop retry.' }
        if ($null -ne $started) {
            $stop = Invoke-AcceptanceJson $arkforge $nativeRuntime @('daemon', 'stop')
            Assert-DaemonStopped $stop $started
            Wait-AcceptanceRuntimeDrain $started $witnesses
            # The reply precedes native process exit. Delete only after both
            # original retained handles signal exit and this is still our root.
            $owned = Get-Item -LiteralPath $runtime -Force
            if (-not $owned.PSIsContainer -or ($owned.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
                [IO.Path]::GetFullPath($owned.Parent.FullName) -ine [IO.Path]::GetFullPath($env:LOCALAPPDATA) -or
                $owned.Name -cnotmatch '^ArkForge-Acceptance-[0-9a-f]{32}$' -or
                (Get-NativeRuntimePath $runtime) -cne $nativeRuntime) { throw 'Owned runtime cleanup refused.' }
            Remove-Item -LiteralPath $owned.FullName -Recurse -Force
        }
    }
    finally { foreach ($witness in $witnesses) { $witness.Dispose() } }
}

$result = [ordered]@{
    schema = 'arkforge.windows-acceptance/v1'
    fullAcceptance = (-not $SkipDevice -and -not $SkipCrossAccount)
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
    releaseBundle = $bundleFacts
    profileId = 'org.openharmony.dayu200'
    profileVersion = '1.0.0'
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
$json = $result | ConvertTo-Json -Depth 8
if ($EvidencePath) {
    Write-AcceptanceEvidence $EvidencePath $json
}
$json
