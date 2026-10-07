Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The layout/closed roles are shared with ArkDeck's arkforge_bundle reader.
$script:ManifestRelativePath = 'Contents/Resources/arkforge-bundle.json'
$script:BundleMembers = @(
    [ordered]@{ path = 'bin/arkforge.exe'; role = 'cli' },
    [ordered]@{ path = 'bin/arkforged.exe'; role = 'daemon' },
    [ordered]@{ path = 'Contents/Resources/profiles/dayu200.yaml'; role = 'profile'; profileId = 'org.openharmony.dayu200' }
)

function Assert-PlainBundlePath([string]$Path, [bool]$Directory = $false) {
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $item.PSIsContainer -ne $Directory) {
        throw 'A release bundle input/member must be an ordinary file or directory, never a reparse point.'
    }
    return $item
}

function Get-StreamFacts([IO.Stream]$Stream) {
    $Stream.Position = 0
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash($Stream)
        return [ordered]@{
            sha256 = ([BitConverter]::ToString($digest)).Replace('-', '').ToLowerInvariant()
            bytes = $Stream.Length
        }
    }
    finally { $sha.Dispose() }
}

function Assert-SystemImageDependencies([string[]]$Libraries) {
    # The bundle schema has no dependency role. Windows supplies this closed
    # set; a CRT/vendor/app-local DLL must not be borrowed from the build host.
    # This exact API set is Windows' documented WaitOnAddress/WakeByAddress
    # library, not a wildcard allowance for arbitrary api-ms-* dependencies.
    $system = @('api-ms-win-core-synch-l1-2-0.dll', 'advapi32.dll', 'bcrypt.dll', 'bcryptprimitives.dll', 'cfgmgr32.dll',
        'combase.dll', 'crypt32.dll', 'dbghelp.dll', 'gdi32.dll', 'imm32.dll',
        'iphlpapi.dll', 'kernel32.dll', 'msvcrt.dll', 'ncrypt.dll', 'netapi32.dll',
        'ntdll.dll', 'ole32.dll', 'oleaut32.dll', 'powrprof.dll', 'propsys.dll',
        'psapi.dll', 'rpcrt4.dll', 'secur32.dll', 'setupapi.dll', 'shell32.dll',
        'shlwapi.dll', 'user32.dll', 'userenv.dll', 'ucrtbase.dll', 'version.dll',
        'winhttp.dll', 'wintrust.dll', 'winusb.dll', 'ws2_32.dll', 'wtsapi32.dll')
    foreach ($library in $Libraries) {
        if ($library.ToLowerInvariant() -notin $system) {
            throw 'The release image imports a dependency outside the Windows system closure.'
        }
    }
}

function Get-ImageImportedLibraries([IO.Stream]$Stream) {
    $reader = [IO.BinaryReader]::new($Stream, [Text.Encoding]::ASCII, $true)
    try {
        $Stream.Position = 0x3c
        $pe = [long]$reader.ReadUInt32()
        $Stream.Position = $pe + 6
        $sectionCount = $reader.ReadUInt16()
        $Stream.Position = $pe + 20
        $optionalBytes = $reader.ReadUInt16()
        $optional = $pe + 24
        if ($sectionCount -lt 1 -or $sectionCount -gt 96 -or $optionalBytes -lt 240 -or
            $optional + $optionalBytes + 40 * $sectionCount -gt $Stream.Length) { throw 'Invalid PE section table.' }
        $Stream.Position = $optional + 108
        if ($reader.ReadUInt32() -lt 14) { throw 'Missing PE dependency directory.' }
        $Stream.Position = $optional + 60
        $headerBytes = $reader.ReadUInt32()
        $sections = @()
        for ($index = 0; $index -lt $sectionCount; $index++) {
            $Stream.Position = $optional + $optionalBytes + 40 * $index + 8
            $sections += [ordered]@{
                virtualBytes = $reader.ReadUInt32(); rva = $reader.ReadUInt32()
                rawBytes = $reader.ReadUInt32(); rawOffset = $reader.ReadUInt32()
            }
        }
        $locate = {
            param([long]$Rva, [long]$Length)
            if ($Rva -ge 0 -and $Rva + $Length -le $headerBytes -and $Rva + $Length -le $Stream.Length) { return $Rva }
            foreach ($section in $sections) {
                $delta = $Rva - [long]$section.rva
                if ($delta -ge 0 -and $delta + $Length -le $section.rawBytes -and
                    [long]$section.rawOffset + $delta + $Length -le $Stream.Length) { return [long]$section.rawOffset + $delta }
            }
            throw 'PE dependency RVA is outside whole file bytes.'
        }
        $Stream.Position = $optional + 216
        if ($reader.ReadUInt32() -ne 0 -or $reader.ReadUInt32() -ne 0) {
            throw 'Delayed dependency loading is not part of the release closure.'
        }
        $Stream.Position = $optional + 120
        $importsRva = $reader.ReadUInt32()
        $importsBytes = $reader.ReadUInt32()
        if ($importsRva -eq 0 -and $importsBytes -eq 0) { return @() }
        if ($importsRva -eq 0 -or $importsBytes -lt 20 -or $importsBytes -gt 1048576) { throw 'Invalid PE dependency directory.' }
        $names = @()
        $terminated = $false
        for ($index = 0; $index -lt 128; $index++) {
            if (20 * ($index + 1) -gt $importsBytes) { break }
            $Stream.Position = & $locate ([long]$importsRva + 20 * $index) 20
            $descriptor = @(0..4 | ForEach-Object { $reader.ReadUInt32() })
            if (@($descriptor | Where-Object { $_ -ne 0 }).Count -eq 0) { $terminated = $true; break }
            $nameRva = $descriptor[3]
            if ($nameRva -eq 0) { throw 'Missing PE dependency name.' }
            $name = ''
            for ($offset = 0; $offset -lt 260; $offset++) {
                $Stream.Position = & $locate ([long]$nameRva + $offset) 1
                $byte = $reader.ReadByte()
                if ($byte -eq 0) { break }
                if ($byte -lt 0x21 -or $byte -gt 0x7e) { throw 'Invalid PE dependency name.' }
                $name += [char]$byte
            }
            if ($offset -eq 260 -or $name -notmatch '^[A-Za-z0-9_.-]+\.dll$' -or $name -in $names) { throw 'Invalid or duplicated PE dependency name.' }
            $names += $name
        }
        if (-not $terminated) { throw 'Unterminated PE dependency directory.' }
        return $names
    }
    finally { $reader.Dispose(); $Stream.Position = 0 }
}

function Assert-ReleaseImage([string]$Path, [IO.Stream]$Stream, [string]$CertificateThumbprint) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or
        $null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Thumbprint -ine $CertificateThumbprint) {
        throw 'The bundle executable does not have the expected trusted release signature.'
    }
    # Both images are x64 native Windows executables. A signed script or another
    # architecture cannot be relabelled as the package's native executable.
    $reader = [IO.BinaryReader]::new($Stream, [Text.Encoding]::UTF8, $true)
    try {
        $Stream.Position = 0
        if ($Stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5a4d) { throw 'Invalid Windows release image.' }
        $Stream.Position = 0x3c
        $offset = $reader.ReadUInt32()
        if ($offset -gt $Stream.Length - 26) { throw 'Invalid Windows release image.' }
        $Stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) { throw 'The release image is not AMD64 PE.' }
        $Stream.Position = $offset + 24
        if ($reader.ReadUInt16() -ne 0x20b) { throw 'The release image is not PE32+.' }
    }
    finally { $reader.Dispose(); $Stream.Position = 0 }
    Assert-SystemImageDependencies @(Get-ImageImportedLibraries $Stream)
}

function Assert-Dayu200Profile([IO.Stream]$Stream) {
    $Stream.Position = 0
    $reader = [IO.StreamReader]::new($Stream, [Text.UTF8Encoding]::new($false, $true), $false, 4096, $true)
    try { $text = $reader.ReadToEnd() }
    finally { $reader.Dispose(); $Stream.Position = 0 }
    if ([regex]::Matches($text, '(?m)^schemaVersion: arkforge\.device-profile/v1\r?$').Count -ne 1 -or
        [regex]::Matches($text, '(?m)^profile:\r?$').Count -ne 1) {
        throw 'The published DAYU200 profile declaration is missing or ambiguous.'
    }
    $block = [regex]::Match($text, '(?m)^profile:\r?\n(?<fields>(?:[ \t].*(?:\r?\n|$)|\r?\n)*)').Groups['fields'].Value
    $ids = [regex]::Matches($block, '(?m)^\s+id: (?<value>[^\r\n]+)\r?$')
    $versions = [regex]::Matches($block, '(?m)^\s+version: (?<value>[^\r\n]+)\r?$')
    if ($ids.Count -ne 1 -or $ids[0].Groups['value'].Value -cne 'org.openharmony.dayu200' -or
        $versions.Count -ne 1 -or $versions[0].Groups['value'].Value -cne '1.0.0') {
        throw 'The bundle requires the published org.openharmony.dayu200@1.0.0 profile.'
    }
}

function Get-ArkForgeReleaseBundleMembers {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][string]$BundleRoot)
    $BundleRoot = (Assert-PlainBundlePath $BundleRoot $true).FullName.TrimEnd('\')
    $expected = @($script:BundleMembers | ForEach-Object { $_.path })
    $observed = @()
    foreach ($item in Get-ChildItem -LiteralPath $BundleRoot -Force -Recurse) {
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Bundle reparse point refused.' }
        if ($item.PSIsContainer) { continue }
        $relative = $item.FullName.Substring($BundleRoot.Length + 1).Replace('\', '/')
        if ($relative -cne $script:ManifestRelativePath -and $relative -cnotin $expected) { throw 'Undeclared release bundle member.' }
        $observed += $relative
    }
    $members = @()
    foreach ($member in $script:BundleMembers) {
        if ($member.path -cnotin $observed) { throw 'Missing release bundle member.' }
        $path = Join-Path $BundleRoot $member.path
        [void](Assert-PlainBundlePath $path)
        $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try { $facts = Get-StreamFacts $stream }
        finally { $stream.Dispose() }
        $fact = [ordered]@{ path = $member.path; sha256 = $facts.sha256; bytes = $facts.bytes; role = $member.role }
        if ($member.Contains('profileId')) { $fact.profileId = $member.profileId }
        $members += $fact
    }
    return $members
}

function New-ArkForgeReleaseBundle {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$PackageRoot,
        [Parameter(Mandatory = $true)][string]$ProfilePath,
        [Parameter(Mandatory = $true)][ValidatePattern('^[0-9A-Fa-f]{40}$')][string]$CertificateThumbprint,
        [Parameter(Mandatory = $true)][ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+$')][string]$Version
    )
    $PackageRoot = (Assert-PlainBundlePath $PackageRoot $true).FullName
    [void](Assert-PlainBundlePath (Join-Path $PackageRoot 'bin') $true)
    $bundle = Join-Path $PackageRoot 'ArkForge.release-bundle'
    if (Test-Path -LiteralPath $bundle) { throw 'Refusing to overwrite an existing release bundle.' }
    $inputs = @(
        (Join-Path $PackageRoot 'bin/arkforge.exe'),
        (Join-Path $PackageRoot 'bin/arkforged.exe'),
        (Assert-PlainBundlePath $ProfilePath).FullName
    )
    # Keep all three source handles read-only with writes/deletes denied until
    # the complete inventory has been remeasured. Sign before this function;
    # it never re-signs or mutates the selected image/profile bytes.
    $streams = @()
    $copiedStreams = @()
    try {
        foreach ($sourcePath in $inputs) {
            [void](Assert-PlainBundlePath $sourcePath)
            $streams += [IO.File]::Open($sourcePath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        }
        Assert-ReleaseImage $inputs[0] $streams[0] $CertificateThumbprint
        Assert-ReleaseImage $inputs[1] $streams[1] $CertificateThumbprint
        Assert-Dayu200Profile $streams[2]
        $original = @($streams | ForEach-Object { Get-StreamFacts $_ })
        [void](New-Item -ItemType Directory -Path $bundle -ErrorAction Stop)
        for ($index = 0; $index -lt $script:BundleMembers.Count; $index++) {
            $destination = Join-Path $bundle $script:BundleMembers[$index].path
            [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
            $output = [IO.File]::Open($destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $streams[$index].Position = 0; $streams[$index].CopyTo($output); $output.Flush($true) }
            finally { $output.Dispose() }
            $copiedStreams += [IO.File]::Open($destination, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        }
        $members = @(Get-ArkForgeReleaseBundleMembers $bundle)
        for ($index = 0; $index -lt $members.Count; $index++) {
            $fresh = Get-StreamFacts $streams[$index]
            if ($fresh.sha256 -ne $original[$index].sha256 -or $fresh.bytes -ne $original[$index].bytes -or
                $members[$index].sha256 -ne $original[$index].sha256 -or $members[$index].bytes -ne $original[$index].bytes) {
                throw 'A release bundle member changed during its byte-exact copy.'
            }
        }
        Assert-ReleaseImage (Join-Path $bundle 'bin/arkforge.exe') $copiedStreams[0] $CertificateThumbprint
        Assert-ReleaseImage (Join-Path $bundle 'bin/arkforged.exe') $copiedStreams[1] $CertificateThumbprint
        $manifest = [ordered]@{ schema = 'arkforge.release-bundle/v1'; version = $Version; members = $members }
        $json = ($manifest | ConvertTo-Json -Depth 6) + "`n"
        $manifestPath = Join-Path $bundle $script:ManifestRelativePath
        $output = [IO.File]::Open($manifestPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json)
            $output.Write($bytes, 0, $bytes.Length)
            $output.Flush($true)
        }
        finally { $output.Dispose() }
        # Re-enumerate after publication; only the manifest and the three closed
        # roles may exist. The signed outer package manifest binds these bytes.
        $published = @(Get-ArkForgeReleaseBundleMembers $bundle)
        for ($index = 0; $index -lt $published.Count; $index++) {
            if ($published[$index].sha256 -ne $original[$index].sha256 -or $published[$index].bytes -ne $original[$index].bytes) {
                throw 'Published release bundle bytes changed.'
            }
        }
        return [ordered]@{
            root = $bundle
            manifestSha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
            members = $members
        }
    }
    finally { foreach ($stream in @($streams + $copiedStreams)) { $stream.Dispose() } }
}

function Assert-HdcPath([string]$Path, [bool]$Directory = $false) {
    $full = [IO.Path]::GetFullPath($Path)
    [void](Assert-PlainBundlePath $full $Directory)
    $parent = [IO.Directory]::GetParent($full)
    while ($null -ne $parent) {
        [void](Assert-PlainBundlePath $parent.FullName $true)
        $parent = $parent.Parent
    }
    return $full
}

function Assert-HdcImage([IO.Stream]$Stream, [bool]$Library) {
    $reader = [IO.BinaryReader]::new($Stream, [Text.Encoding]::ASCII, $true)
    try {
        $Stream.Position = 0
        if ($Stream.Length -lt 64 -or $Stream.Length -gt 67108864 -or $reader.ReadUInt16() -ne 0x5a4d) { throw 'Invalid bounded HDC PE image.' }
        $Stream.Position = 0x3c
        $pe = [long]$reader.ReadUInt32()
        if ($pe -gt $Stream.Length - 26) { throw 'Invalid HDC PE header.' }
        $Stream.Position = $pe
        if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) { throw 'HDC package images must be AMD64.' }
        $Stream.Position = $pe + 22
        $flags = $reader.ReadUInt16()
        if (($flags -band 2) -eq 0 -or (($flags -band 0x2000) -ne 0) -ne $Library) { throw 'HDC executable/library PE kind mismatch.' }
        $Stream.Position = $pe + 24
        if ($reader.ReadUInt16() -ne 0x20b) { throw 'HDC package images must be PE32+.' }
    }
    finally { $reader.Dispose(); $Stream.Position = 0 }
    $imports = @(Get-ImageImportedLibraries $Stream)
    # HDC's official Windows SDK build uses the OS Universal CRT API set.
    # This HDC-only list does not extend the static-CRT ArkForge bundle closure.
    $ucrt = @('api-ms-win-crt-convert-l1-1-0.dll', 'api-ms-win-crt-environment-l1-1-0.dll',
        'api-ms-win-crt-filesystem-l1-1-0.dll', 'api-ms-win-crt-heap-l1-1-0.dll',
        'api-ms-win-crt-locale-l1-1-0.dll', 'api-ms-win-crt-math-l1-1-0.dll',
        'api-ms-win-crt-multibyte-l1-1-0.dll', 'api-ms-win-crt-private-l1-1-0.dll',
        'api-ms-win-crt-runtime-l1-1-0.dll', 'api-ms-win-crt-stdio-l1-1-0.dll',
        'api-ms-win-crt-string-l1-1-0.dll', 'api-ms-win-crt-time-l1-1-0.dll',
        'api-ms-win-crt-utility-l1-1-0.dll')
    foreach ($name in $imports) {
        if (-not $Library -and $name -ieq 'libusb_shared.dll') { continue }
        if ($name.ToLowerInvariant() -in $ucrt) { continue }
        Assert-SystemImageDependencies @($name)
    }
    return $imports
}

function Open-HdcInputs([string]$HdcPath, [string]$NoticePath) {
    $held = @()
    try {
        $hdc = Assert-HdcPath $HdcPath
        $stream = [IO.File]::Open($hdc, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        $held += [ordered]@{ path = 'tools/hdc.exe'; source = $hdc; stream = $stream; facts = Get-StreamFacts $stream }
        $imports = @(Assert-HdcImage $stream $false)
        if ('libusb_shared.dll' -in $imports) {
            $usb = Assert-HdcPath (Join-Path ([IO.Path]::GetDirectoryName($hdc)) 'libusb_shared.dll')
            $stream = [IO.File]::Open($usb, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
            $held += [ordered]@{ path = 'tools/libusb_shared.dll'; source = $usb; stream = $stream; facts = Get-StreamFacts $stream }
            [void](Assert-HdcImage $stream $true)
        }
        if (-not $NoticePath) { $NoticePath = Join-Path ([IO.Path]::GetDirectoryName($hdc)) 'NOTICE.txt' }
        $notice = Assert-HdcPath $NoticePath
        $stream = [IO.File]::Open($notice, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        $held += [ordered]@{ path = 'tools/NOTICE.txt'; source = $notice; stream = $stream; facts = Get-StreamFacts $stream }
        if ($stream.Length -lt 1 -or $stream.Length -gt 2097152) { throw 'HDC notice must be bounded whole source bytes.' }
        return ,$held
    }
    catch {
        foreach ($heldFile in $held) { $heldFile.stream.Dispose() }
        throw
    }
}

function Copy-HdcInput($InputFile, [string]$Destination) {
    # Every source handle denies write/delete until the complete copy is checked.
    [void](Assert-HdcPath $InputFile.source)
    $before = Get-StreamFacts $InputFile.stream
    if ($before.sha256 -cne $InputFile.facts.sha256 -or $before.bytes -ne $InputFile.facts.bytes) { throw 'HDC source drift before copy.' }
    $output = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        $InputFile.stream.Position = 0
        $InputFile.stream.CopyTo($output, 16384)
        $output.Flush($true)
        $copied = Get-StreamFacts $output
        $source = Get-StreamFacts $InputFile.stream
        [void](Assert-HdcPath $InputFile.source)
        if ($copied.sha256 -cne $before.sha256 -or $copied.bytes -ne $before.bytes -or
            $source.sha256 -cne $before.sha256 -or $source.bytes -ne $before.bytes) { throw 'Whole HDC source/copy bytes drifted.' }
    }
    finally { $output.Dispose() }
}

function Copy-ArkForgeHdcPackage([string]$HdcPath, [string]$ToolsPath, [string]$NoticePath = '') {
    $tools = Assert-HdcPath $ToolsPath $true
    if (@(Get-ChildItem -LiteralPath $tools -Force).Count -ne 0) { throw 'HDC staging directory must be fresh and empty.' }
    $held = Open-HdcInputs $HdcPath $NoticePath
    try {
        foreach ($heldFile in $held) { Copy-HdcInput $heldFile (Join-Path $tools ([IO.Path]::GetFileName($heldFile.path))) }
        return @($held | ForEach-Object { [ordered]@{ path = $_.path; sha256 = $_.facts.sha256; bytes = $_.facts.bytes } })
    }
    finally { foreach ($heldFile in $held) { $heldFile.stream.Dispose() } }
}

function Assert-ArkForgeHdcPackage([string]$PackageRoot, [string]$CertificateThumbprint, [object[]]$ManifestFiles) {
    $tools = Assert-HdcPath (Join-Path $PackageRoot 'tools') $true
    $held = Open-HdcInputs (Join-Path $tools 'hdc.exe') (Join-Path $tools 'NOTICE.txt')
    try {
        $names = @($held | ForEach-Object { [IO.Path]::GetFileName($_.path) })
        $entries = @(Get-ChildItem -LiteralPath $tools -Force)
        if ($entries.Count -ne $names.Count -or @($entries | Where-Object { $_.PSIsContainer -or $_.Name -cnotin $names }).Count -ne 0) { throw 'HDC tools contain undeclared members.' }
        foreach ($heldFile in $held) {
            if (-not $heldFile.path.EndsWith('/NOTICE.txt', [StringComparison]::Ordinal)) {
                $signature = Get-AuthenticodeSignature -LiteralPath $heldFile.source
                if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or $null -eq $signature.SignerCertificate -or
                    $signature.SignerCertificate.Thumbprint -ine $CertificateThumbprint) { throw 'HDC package release signature mismatch.' }
            }
            $boundFacts = @($ManifestFiles | Where-Object { $_.path -ceq $heldFile.path })
            if ($boundFacts.Count -ne 1 -or $boundFacts[0].sha256 -cnotmatch '^[0-9a-f]{64}$' -or
                ($boundFacts[0].bytes -isnot [int] -and $boundFacts[0].bytes -isnot [long]) -or
                $boundFacts[0].sha256 -cne $heldFile.facts.sha256 -or
                $boundFacts[0].bytes -ne $heldFile.facts.bytes) { throw 'HDC member is not wholly bound by the signed package manifest.' }
        }
    }
    finally { foreach ($heldFile in $held) { $heldFile.stream.Dispose() } }
}

Export-ModuleMember -Function New-ArkForgeReleaseBundle, Get-ArkForgeReleaseBundleMembers, Copy-ArkForgeHdcPackage, Assert-ArkForgeHdcPackage
