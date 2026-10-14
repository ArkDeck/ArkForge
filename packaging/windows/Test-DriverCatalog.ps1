[CmdletBinding()]
param(
    [string]$SignTool = '',
    [string]$Catalog = '',
    [string]$Inf = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Run only the producer's signature helpers against existing public driver
# bytes. Never evaluate the package body, install a driver, or open a device.
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'package-arkforge.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'The package producer does not parse.' }
foreach ($name in @('Invoke-Checked', 'Assert-DriverCatalog')) {
    $definitions = @($ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name
    }, $false))
    if ($definitions.Count -ne 1) { throw "Expected one producer helper: $name" }
    . ([scriptblock]::Create($definitions[0].Extent.Text))
}

if (-not $SignTool) {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
    $tools = @(Get-ChildItem -LiteralPath $kits -Directory | Where-Object { $_.Name -match '^10\.[0-9.]+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64/signtool.exe' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
    if ($tools.Count -eq 0) { throw 'Windows SDK x64 SignTool is required; no skipped pass.' }
    $SignTool = $tools[0]
}
if ([bool]$Catalog -ne [bool]$Inf) { throw 'Supply both Catalog and Inf, or neither.' }
if (-not $Catalog) {
    $store = Join-Path $env:windir 'System32/DriverStore/FileRepository'
    # The OS image supplies this cryptographic fixture; it need not be a USB
    # driver or currently bound to hardware. Keep its whole original bytes.
    $pairs = @(Get-ChildItem -LiteralPath $store -Directory | Sort-Object Name | ForEach-Object {
        $directory = $_.FullName
        $members = @(Get-ChildItem -LiteralPath $directory -Filter '*.inf' -File)
        foreach ($cat in @(Get-ChildItem -LiteralPath $directory -Filter '*.cat' -File)) {
            foreach ($member in $members) {
                [pscustomobject]@{ Catalog = $cat.FullName; Inf = $member.FullName }
            }
        }
    })
    if ($pairs.Count -eq 0) { throw 'No installed driver catalog/INF fixture exists; no skipped pass.' }
    $Catalog = $pairs[0].Catalog
    $Inf = $pairs[0].Inf
}

$beforeCatalog = (Get-FileHash -LiteralPath $Catalog -Algorithm SHA256).Hash
$beforeInf = (Get-FileHash -LiteralPath $Inf -Algorithm SHA256).Hash
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$fixture = Join-Path $repo ('.driver-catalog-fixture-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($fixture)
$passed = 0
function Expect-Refusal([scriptblock]$Body) {
    $refused = $false
    try { & $Body | Out-Null }
    catch { $refused = $true }
    if (-not $refused) { throw 'Invalid driver bytes were accepted.' }
}
try {
    Assert-DriverCatalog $SignTool $Catalog $Inf
    $passed++
    Write-Output 'PASS native kernel-policy catalog and exact INF membership'

    $badCatalog = Join-Path $fixture 'unsigned.cat'
    [IO.File]::WriteAllText($badCatalog, 'not a signed catalog')
    Expect-Refusal { Assert-DriverCatalog $SignTool $badCatalog $Inf }
    $passed++
    Write-Output 'PASS unsigned catalog refuses'

    $badInf = Join-Path $fixture ([IO.Path]::GetFileName($Inf))
    [IO.File]::Copy($Inf, $badInf)
    [IO.File]::AppendAllText($badInf, "`r`n; changed fixture bytes`r`n")
    Expect-Refusal { Assert-DriverCatalog $SignTool $Catalog $badInf }
    $passed++
    Write-Output 'PASS changed INF refuses under the unchanged trusted catalog'

    Expect-Refusal { Assert-DriverCatalog $SignTool $Catalog (Join-Path $fixture 'absent.inf') }
    $passed++
    Write-Output 'PASS absent catalog member refuses'

    if ((Get-FileHash -LiteralPath $Catalog -Algorithm SHA256).Hash -cne $beforeCatalog -or
        (Get-FileHash -LiteralPath $Inf -Algorithm SHA256).Hash -cne $beforeInf) {
        throw 'The original cryptographic fixture bytes changed.'
    }
}
finally {
    $resolved = [IO.Path]::GetFullPath($fixture)
    if ([IO.Path]::GetDirectoryName($resolved) -ine $repo -or
        [IO.Path]::GetFileName($resolved) -cnotmatch '^\.driver-catalog-fixture-[0-9a-f]{32}$') {
        throw 'Fixture cleanup target refused.'
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
Write-Output "Driver catalog checks passed: $passed; no driver installation or hardware execution."
# Expected native refusals above must not become the workflow shell's exit code.
$global:LASTEXITCODE = 0
