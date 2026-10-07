[CmdletBinding()]
param([string]$ReportPath = '')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'ReleaseBundle.psm1') -Force
$module = Get-Module ReleaseBundle
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$fixture = Join-Path $repo ('target\hdc-package-tests\' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $fixture)
$script:cases = @()

function Assert([bool]$Value, [string]$Message) { if (-not $Value) { throw $Message } }
function Refuses([scriptblock]$Body, [string]$Message = '') {
    $refused = $false
    try { & $Body | Out-Null }
    catch {
        if ($Message -and $_.Exception.Message -notlike "*$Message*") { throw }
        $refused = $true
    }
    Assert $refused 'Expected an exact fail-closed refusal.'
}
function Case([string]$Name, [scriptblock]$Body) {
    try { & $Body }
    catch { Write-Output "FAIL $Name`n$($_.ScriptStackTrace)"; throw }
    $script:cases += $Name
    Write-Output "PASS $Name"
}
function Put16([byte[]]$Bytes, [int]$Offset, [uint16]$Value) { [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset) }
function Put32([byte[]]$Bytes, [int]$Offset, [uint32]$Value) { [BitConverter]::GetBytes($Value).CopyTo($Bytes, $Offset) }
function Pe([string[]]$Imports, [bool]$Library = $false) {
    # Explicitly synthetic PE metadata, not runnable HDC/library/driver bytes.
    $bytes = [byte[]]::new(4096)
    Put16 $bytes 0 0x5a4d; Put32 $bytes 0x3c 128
    Put32 $bytes 128 0x4550; Put16 $bytes 132 0x8664; Put16 $bytes 134 1
    Put16 $bytes 148 240; Put16 $bytes 150 $(if ($Library) { 0x2002 } else { 2 })
    Put16 $bytes 152 0x20b; Put32 $bytes 212 512; Put32 $bytes 260 16
    Put32 $bytes 272 0x1000; Put32 $bytes 276 ([uint32](20 * ($Imports.Count + 1)))
    Put32 $bytes 400 3584; Put32 $bytes 404 0x1000; Put32 $bytes 408 3584; Put32 $bytes 412 512
    $next = 20 * ($Imports.Count + 1)
    for ($i = 0; $i -lt $Imports.Count; $i++) {
        Put32 $bytes (512 + 20 * $i + 12) ([uint32](0x1000 + $next))
        $name = [Text.Encoding]::ASCII.GetBytes($Imports[$i])
        $name.CopyTo($bytes, 512 + $next)
        $next += $name.Length + 1
    }
    return ,$bytes
}
function Inputs([string]$Name, [string[]]$Imports = @('libusb_shared.dll', 'KERNEL32.dll'), [string[]]$UsbImports = @('WINUSB.dll', 'api-ms-win-crt-runtime-l1-1-0.dll')) {
    $source = Join-Path $fixture $Name
    [void](New-Item -ItemType Directory -Path $source)
    [IO.File]::WriteAllBytes((Join-Path $source 'hdc.exe'), (Pe $Imports))
    [IO.File]::WriteAllBytes((Join-Path $source 'libusb_shared.dll'), (Pe $UsbImports $true))
    [IO.File]::WriteAllText((Join-Path $source 'NOTICE.txt'), 'Synthetic software-test notice; not a redistribution license.')
    $tools = Join-Path $fixture ($Name + '-tools')
    [void](New-Item -ItemType Directory -Path $tools)
    return [pscustomobject]@{ source = $source; tools = $tools; hdc = Join-Path $source 'hdc.exe' }
}
function Stage($Inputs) { return @(Copy-ArkForgeHdcPackage -HdcPath $Inputs.hdc -ToolsPath $Inputs.tools) }
function NoCopy($Inputs, [scriptblock]$Body, [string]$Message = '') {
    Refuses $Body $Message
    Assert (@(Get-ChildItem -LiteralPath $Inputs.tools -Force).Count -eq 0) 'Input refusal must precede every copy.'
}
function Tree([string]$Root) {
    return (@(Get-ChildItem -LiteralPath $Root -File -Recurse | Sort-Object FullName | ForEach-Object {
        $_.FullName.Substring($Root.Length + 1) + ':' + (Get-FileHash -LiteralPath $_.FullName).Hash
    }) -join "`n")
}

Case 'dynamic HDC copies only the exact dependency and whole notice' {
    $p = Inputs 'dynamic'
    [IO.File]::WriteAllText((Join-Path $p.source 'unrelated.dll'), 'must not be copied')
    $facts = Stage $p
    Assert (($facts.path -join ',') -ceq 'tools/hdc.exe,tools/libusb_shared.dll,tools/NOTICE.txt') 'Exact dynamic closure required.'
    foreach ($fact in $facts) {
        $name = [IO.Path]::GetFileName($fact.path)
        Assert ($fact.sha256 -ceq (Get-FileHash -LiteralPath (Join-Path $p.source $name)).Hash.ToLowerInvariant()) 'Full original source SHA required.'
        Assert ($fact.sha256 -ceq (Get-FileHash -LiteralPath (Join-Path $p.tools $name)).Hash.ToLowerInvariant()) 'Whole copied bytes must equal source.'
        Assert ($fact.bytes -eq (Get-Item -LiteralPath (Join-Path $p.tools $name)).Length) 'Whole copied count required.'
    }
    Assert (@(Get-ChildItem -LiteralPath $p.tools).Count -eq 3) 'No unrelated source directory file copied.'
}
Case 'system-only HDC does not borrow an unused source DLL' {
    $p = Inputs 'static' @('KERNEL32.dll')
    Assert (((Stage $p).path -join ',') -ceq 'tools/hdc.exe,tools/NOTICE.txt') 'System-only tool has no app-local dependency.'
}
Case 'missing libusb refuses before every output' {
    $p = Inputs 'missing-usb'; [IO.File]::Delete((Join-Path $p.source 'libusb_shared.dll'))
    NoCopy $p { Stage $p }
}
Case 'missing source notice refuses before every output' {
    $p = Inputs 'missing-notice'; [IO.File]::Delete((Join-Path $p.source 'NOTICE.txt'))
    NoCopy $p { Stage $p }
}
Case 'oversized source notice refuses' {
    $p = Inputs 'huge-notice'; [IO.File]::WriteAllBytes((Join-Path $p.source 'NOTICE.txt'), [byte[]]::new(2097153))
    NoCopy $p { Stage $p } 'bounded'
}
Case 'all input length bounds precede whole hashing' {
    function Assert-BoundedHashOrder([string]$Path) {
        & $module {
            param($Path)
            $script:BoundedSavedFacts = (Get-Item Function:Get-StreamFacts).ScriptBlock
            try {
                Set-Item Function:script:Get-StreamFacts {
                    param([IO.Stream]$Stream)
                    $limit = if ([IO.Path]::GetFileName($Stream.Name) -ceq 'NOTICE.txt') { 2097152 } else { 67108864 }
                    if ($Stream.Length -gt $limit) { throw 'Oversized source was hashed before its input bound.' }
                    & $script:BoundedSavedFacts $Stream
                }
                Open-HdcInputs $Path ''
            }
            finally {
                Set-Item Function:script:Get-StreamFacts $script:BoundedSavedFacts
                Remove-Variable BoundedSavedFacts -Scope Script
            }
        } $Path
    }
    foreach ($name in @('hdc.exe', 'libusb_shared.dll', 'NOTICE.txt')) {
        $p = Inputs ('oversized-' + $name)
        $stream = [IO.File]::OpenWrite((Join-Path $p.source $name))
        try { $stream.SetLength($(if ($name -ceq 'NOTICE.txt') { 2097153 } else { 67108865 })) }
        finally { $stream.Dispose() }
        NoCopy $p { Assert-BoundedHashOrder $p.hdc } 'bounded'
    }
}
Case 'unexpected HDC vendor dependency refuses' {
    $p = Inputs 'vendor' @('libusb_shared.dll', 'vendor.dll')
    NoCopy $p { Stage $p } 'system closure'
}
Case 'libusb cannot introduce another app-local dependency' {
    $p = Inputs 'transitive' @('libusb_shared.dll') @('helper.dll')
    NoCopy $p { Stage $p } 'system closure'
}
Case 'libusb self dependency and invented CRT contract refuse' {
    $p = Inputs 'cycle' @('libusb_shared.dll') @('libusb_shared.dll')
    NoCopy $p { Stage $p } 'system closure'
    $p = Inputs 'unknown-crt' @('api-ms-win-crt-invented-l1-1-0.dll')
    NoCopy $p { Stage $p } 'system closure'
}
Case 'duplicate PE dependency names refuse' {
    $p = Inputs 'duplicate' @('libusb_shared.dll', 'LIBUSB_SHARED.DLL')
    NoCopy $p { Stage $p } 'duplicated'
}
Case 'wrong HDC architecture refuses' {
    $p = Inputs 'arm'; $bytes = [IO.File]::ReadAllBytes($p.hdc); Put16 $bytes 132 0xaa64; [IO.File]::WriteAllBytes($p.hdc, $bytes)
    NoCopy $p { Stage $p } 'AMD64'
}
Case 'wrong dependency architecture refuses' {
    $p = Inputs 'x86-usb'; $path = Join-Path $p.source 'libusb_shared.dll'; $bytes = [IO.File]::ReadAllBytes($path); Put16 $bytes 132 0x14c; [IO.File]::WriteAllBytes($path, $bytes)
    NoCopy $p { Stage $p } 'AMD64'
}
Case 'PE executable and DLL roles cannot be relabelled' {
    $p = Inputs 'wrong-kind'; [IO.File]::WriteAllBytes((Join-Path $p.source 'libusb_shared.dll'), (Pe @('KERNEL32.dll')))
    NoCopy $p { Stage $p } 'kind mismatch'
}
Case 'malformed and delayed dependency directories refuse' {
    $p = Inputs 'rva'; $bytes = [IO.File]::ReadAllBytes($p.hdc); Put32 $bytes 272 ([uint32]::MaxValue); [IO.File]::WriteAllBytes($p.hdc, $bytes)
    NoCopy $p { Stage $p } 'RVA'
    $p = Inputs 'delay'; $bytes = [IO.File]::ReadAllBytes($p.hdc); Put32 $bytes 368 1; [IO.File]::WriteAllBytes($p.hdc, $bytes)
    NoCopy $p { Stage $p } 'Delayed'
}
Case 'reparse source ancestry refuses without copying' {
    $p = Inputs 'real-source'
    $link = Join-Path $fixture 'source-junction'; [void](New-Item -ItemType Junction -Path $link -Target $p.source)
    NoCopy $p { Copy-ArkForgeHdcPackage -HdcPath (Join-Path $link 'hdc.exe') -ToolsPath $p.tools } 'reparse'
}
Case 'reparse staging ancestry refuses' {
    $p = Inputs 'real-stage'
    $link = Join-Path $fixture 'stage-junction'; [void](New-Item -ItemType Junction -Path $link -Target $p.tools)
    NoCopy $p { Copy-ArkForgeHdcPackage -HdcPath $p.hdc -ToolsPath $link } 'reparse'
}
Case 'fresh output exclusive creation preserves all existing bytes' {
    $p = Inputs 'retained'; [IO.File]::WriteAllText((Join-Path $p.tools 'hdc.exe'), 'original output')
    $before = Tree $p.tools
    Refuses { Stage $p } 'fresh and empty'
    Assert ((Tree $p.tools) -ceq $before) 'Existing output must stay byte-exact.'
}
Case 'held source rejects write and rename before whole copy' {
    $p = Inputs 'held'
    $held = & $module { param($Path) Open-HdcInputs $Path '' } $p.hdc
    try {
        Refuses { [IO.File]::WriteAllText($p.hdc, 'tampered') }
        Refuses { [IO.File]::Move($p.hdc, $p.hdc + '.moved') }
        & $module { param($File, $Target) Copy-HdcInput $File $Target } $held[0] (Join-Path $p.tools 'hdc.exe')
        Assert ((Get-FileHash -LiteralPath (Join-Path $p.tools 'hdc.exe')).Hash.ToLowerInvariant() -ceq $held[0].facts.sha256) 'Retained exact source copied.'
    } finally { foreach ($file in $held) { $file.stream.Dispose() } }
}
Case 'source digest drift refuses before exclusive output creation' {
    $p = Inputs 'digest-drift'
    $held = & $module { param($Path) Open-HdcInputs $Path '' } $p.hdc
    try {
        $held[0].facts.sha256 = '0' * 64
        NoCopy $p { & $module { param($File, $Target) Copy-HdcInput $File $Target } $held[0] (Join-Path $p.tools 'hdc.exe') } 'drift'
    } finally { foreach ($file in $held) { $file.stream.Dispose() } }
}
Case 'unsigned staged metadata cannot pass the release signature gate' {
    $p = Inputs 'unsigned'; $facts = Stage $p
    Refuses { Assert-ArkForgeHdcPackage (Split-Path -Parent $p.tools) ('0' * 40) $facts }
    # The exact tools child is required; do not accidentally accept other roots.
    $root = Join-Path $fixture 'unsigned-package'; [void](New-Item -ItemType Directory -Path $root)
    [IO.Directory]::Move($p.tools, (Join-Path $root 'tools'))
    Refuses { Assert-ArkForgeHdcPackage $root ('0' * 40) $facts } 'release signature'
}
Case 'signed host fixture enforces signer inventory and whole manifest bytes' {
    # Real signed Windows bytes provide cryptographic gate coverage; they are
    # never executed, never a release HDC, and never hardware evidence.
    $root = Join-Path $fixture 'signed'; $tools = Join-Path $root 'tools'
    [void](New-Item -ItemType Directory -Path $tools)
    $hostImage = Join-Path $env:SystemRoot 'System32\where.exe'
    [IO.File]::Copy($hostImage, (Join-Path $tools 'hdc.exe'))
    [IO.File]::WriteAllText((Join-Path $tools 'NOTICE.txt'), 'Public system-image software fixture notice.')
    $signer = (Get-AuthenticodeSignature -LiteralPath $hostImage).SignerCertificate.Thumbprint
    $facts = @(@('hdc.exe', 'NOTICE.txt') | ForEach-Object { [pscustomobject]@{ path = 'tools/' + $_; sha256 = (Get-FileHash -LiteralPath (Join-Path $tools $_)).Hash.ToLowerInvariant(); bytes = (Get-Item -LiteralPath (Join-Path $tools $_)).Length } })
    Assert-ArkForgeHdcPackage $root $signer $facts
    Refuses { Assert-ArkForgeHdcPackage $root ('0' * 40) $facts } 'release signature'
    Refuses { Assert-ArkForgeHdcPackage $root $signer @($facts[0]) } 'signed package manifest'
    Refuses { Assert-ArkForgeHdcPackage $root $signer @($facts + $facts[0]) } 'signed package manifest'
    $wrong = $facts | ConvertTo-Json | ConvertFrom-Json; $wrong[0].sha256 = '0' * 64
    Refuses { Assert-ArkForgeHdcPackage $root $signer $wrong } 'signed package manifest'
    $wrong = $facts | ConvertTo-Json | ConvertFrom-Json; $wrong[1].bytes = [string]$wrong[1].bytes
    Refuses { Assert-ArkForgeHdcPackage $root $signer $wrong } 'signed package manifest'
    [IO.File]::WriteAllText((Join-Path $tools 'extra.dll'), 'undeclared')
    Refuses { Assert-ArkForgeHdcPackage $root $signer $facts } 'undeclared'
}
Case 'production consumers validate signed closure before installation or launch' {
    $package = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'package-arkforge.ps1'))
    Assert ($package.Contains('Copy-ArkForgeHdcPackage') -and $package.Contains("'tools\libusb_shared.dll'")) 'Producer must include exact dynamic dependency.'
    Assert ($package.IndexOf('Copy-ArkForgeHdcPackage') -lt $package.IndexOf("'build', '--manifest-path'")) 'Missing source closure must fail before Cargo.'
    Assert ($package.IndexOf('Assert-ArkForgeHdcPackage') -lt $package.IndexOf('$trustedJson =')) 'Complete signed closure checked before sealing manifest.'
    $install = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Install-ArkForge.ps1'))
    Assert ($install.IndexOf('Assert-ArkForgeHdcPackage') -lt $install.IndexOf('$pnputil =')) 'Install must reject closure before driver action.'
    $accept = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Test-ArkForgePackage.ps1'))
    Assert ($accept.IndexOf('Assert-ArkForgeHdcPackage') -lt $accept.IndexOf('$hdcProbe =')) 'Acceptance must reject closure before the HDC self-test.'
    Assert ($package.Contains("'ReleaseBundle.psm1'")) 'The closure validator itself is signed and manifest-bound.'
}

$result = [ordered]@{ schema = 'arkforge.hdc-package-software-tests/v1'; passed = $cases.Count; failed = 0; skipped = 0; cases = $cases; executedHdc = $false; hardwareEvidence = $false; productionAcceptance = $false }
if ($ReportPath) {
    $out = [IO.File]::Open([IO.Path]::GetFullPath($ReportPath), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($result | ConvertTo-Json -Depth 4) + "`n"); $out.Write($bytes, 0, $bytes.Length) }
    finally { $out.Dispose() }
}
$result | ConvertTo-Json -Depth 4
