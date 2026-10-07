[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Load only pure/native helper definitions. Never evaluate the production
# acceptance body, a CLI image, a credential, a service or a USB device.
# Native process checks inspect only this already-running PowerShell process.
$scriptPath = Join-Path $PSScriptRoot 'Test-ArkForgePackage.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'Acceptance script does not parse.' }
foreach ($definition in $ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
    . ([scriptblock]::Create($definition.Extent.Text))
}

$script:passed = 0
function Check([string]$Name, [scriptblock]$Body) {
    & $Body
    $script:passed++
    Write-Output "PASS $Name"
}
function Refuses([scriptblock]$Body) {
    $refused = $false
    try { & $Body | Out-Null }
    catch { $refused = $true }
    if (-not $refused) { throw 'Expected a strict refusal.' }
}
function Started {
    return ('{"schema":"arkforge.daemon-status/v1","running":true,"supervisor_pid":12,"daemon_pid":13,' +
        '"authority":{"namespace":"arkforge.cli","pairing_epoch":1,"support_records_available":false,"hardware_campaign":null,' +
        '"hdc":{"bound":true,"sha256":"' + ('a' * 64) + '"}},"mechanics_ready":true,"active_jobs":0,' +
        '"blockers":["AUTHORITY_SUPPORT_UNPUBLISHED"],"next_commands":["arkforge device list"]}') | ConvertFrom-Json
}
function Snapshot {
    return '{"schema":"arkforge.status/v1","complete":true,"runtime":{"running":true,"pairing_epoch":1,"mechanics_ready":true,"hdc_bound":true,"active_job_count":0,"active_jobs":[]},"devices":{"available":true,"complete":true,"reason":null,"items":[]},"artifacts":{"available":true,"complete":true,"reason":null,"items":[]},"jobs":{"available":true,"complete":true,"reason":null,"items":[]}}' | ConvertFrom-Json
}
function Discovery {
    return ('{"schema":"arkforge.device-list/v1","deep":false,"filtered_to":null,"observations":[{"observation_id":"observation-fixture",' +
        '"mode":"rockusb-loader","malformed_descriptor":false,"topology_sha256":"' + ('b' * 64) + '","descriptor_sha256":"' + ('c' * 64) + '",' +
        '"identification":{"model":null,"profile":"org.openharmony.dayu200","profile_resolution":"inferred","compatible_profiles":["org.openharmony.dayu200"]}}]}') | ConvertFrom-Json
}
function DrainWitness([int]$ProcessId, [bool]$Exited = $true) {
    $witness = [pscustomobject]@{ ProcessId = $ProcessId; BirthFileTime = 1L; IdentityVerified = $true; Exited = $Exited }
    $witness | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value {
        param([int]$Milliseconds)
        if ($Milliseconds -lt 0 -or $Milliseconds -gt 10000) { throw 'Unbounded drain wait.' }
        return $this.Exited
    }
    return $witness
}

Check 'real daemon projection does not invent a campaign or production support' { Assert-DaemonStarted (Started) ('a' * 64) }
Check 'wrong HDC and string booleans refuse' {
    Refuses { Assert-DaemonStarted (Started) ('b' * 64) }
    $s = Started; $s.mechanics_ready = 'true'; Refuses { Assert-DaemonStarted $s ('a' * 64) }
}
Check 'active Jobs and numeric strings refuse' {
    $s = Started; $s.active_jobs = 1; Refuses { Assert-DaemonStarted $s ('a' * 64) }
    $s = Started; $s.authority.pairing_epoch = '1'; Refuses { Assert-DaemonStarted $s ('a' * 64) }
}
Check 'complete same-epoch owner snapshot' { Assert-StatusSnapshot (Snapshot) (Started) }
Check 'partial unavailable and epoch-drift snapshots refuse despite CLI exit zero' {
    $s = Snapshot; $s.complete = $false; Refuses { Assert-StatusSnapshot $s (Started) }
    $s = Snapshot; $s.jobs.available = $false; Refuses { Assert-StatusSnapshot $s (Started) }
    $s = Snapshot; $s.runtime.pairing_epoch = 2; Refuses { Assert-StatusSnapshot $s (Started) }
}
Check 'passive exact DAYU200 discovery requires no invented model proof' { [void](Assert-Dayu200Discovery (Discovery)) }
Check 'empty duplicate foreign malformed and filtered discoveries refuse' {
    $s = Discovery; $s.observations = @(); Refuses { Assert-Dayu200Discovery $s }
    $s = Discovery; $s.observations = @($s.observations[0], $s.observations[0]); Refuses { Assert-Dayu200Discovery $s }
    $s = Discovery; $s.observations[0].identification.profile = 'org.openharmony.dayu600'; Refuses { Assert-Dayu200Discovery $s }
    $s = Discovery; $s.observations[0].malformed_descriptor = $true; Refuses { Assert-Dayu200Discovery $s }
    $s = Discovery; $s.filtered_to = 'observation-fixture'; Refuses { Assert-Dayu200Discovery $s }
}
Check 'stop must be true and the same original epoch' {
    Assert-DaemonStopped ('{"schema":"arkforge.daemon-stop/v1","stopped":true,"pairing_epoch":1}' | ConvertFrom-Json) (Started)
    Refuses { Assert-DaemonStopped ('{"schema":"arkforge.daemon-stop/v1","stopped":false,"pairing_epoch":1}' | ConvertFrom-Json) (Started) }
    Refuses { Assert-DaemonStopped ('{"schema":"arkforge.daemon-stop/v1","stopped":true,"pairing_epoch":2}' | ConvertFrom-Json) (Started) }
}
Check 'native supervisor access-denied context required' {
    Assert-CrossAccountDenial 5 'No CLI authority supervisor is listening at fixture: Access is denied. (os error 5)'
    Refuses { Assert-CrossAccountDenial 5 'failed reading a file (os error 5)' }
    Refuses { Assert-CrossAccountDenial 2 'Unknown daemon command status' }
    Refuses { Assert-CrossAccountDenial 5 'No CLI authority supervisor is listening at fixture: file missing (os error 2)' }
    Refuses { Assert-CrossAccountDenial 0 'No CLI authority supervisor is listening at fixture: (os error 5)' }
}

$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$fixture = Join-Path $repo ('.afw1-fixture-' + [guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $fixture)
try {
    $native = Get-NativeRuntimePath $fixture
    Check 'owner final native path is stable under denied-user fallback spelling' {
        if (-not $native.StartsWith('\\?\', [StringComparison]::Ordinal) -or
            (Get-NativeRuntimePath $native) -cne $native) { throw 'Native endpoint spelling is not stable.' }
    }
    $leaf = Join-Path $fixture 'file.txt'
    [IO.File]::WriteAllText($leaf, 'file fixture')
    Check 'missing and regular-file runtime roots refuse' {
        Refuses { Get-NativeRuntimePath $leaf }
        Refuses { Get-NativeRuntimePath (Join-Path $fixture 'absent') }
    }
    Check 'retained native witness binds own PID birth image and SHA without a launch' {
        $ownImage = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
        $ownSha = (Get-FileHash -LiteralPath $ownImage -Algorithm SHA256).Hash.ToLowerInvariant()
        $own = [ArkForgeAcceptance.NativePath]::RetainProcess($PID, $ownImage, $ownSha)
        try {
            if (-not $own.IdentityVerified -or $own.ProcessId -ne $PID -or $own.BirthFileTime -le 0 -or
                $own.ImageSha256 -cne $ownSha -or $own.WaitForExit(0)) { throw 'Own native process identity or live wait proof failed.' }
        }
        finally { $own.Dispose() }
        Refuses { $own.WaitForExit(0) }
    }
    Check 'wrong image same-byte copy digest and missing PID refuse native ownership' {
        $ownImage = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
        $ownSha = (Get-FileHash -LiteralPath $ownImage -Algorithm SHA256).Hash.ToLowerInvariant()
        $copy = Join-Path $fixture 'copied-image.exe'
        [IO.File]::Copy($ownImage, $copy)
        Refuses { [ArkForgeAcceptance.NativePath]::RetainProcess($PID, $leaf, $ownSha) }
        Refuses { [ArkForgeAcceptance.NativePath]::RetainProcess($PID, $copy, $ownSha) }
        Refuses { [ArkForgeAcceptance.NativePath]::RetainProcess($PID, $ownImage, ('0' * 64)) }
        Refuses { [ArkForgeAcceptance.NativePath]::RetainProcess(0, $ownImage, $ownSha) }
    }
    Check 'both original witnesses must exit within the one drain bound' {
        Wait-AcceptanceRuntimeDrain (Started) @((DrainWitness 12), (DrainWitness 13)) 0
        Refuses { Wait-AcceptanceRuntimeDrain (Started) @((DrainWitness 12), (DrainWitness 13)) 10001 }
    }
    Check 'missing stale birth and unverified process witnesses refuse cleanup' {
        Refuses { Wait-AcceptanceRuntimeDrain (Started) @((DrainWitness 12)) 0 }
        Refuses { Wait-AcceptanceRuntimeDrain (Started) @((DrainWitness 99), (DrainWitness 13)) 0 }
        $witness = DrainWitness 12; $witness.BirthFileTime = 0L
        Refuses { Wait-AcceptanceRuntimeDrain (Started) @($witness, (DrainWitness 13)) 0 }
        $witness = DrainWitness 12; $witness.IdentityVerified = $false
        Refuses { Wait-AcceptanceRuntimeDrain (Started) @($witness, (DrainWitness 13)) 0 }
        Refuses { Wait-AcceptanceRuntimeDrain (Started) @($null, (DrainWitness 13)) 0 }
    }
    Check 'typed stopped acknowledgement cannot replace native drain or permit deletion' {
        $retained = Join-Path $fixture 'retained-on-timeout.txt'
        [IO.File]::WriteAllText($retained, 'owned evidence retained')
        Assert-DaemonStopped ('{"schema":"arkforge.daemon-stop/v1","stopped":true,"pairing_epoch":1}' | ConvertFrom-Json) (Started)
        Refuses {
            Wait-AcceptanceRuntimeDrain (Started) @((DrainWitness 12), (DrainWitness 13 $false)) 0
            Remove-Item -LiteralPath $retained
        }
        if ([IO.File]::ReadAllText($retained) -cne 'owned evidence retained') { throw 'Uncertain drain deleted retained evidence.' }
    }
    Check 'actual finally block preserves uncertain denied stop without an owner stop retry' {
        $entry = @($ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.TryStatementAst] -and
                $node.Body.Extent.Text.Contains('$hdcProbe = Start-Process')
        }, $false))
        if ($entry.Count -ne 1 -or $null -eq $entry[0].Finally) { throw 'The actual acceptance cleanup block is not unique.' }
        $text = $entry[0].Finally.Extent.Text
        $cleanup = [scriptblock]::Create($text.Substring(1, $text.Length - 2))
        $script:stopCalls = 0
        function Invoke-AcceptanceJson { $script:stopCalls++; throw 'Unexpected owner stop replay.' }
        $crossAccountStopUncertain = $true
        $started = Started
        $witnesses = @()
        $runtime = Join-Path $fixture 'uncertain-runtime'
        [void][IO.Directory]::CreateDirectory($runtime)
        $retained = Join-Path $runtime 'original-evidence'
        [IO.File]::WriteAllText($retained, 'retain original bytes')
        Refuses { & $cleanup }
        if ($script:stopCalls -ne 0 -or [IO.File]::ReadAllText($retained) -cne 'retain original bytes') { throw 'Uncertain denied stop retried or changed its retained runtime.' }
    }
    $alias = Join-Path $fixture 'alias'
    [void](New-Item -ItemType Junction -Path $alias -Target $fixture)
    try { Check 'junction runtime roots refuse' { Refuses { Get-NativeRuntimePath $alias } } }
    finally { Remove-Item -LiteralPath $alias -Force }
    Check 'immutable evidence cannot be overwritten' {
        $path = Join-Path $fixture 'result.json'
        Write-AcceptanceEvidence $path '{"fixture":true}'
        $before = [IO.File]::ReadAllBytes($path)
        Refuses { Write-AcceptanceEvidence $path '{"fixture":false}' }
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) -cne [Convert]::ToBase64String($before)) { throw 'Existing evidence changed.' }
    }
    Check 'package path traversal and directory member refuse' {
        Refuses { Get-AcceptanceFileFact $fixture '../foreign' }
        Refuses { Get-AcceptanceFileFact $fixture 'C:/foreign' }
        Refuses { Get-AcceptanceFileFact $fixture '.' }
    }

    $package = Join-Path $fixture 'package'
    [void][IO.Directory]::CreateDirectory((Join-Path $package 'bin'))
    [void][IO.Directory]::CreateDirectory((Join-Path $package 'ArkForge.release-bundle/bin'))
    [void][IO.Directory]::CreateDirectory((Join-Path $package 'ArkForge.release-bundle/Contents/Resources/profiles'))
    $members = @()
    foreach ($entry in @(@('bin/arkforge.exe', 'cli'), @('bin/arkforged.exe', 'daemon'), @('Contents/Resources/profiles/dayu200.yaml', 'profile'))) {
        [IO.File]::WriteAllText((Join-Path $package ('ArkForge.release-bundle/' + $entry[0])), 'ordinary software fixture ' + $entry[1])
        if ($entry[1] -ne 'profile') { [IO.File]::Copy((Join-Path $package ('ArkForge.release-bundle/' + $entry[0])), (Join-Path $package $entry[0])) }
        $facts = Get-AcceptanceFileFact $package ('ArkForge.release-bundle/' + $entry[0])
        $member = [ordered]@{ path = $entry[0]; role = $entry[1]; bytes = $facts.bytes; sha256 = $facts.sha256 }
        if ($entry[1] -eq 'profile') { $member.profileId = 'org.openharmony.dayu200' }
        $members += [pscustomobject]$member
    }
    $manifestPath = 'ArkForge.release-bundle/Contents/Resources/arkforge-bundle.json'
    [IO.File]::WriteAllText((Join-Path $package $manifestPath), ([ordered]@{ schema = 'arkforge.release-bundle/v1'; version = '0.1.0'; members = $members } | ConvertTo-Json -Depth 8))
    $files = @(Get-ChildItem -LiteralPath $package -Recurse -File | ForEach-Object {
        $relative = $_.FullName.Substring($package.Length + 1).Replace('\', '/')
        $fact = Get-AcceptanceFileFact $package $relative
        [pscustomobject]@{ path = $relative; sha256 = $fact.sha256; bytes = $fact.bytes }
    })
    $trusted = [pscustomobject]@{ files = $files }
    $manifestFact = Get-AcceptanceFileFact $package $manifestPath
    $receipt = [pscustomobject]@{ version = '0.1.0'; files = $files; releaseBundle = [pscustomobject]@{ path = 'ArkForge.release-bundle'; manifest = $manifestPath; manifestSha256 = $manifestFact.sha256 } }
    Check 'bundle exact three roles bind accepted executable and signed manifest bytes' { [void](Assert-ReleaseBundle $package $receipt $trusted) }
    Check 'bundle copied executable drift refuses' {
        $path = Join-Path $package 'bin/arkforge.exe'
        $bytes = [IO.File]::ReadAllBytes($path)
        [IO.File]::WriteAllText($path, 'drift')
        try { Refuses { Assert-ReleaseBundle $package $receipt $trusted } }
        finally { [IO.File]::WriteAllBytes($path, $bytes) }
    }
    Check 'bundle unsigned header and manifest digest drift refuse' {
        Refuses { Assert-ReleaseBundle $package $receipt ([pscustomobject]@{ files = @($files | Where-Object { $_.path -ne $manifestPath }) }) }
        $saved = $receipt.releaseBundle.manifestSha256
        $receipt.releaseBundle.manifestSha256 = 'd' * 64
        try { Refuses { Assert-ReleaseBundle $package $receipt $trusted } }
        finally { $receipt.releaseBundle.manifestSha256 = $saved }
    }
    Check 'duplicate bundle role refuses even when the header digest is updated' {
        $path = Join-Path $package $manifestPath
        $original = [IO.File]::ReadAllBytes($path)
        $bad = [ordered]@{ schema = 'arkforge.release-bundle/v1'; version = '0.1.0'; members = @($members[0], $members[0], $members[2]) }
        [IO.File]::WriteAllText($path, ($bad | ConvertTo-Json -Depth 8))
        $fact = Get-AcceptanceFileFact $package $manifestPath
        $header = @($trusted.files | Where-Object { $_.path -ceq $manifestPath })[0]
        $oldSha = $header.sha256; $oldBytes = $header.bytes
        $header.sha256 = $fact.sha256; $header.bytes = $fact.bytes
        $receipt.releaseBundle.manifestSha256 = $fact.sha256
        try { Refuses { Assert-ReleaseBundle $package $receipt $trusted } }
        finally {
            [IO.File]::WriteAllBytes($path, $original)
            $header.sha256 = $oldSha; $header.bytes = $oldBytes
            $receipt.releaseBundle.manifestSha256 = $oldSha
        }
    }
    Check 'undeclared bundle bytes refuse' {
        $extra = Join-Path $package 'ArkForge.release-bundle/extra.txt'
        [IO.File]::WriteAllText($extra, 'extra')
        try { Refuses { Assert-ReleaseBundle $package $receipt $trusted } }
        finally { Remove-Item -LiteralPath $extra }
    }

    $workflowPath = Join-Path $repo '.github/workflows/windows-acceptance.yml'
    $workflow = [IO.File]::ReadAllText($workflowPath)
    $export = [regex]::Match($workflow, '(?ms)^      - name: Bind immutable package, bundle and acceptance exports\r?\n.*?        run: \|\r?\n(?<body>.*?)(?=^      - name:)').Groups['body'].Value
    if (-not $export) { throw 'Missing delivery export step.' }
    $export = ($export -split '\r?\n' | ForEach-Object { $_ -replace '^          ', '' }) -join "`n"
    $exportAst = [Management.Automation.Language.Parser]::ParseInput($export, [ref]$tokens, [ref]$errors)
    if ($errors.Count -ne 0) { throw 'Delivery export does not parse.' }
    foreach ($definition in $exportAst.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false)) { . ([scriptblock]::Create($definition.Extent.Text)) }
    $archivePath = Join-Path $fixture 'archive.zip'
    Compress-Archive -Path (Join-Path $package '*') -DestinationPath $archivePath
    Check 'whole delivery ZIP entries match exact length and SHA' { Assert-DeliveryArchive $archivePath $files }
    Check 'foreign digest missing duplicate and undeclared ZIP inventory refuse' {
        $bad = @($files | ForEach-Object { [pscustomobject]@{ path = $_.path; bytes = $_.bytes; sha256 = $_.sha256 } })
        $bad[0].sha256 = 'e' * 64
        Refuses { Assert-DeliveryArchive $archivePath $bad }
        Refuses { Assert-DeliveryArchive $archivePath @($files[0]) }
        Refuses { Assert-DeliveryArchive $archivePath @($files + $files[0]) }
    }
    Check 'count-preserving duplicate ZIP expectations cannot hide an undeclared member' {
        $bad = @($files | ForEach-Object { [pscustomobject]@{ path = $_.path; bytes = $_.bytes; sha256 = $_.sha256 } })
        $bad[1] = $bad[0]
        if ($bad.Count -ne $files.Count) { throw 'The duplicate-substitution fixture must preserve inventory count.' }
        Refuses { Assert-DeliveryArchive $archivePath $bad }
        $bad[1] = [pscustomobject]@{ path = $bad[0].path.ToUpperInvariant(); bytes = $bad[0].bytes; sha256 = $bad[0].sha256 }
        Refuses { Assert-DeliveryArchive $archivePath $bad }
    }
    Check 'unsafe paths and non-measured ZIP expectations refuse' {
        foreach ($path in @('../foreign', 'bin/./arkforge.exe', 'C:/foreign', 'bin\\arkforge.exe')) {
            Refuses { Assert-DeliveryArchive $archivePath @([pscustomobject]@{ path = $path; bytes = 1L; sha256 = ('a' * 64) }) }
        }
        Refuses { Assert-DeliveryArchive $archivePath @([pscustomobject]@{ path = 'file'; bytes = '1'; sha256 = ('a' * 64) }) }
        Refuses { Assert-DeliveryArchive $archivePath @([pscustomobject]@{ path = 'file'; bytes = -1L; sha256 = ('a' * 64) }) }
        Refuses { Assert-DeliveryArchive $archivePath @([pscustomobject]@{ path = 'file'; bytes = 1L; sha256 = ('A' * 64) }) }
    }
    Check 'physical workflow never skips hardware or deletes unproven active runtime' {
        if ($workflow -match '-SkipDevice|-SkipCrossAccount' -or
            $workflow -notmatch "if: always\(\) && env\.ARKFORGE_ACCEPTANCE_CLEANUP_READY == 'true'" -or
            $workflow -notmatch 'refs/heads/' -or
            $workflow -notmatch 'windows-production' -or
            $workflow -notmatch 'arkforge-dayu200') { throw 'The protected full physical workflow contract drifted.' }
        $source = [IO.File]::ReadAllText($scriptPath)
        if ($source -match "'daemon', 'status'" -or
            $source -notmatch "'daemon', 'stop'" -or
            $source -notmatch '\$nativeRuntime' -or
            $source.IndexOf('Assert-DaemonStopped $stop $started') -gt $source.IndexOf('Wait-AcceptanceRuntimeDrain $started $witnesses') -or
            $source.IndexOf('Wait-AcceptanceRuntimeDrain $started $witnesses') -gt $source.IndexOf('Remove-Item -LiteralPath $owned.FullName -Recurse -Force')) { throw 'The typed ACL/stop cleanup contract drifted.' }
        if ($source.IndexOf('$crossAccountStopUncertain = $true') -gt $source.IndexOf('$denied = Start-Process') -or
            $source.IndexOf('Assert-CrossAccountDenial $denied.ExitCode $deniedErrorText') -gt $source.LastIndexOf('$crossAccountStopUncertain = $false') -or
            $source.IndexOf('if ($crossAccountStopUncertain)') -gt $source.IndexOf('$stop = Invoke-AcceptanceJson')) { throw 'Uncertain denied stop could replay lifecycle or delete evidence.' }
    }
    Check 'every PowerShell workflow body parses without execution' {
        foreach ($relative in @('.github/workflows/windows-acceptance.yml', '.github/workflows/windows.yml')) {
            $yaml = [IO.File]::ReadAllText((Join-Path $repo $relative))
            foreach ($block in [regex]::Matches($yaml, '(?m)^        run: \|\r?\n(?<body>(?:          .*\r?\n|\r?\n)+)')) {
                $body = ($block.Groups['body'].Value -split '\r?\n' | ForEach-Object { $_ -replace '^          ', '' }) -join "`n"
                [void][Management.Automation.Language.Parser]::ParseInput($body, [ref]$tokens, [ref]$errors)
                if ($errors.Count -ne 0) { throw 'A PowerShell workflow body does not parse.' }
            }
        }
    }
}
finally {
    $resolved = [IO.Path]::GetFullPath($fixture)
    if ([IO.Path]::GetDirectoryName($resolved) -ine $repo -or [IO.Path]::GetFileName($resolved) -cnotmatch '^\.afw1-fixture-[0-9a-f]{32}$') { throw 'Fixture cleanup target refused.' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
Write-Output "Acceptance software guard checks passed: $script:passed; hardware/cross-account execution not run."
