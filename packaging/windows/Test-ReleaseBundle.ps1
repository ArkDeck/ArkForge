[CmdletBinding()]
param([string]$ReportPath = '')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
Import-Module (Join-Path $PSScriptRoot 'ReleaseBundle.psm1') -Force
$module = Get-Module ReleaseBundle
$fixture = Join-Path $repoRoot ('target\release-bundle-tests\' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $fixture)
# This is signed public host-tool fixture data, never an ArkForge release or
# hardware result. Nothing in this suite launches an image, daemon or device.
$hostImage = Join-Path $env:SystemRoot 'System32\where.exe'
$hostSignature = Get-AuthenticodeSignature -LiteralPath $hostImage
if ($hostSignature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
    throw 'The software fixture requires a trusted signed Windows system image.'
}
$signer = $hostSignature.SignerCertificate.Thumbprint
$profile = Join-Path $repoRoot 'profiles\dayu200.yaml'
$script:fixtureProfile = $profile
$script:fixtureSigner = $signer
$cases = @()

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-Refused([scriptblock]$Body, [string]$Message = '') {
    $refused = $false
    try { & $Body | Out-Null }
    catch {
        if ($Message -and $_.Exception.Message -notlike "*$Message*") { throw }
        $refused = $true
    }
    Assert-True $refused 'Expected a fail-closed refusal.'
}
function New-TestPackage([string]$Name) {
    $path = Join-Path $fixture $Name
    [void](New-Item -ItemType Directory -Path (Join-Path $path 'bin'))
    [IO.File]::Copy($hostImage, (Join-Path $path 'bin\arkforge.exe'))
    [IO.File]::Copy($hostImage, (Join-Path $path 'bin\arkforged.exe'))
    return $path
}
function Produce([string]$Package, [string]$Profile = $script:fixtureProfile, [string]$Signer = $script:fixtureSigner) {
    return New-ArkForgeReleaseBundle -PackageRoot $Package -ProfilePath $Profile -CertificateThumbprint $Signer -Version '0.1.0'
}
function Tree([string]$Root) {
    return @((Get-ChildItem -LiteralPath $Root -File -Recurse | Sort-Object FullName | ForEach-Object {
        $_.FullName.Substring($Root.Length + 1) + ':' + (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
    })) -join "`n"
}
function Case([string]$Name, [scriptblock]$Body) {
    & $Body
    $script:cases += $Name
    Write-Output "PASS $Name"
}

Case 'signed inputs produce exact closed Windows bundle' {
    $package = New-TestPackage 'positive'
    $result = Produce $package
    $script:positive = $result.root
    $facts = @(Get-ArkForgeReleaseBundleMembers $result.root)
    Assert-True ($facts.Count -eq 3) 'Three members required.'
    Assert-True (($facts.path -join ',') -eq 'bin/arkforge.exe,bin/arkforged.exe,Contents/Resources/profiles/dayu200.yaml') 'Exact Windows member paths required.'
    Assert-True (($facts.role -join ',') -eq 'cli,daemon,profile') 'Closed role order required.'
    Assert-True ($facts[2].profileId -eq 'org.openharmony.dayu200') 'Exact profile ID required.'
    foreach ($index in 0..2) {
        $source = if ($index -eq 2) { $profile } else { Join-Path $package $facts[$index].path }
        Assert-True ($facts[$index].sha256 -ceq (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()) 'Full source digest must match copied member.'
        Assert-True ($facts[$index].bytes -eq (Get-Item -LiteralPath $source).Length) 'Whole source byte count must match.'
    }
    $manifestPath = Join-Path $result.root 'Contents\Resources\arkforge-bundle.json'
    Assert-True ($result.manifestSha256 -ceq (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()) 'Exact manifest SHA required.'
}
Case 'manifest is BOM-free with the consumer schema and closed fields' {
    $path = Join-Path $positive 'Contents\Resources\arkforge-bundle.json'
    $bytes = [IO.File]::ReadAllBytes($path)
    Assert-True (-not ($bytes[0] -eq 0xef -and $bytes[1] -eq 0xbb -and $bytes[2] -eq 0xbf)) 'The Rust consumer does not accept a UTF8 BOM.'
    $manifest = [Text.Encoding]::UTF8.GetString($bytes) | ConvertFrom-Json
    Assert-True ($manifest.schema -ceq 'arkforge.release-bundle/v1' -and $manifest.version -ceq '0.1.0') 'Published schema/version required.'
    Assert-True ((@($manifest.PSObject.Properties.Name | Sort-Object) -join ',') -eq 'members,schema,version') 'Closed root fields required.'
    Assert-True ((@($manifest.members[0].PSObject.Properties.Name | Sort-Object) -join ',') -eq 'bytes,path,role,sha256') 'Closed executable fields required.'
    Assert-True ((@($manifest.members[2].PSObject.Properties.Name | Sort-Object) -join ',') -eq 'bytes,path,profileId,role,sha256') 'Closed profile fields required.'
    Assert-True (@(Get-ChildItem -LiteralPath $positive -File -Recurse).Count -eq 4) 'No undeclared file is permitted.'
}
Case 'existing output is refused and every byte retained' {
    $package = Split-Path -Parent $positive
    $before = Tree $package
    Assert-Refused { Produce $package } 'overwrite'
    Assert-True ((Tree $package) -ceq $before) 'Existing output must remain byte-exact.'
}
Case 'signer mismatch refuses before output creation' {
    $package = New-TestPackage 'wrong-signer'
    Assert-Refused { Produce $package -Signer ('0' * 40) } 'release signature'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $package 'ArkForge.release-bundle'))) 'No bundle may be created after signer refusal.'
}
Case 'unsigned image refuses before output creation' {
    $package = New-TestPackage 'unsigned'
    [IO.File]::WriteAllText((Join-Path $package 'bin\arkforged.exe'), 'not a signed image')
    Assert-Refused { Produce $package } 'release signature'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $package 'ArkForge.release-bundle'))) 'No unsigned bundle output.'
}
Case 'missing executable refuses before output creation' {
    $package = New-TestPackage 'missing-image'
    [IO.File]::Delete((Join-Path $package 'bin\arkforged.exe'))
    Assert-Refused { Produce $package }
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $package 'ArkForge.release-bundle'))) 'No incomplete bundle output.'
}
Case 'wrong profile identity refuses before output creation' {
    $package = New-TestPackage 'wrong-profile'
    $wrong = Join-Path $package 'wrong.yaml'
    [IO.File]::WriteAllText($wrong, ([IO.File]::ReadAllText($profile).Replace('id: org.openharmony.dayu200', 'id: org.openharmony.other')))
    Assert-Refused { Produce $package -Profile $wrong } 'DAYU200'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $package 'ArkForge.release-bundle'))) 'No wrong-profile bundle output.'
}
Case 'duplicate profile declaration refuses before output creation' {
    $package = New-TestPackage 'duplicate-profile'
    $wrong = Join-Path $package 'duplicate.yaml'
    [IO.File]::WriteAllText($wrong, ([IO.File]::ReadAllText($profile) + "`nprofile:`n  id: org.openharmony.dayu200`n  version: 1.0.0`n"))
    Assert-Refused { Produce $package -Profile $wrong } 'ambiguous'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $package 'ArkForge.release-bundle'))) 'No duplicate-profile bundle output.'
}
Case 'wrong profile version refuses' {
    $package = New-TestPackage 'wrong-profile-version'
    $wrong = Join-Path $package 'wrong-version.yaml'
    [IO.File]::WriteAllText($wrong, ([IO.File]::ReadAllText($profile).Replace('  version: 1.0.0', '  version: 9.0.0')))
    Assert-Refused { Produce $package -Profile $wrong } '1.0.0'
}
Case 'profile identity uses the consumers exact case' {
    $package = New-TestPackage 'wrong-profile-case'
    $wrong = Join-Path $package 'wrong-case.yaml'
    [IO.File]::WriteAllText($wrong, ([IO.File]::ReadAllText($profile).Replace('id: org.openharmony.dayu200', 'id: org.openharmony.DAYU200')))
    Assert-Refused { Produce $package -Profile $wrong } 'DAYU200'
}
Case 'undeclared bundle file refuses' {
    $package = New-TestPackage 'extra-member'
    $result = Produce $package
    [IO.File]::WriteAllText((Join-Path $result.root 'extra.txt'), 'undeclared')
    Assert-Refused { Get-ArkForgeReleaseBundleMembers $result.root } 'Undeclared'
}
Case 'missing bundle member refuses' {
    $package = New-TestPackage 'missing-member'
    $result = Produce $package
    [IO.File]::Delete((Join-Path $result.root 'bin\arkforged.exe'))
    Assert-Refused { Get-ArkForgeReleaseBundleMembers $result.root } 'Missing'
}
Case 'changed member gets its actual changed digest' {
    $package = New-TestPackage 'drift'
    $result = Produce $package
    $path = Join-Path $result.root 'Contents\Resources\profiles\dayu200.yaml'
    [IO.File]::AppendAllText($path, "`n# changed bytes`n")
    $fresh = @(Get-ArkForgeReleaseBundleMembers $result.root)
    Assert-True ($fresh[2].sha256 -cne $result.members[2].sha256 -and $fresh[2].bytes -gt $result.members[2].bytes) 'Remeasurement may not return cached facts.'
}
Case 'bundle root reparse point refuses' {
    $link = Join-Path $fixture 'root-junction'
    [void](New-Item -ItemType Junction -Path $link -Target $positive)
    Assert-Refused { Get-ArkForgeReleaseBundleMembers $link } 'reparse'
}
Case 'nested reparse point refuses' {
    $package = New-TestPackage 'nested-junction'
    $result = Produce $package
    [void](New-Item -ItemType Junction -Path (Join-Path $result.root 'redirect') -Target $positive)
    Assert-Refused { Get-ArkForgeReleaseBundleMembers $result.root } 'reparse'
}
Case 'package integration signs before bundle and binds it in the outer manifest' {
    $source = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'package-arkforge.ps1'))
    $produce = $source.IndexOf('$releaseBundle = New-ArkForgeReleaseBundle', [StringComparison]::Ordinal)
    $verified = $source.IndexOf("@('verify', '/pa', '/all', '/v', `$file)", [StringComparison]::Ordinal)
    $outer = $source.IndexOf('$trustedManifest = [ordered]@{', [StringComparison]::Ordinal)
    Assert-True ($verified -ge 0 -and $produce -gt $verified -and $outer -gt $produce) 'Trusted signed outer manifest must follow verified-image bundle construction.'
    Assert-True ($source.Contains("@('verify', '/kp', '/all', '/v', `$driverCatalog)") -and
        $source.Contains("@('verify', '/kp', '/c', `$driverCatalog, `$driverInf)")) 'Existing kernel driver trust checks must remain.'
}
Case 'host CRT and app-local dependencies are refused' {
    Assert-Refused { & $module { Assert-SystemImageDependencies @('kernel32.dll', 'vcruntime140.dll') } } 'system closure'
    Assert-Refused { & $module { Assert-SystemImageDependencies @('C:\host\kernel32.dll') } } 'system closure'
    Assert-Refused { & $module { Assert-SystemImageDependencies @('vendor.dll') } } 'system closure'
    Assert-Refused { & $module { Assert-SystemImageDependencies @('api-ms-win-invented.dll') } } 'system closure'
    & $module { Assert-SystemImageDependencies @('KERNEL32.dll', 'WINUSB.dll', 'bcrypt.dll', 'api-ms-win-core-synch-l1-2-0.dll') }
}
Case 'all producer PowerShell sources parse' {
    foreach ($relative in @('package-arkforge.ps1', 'ReleaseBundle.psm1', 'Test-ReleaseBundle.ps1')) {
        $tokens = $null
        $errors = $null
        [void][Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $relative), [ref]$tokens, [ref]$errors)
        Assert-True ($errors.Count -eq 0) 'Producer script/module parse failed.'
    }
}
Case 'malformed dependency RVA is refused' {
    $bytes = [IO.File]::ReadAllBytes($hostImage)
    $pe = [BitConverter]::ToUInt32($bytes, 0x3c)
    [BitConverter]::GetBytes([uint32]::MaxValue).CopyTo($bytes, [int]$pe + 24 + 120)
    [BitConverter]::GetBytes([uint32]20).CopyTo($bytes, [int]$pe + 24 + 124)
    $stream = [IO.MemoryStream]::new($bytes, $false)
    try { Assert-Refused { & $module { param($InputStream) Get-ImageImportedLibraries $InputStream } $stream } 'RVA' }
    finally { $stream.Dispose() }
}
Case 'delayed imports are refused rather than missed' {
    $bytes = [IO.File]::ReadAllBytes($hostImage)
    $pe = [BitConverter]::ToUInt32($bytes, 0x3c)
    [BitConverter]::GetBytes([uint32]1).CopyTo($bytes, [int]$pe + 24 + 216)
    [BitConverter]::GetBytes([uint32]32).CopyTo($bytes, [int]$pe + 24 + 220)
    $stream = [IO.MemoryStream]::new($bytes, $false)
    try { Assert-Refused { & $module { param($InputStream) Get-ImageImportedLibraries $InputStream } $stream } 'Delayed' }
    finally { $stream.Dispose() }
}
Case 'incomplete whole PE bytes are refused' {
    $bytes = [IO.File]::ReadAllBytes($hostImage)
    $pe = [BitConverter]::ToUInt32($bytes, 0x3c)
    $short = [byte[]]::new([int]$pe + 28)
    [Array]::Copy($bytes, $short, $short.Length)
    $stream = [IO.MemoryStream]::new($short, $false)
    try { Assert-Refused { & $module { param($InputStream) Get-ImageImportedLibraries $InputStream } $stream } }
    finally { $stream.Dispose() }
}

$result = [ordered]@{ schema = 'arkforge.release-bundle-software-tests/v1'; passed = $cases.Count; failed = 0; skipped = 0; cases = $cases; hardwareEvidence = $false; productionAcceptance = $false }
if ($ReportPath) {
    $file = [IO.File]::Open([IO.Path]::GetFullPath($ReportPath), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($result | ConvertTo-Json -Depth 5) + "`n")
        $file.Write($bytes, 0, $bytes.Length)
    }
    finally { $file.Dispose() }
}
$result | ConvertTo-Json -Depth 5
