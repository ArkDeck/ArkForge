[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$CertificateThumbprint,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^https://')]
    [string]$TimestampUrl,

    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string]$HdcPath,

    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string]$DriverPackageDirectory,

    [string]$OutputDirectory = '',
    [string]$SignTool = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-RequiredTool([string]$ExplicitPath, [string]$CommandName) {
    if ($ExplicitPath) {
        if (-not (Test-Path -LiteralPath $ExplicitPath -PathType Leaf)) {
            throw "$CommandName was not found at $ExplicitPath"
        }
        return (Resolve-Path -LiteralPath $ExplicitPath).Path
    }
    $command = Get-Command $CommandName -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        throw "$CommandName is required; install the Windows SDK or pass its explicit path."
    }
    return $command.Source
}

function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program failed with exit code $LASTEXITCODE"
    }
}

function Get-RelativeFileFacts([string]$Root) {
    $rootPath = (Resolve-Path -LiteralPath $Root).Path.TrimEnd('\')
    return @(Get-ChildItem -LiteralPath $Root -File -Recurse | Sort-Object FullName | ForEach-Object {
        [ordered]@{
            path = $_.FullName.Substring($rootPath.Length + 1).Replace('\', '/')
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            bytes = $_.Length
        }
    })
}

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptRoot '..\..')).Path
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $repoRoot 'target\arkforge-windows-release'
}
$outputPath = [System.IO.Path]::GetFullPath($OutputDirectory)
$zipPath = "$outputPath.zip"
if ((Test-Path -LiteralPath $outputPath) -or (Test-Path -LiteralPath $zipPath)) {
    throw "Refusing to overwrite an existing release output: $outputPath or $zipPath"
}

$cargo = Resolve-RequiredTool '' 'cargo.exe'
$signToolPath = Resolve-RequiredTool $SignTool 'signtool.exe'
$driverPackagePath = (Resolve-Path -LiteralPath $DriverPackageDirectory).Path
$driverInf = Join-Path $driverPackagePath 'arkforge-rockusb.inf'
$driverCatalog = Join-Path $driverPackagePath 'arkforge-rockusb.cat'
if (-not (Test-Path -LiteralPath $driverInf -PathType Leaf) -or -not (Test-Path -LiteralPath $driverCatalog -PathType Leaf)) {
    throw 'DriverPackageDirectory must contain arkforge-rockusb.inf and arkforge-rockusb.cat.'
}
$canonicalInf = Join-Path $scriptRoot 'driver\arkforge-rockusb.inf'
if ((Get-FileHash -LiteralPath $driverInf -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $canonicalInf -Algorithm SHA256).Hash) {
    throw 'The signed driver INF does not match the canonical ArkForge WinUSB binding.'
}
Invoke-Checked $signToolPath @('verify', '/kp', '/all', '/v', $driverCatalog)
Invoke-Checked $signToolPath @('verify', '/kp', '/c', $driverCatalog, $driverInf)
$certificate = Get-ChildItem -Path @('Cert:\CurrentUser\My', 'Cert:\LocalMachine\My') |
    Where-Object { $_.Thumbprint -ieq $CertificateThumbprint } |
    Select-Object -First 1
if ($null -eq $certificate -or -not $certificate.HasPrivateKey) {
    throw "A code-signing certificate with private key $CertificateThumbprint was not found."
}
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("arkforge-package-" + [guid]::NewGuid().ToString('N'))

try {
    New-Item -ItemType Directory -Path (Join-Path $stage 'bin') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $stage 'tools') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $stage 'driver') -Force | Out-Null

    Invoke-Checked $cargo @(
        'build', '--locked', '--release', '--target', 'x86_64-pc-windows-msvc',
        '-p', 'arkforge-cli', '--bin', 'arkforge',
        '-p', 'arkforged', '--bin', 'arkforged'
    )
    $releaseRoot = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release'
    Copy-Item -LiteralPath (Join-Path $releaseRoot 'arkforge.exe') -Destination (Join-Path $stage 'bin\arkforge.exe')
    Copy-Item -LiteralPath (Join-Path $releaseRoot 'arkforged.exe') -Destination (Join-Path $stage 'bin\arkforged.exe')
    Copy-Item -LiteralPath (Resolve-Path -LiteralPath $HdcPath).Path -Destination (Join-Path $stage 'tools\hdc.exe')
    Copy-Item -LiteralPath $driverInf -Destination (Join-Path $stage 'driver\arkforge-rockusb.inf')
    Copy-Item -LiteralPath $driverCatalog -Destination (Join-Path $stage 'driver\arkforge-rockusb.cat')
    Copy-Item -LiteralPath (Join-Path $scriptRoot 'Install-ArkForge.ps1') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $scriptRoot 'Uninstall-ArkForge.ps1') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $scriptRoot 'Test-ArkForgePackage.ps1') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $scriptRoot 'README.md') -Destination $stage

    $signedFiles = @(
        (Join-Path $stage 'bin\arkforge.exe'),
        (Join-Path $stage 'bin\arkforged.exe'),
        (Join-Path $stage 'tools\hdc.exe')
    )
    foreach ($file in $signedFiles) {
        Invoke-Checked $signToolPath @(
            'sign', '/sha1', $CertificateThumbprint, '/fd', 'SHA256',
            '/tr', $TimestampUrl, '/td', 'SHA256', '/v', $file
        )
        Invoke-Checked $signToolPath @('verify', '/pa', '/all', '/v', $file)
    }
    foreach ($script in @('Install-ArkForge.ps1', 'Uninstall-ArkForge.ps1', 'Test-ArkForgePackage.ps1')) {
        $signature = Set-AuthenticodeSignature -FilePath (Join-Path $stage $script) -Certificate $certificate -HashAlgorithm SHA256 -TimestampServer $TimestampUrl
        if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
            throw "PowerShell signing failed for $script ($($signature.Status))."
        }
    }

    $hdcDigest = (Get-FileHash -LiteralPath (Join-Path $stage 'tools\hdc.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    $runtimeManifest = [ordered]@{
        schema = 'arkforge.windows-runtime/v1'
        version = '0.1.0'
        architecture = 'x86_64-pc-windows-msvc'
        arkforge = 'bin/arkforge.exe'
        arkforged = 'bin/arkforged.exe'
        hdc = [ordered]@{
            path = 'tools/hdc.exe'
            sha256 = $hdcDigest
            requireTrustedAuthenticode = $true
        }
        driver = [ordered]@{
            inf = 'driver/arkforge-rockusb.inf'
            catalog = 'driver/arkforge-rockusb.cat'
            hardwareIds = @('USB\VID_2207&PID_350A')
            interfaceGuid = '{6A4E21F0-50A4-4D7A-B71B-9E945B3F6B7B}'
        }
    }
    $runtimeManifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $stage 'arkforge-runtime.json') -Encoding UTF8

    # The JSON receipt is intentionally human/tooling metadata. Installation
    # trust comes from this independently Authenticode-signed manifest, which
    # binds every payload byte without a circular self-hash.
    $trustedManifest = [ordered]@{
        schema = 'arkforge.windows-trusted-manifest/v1'
        certificateThumbprint = $CertificateThumbprint.ToUpperInvariant()
        files = Get-RelativeFileFacts $stage
    }
    $trustedJson = $trustedManifest | ConvertTo-Json -Depth 8 -Compress
    $trustedBase64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($trustedJson))
    $trustedScript = @"
`$json = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$trustedBase64'))
`$json | ConvertFrom-Json
"@
    $trustedManifestPath = Join-Path $stage 'ArkForge.PackageManifest.ps1'
    $trustedScript | Set-Content -LiteralPath $trustedManifestPath -Encoding ASCII
    $trustedSignature = Set-AuthenticodeSignature -FilePath $trustedManifestPath -Certificate $certificate -HashAlgorithm SHA256 -TimestampServer $TimestampUrl
    if ($trustedSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "PowerShell signing failed for ArkForge.PackageManifest.ps1 ($($trustedSignature.Status))."
    }

    $receipt = [ordered]@{
        schema = 'arkforge.windows-package-receipt/v1'
        version = '0.1.0'
        target = 'x86_64-pc-windows-msvc'
        certificateThumbprint = $CertificateThumbprint.ToUpperInvariant()
        timestampUrl = $TimestampUrl
        files = Get-RelativeFileFacts $stage
        acceptance = [ordered]@{
            software = 'run Test-ArkForgePackage.ps1 after installation'
            hardware = 'requires Windows x64, a DAYU200 in Loader mode, and the packaged HDC'
            destructiveFlash = 'not performed by packaging or installation'
        }
    }
    $receipt | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $stage 'package-receipt.json') -Encoding UTF8

    New-Item -ItemType Directory -Path (Split-Path -Parent $outputPath) -Force | Out-Null
    Move-Item -LiteralPath $stage -Destination $outputPath
    Compress-Archive -Path (Join-Path $outputPath '*') -DestinationPath $zipPath -CompressionLevel Optimal
    Write-Output $outputPath
    Write-Output $zipPath
}
finally {
    if (Test-Path -LiteralPath $stage) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
}
