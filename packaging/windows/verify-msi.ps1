<#
.SYNOPSIS
    Check that the MSI installs the thing it claims to, and removes it again.

.DESCRIPTION
    The macOS package can be verified by extracting its payload, because a .pkg is
    a copy operation with metadata. An MSI is not: its components, its key paths,
    its shortcut and its upgrade behaviour are *authored*, and every one of them can
    be authored in a way that builds cleanly and then does the wrong thing on a
    machine. So this actually installs it, interrogates what landed, and uninstalls.

    What each check corresponds to:

      * a ProductVersion that disagrees with Cargo.toml makes every future upgrade
        decision on a wrong number;
      * a changed UpgradeCode means the next release installs *beside* this one
        instead of replacing it, and both stay in Programs and Features;
      * a shortcut whose target or key path is wrong installs fine and leaves either
        nothing in the Start Menu or something that is left behind on uninstall;
      * an install that leaves files behind is a machine nobody can cleanly upgrade.

    Installing needs administrator rights. Without them the static checks still run
    and the install is skipped with a warning — except under YKDM_VERIFY_RELEASE=1,
    where it is a failure, because a release must not be the first time anybody
    finds out whether the installer works.

.PARAMETER Msi
    The package to check. Defaults to the newest .msi in target\windows.
#>
param(
    [string]$Msi = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The UpgradeCode is deliberately duplicated here. It is the identity by which every
# future version recognises this product, so it must never change — and a check that
# read it out of Package.wxs would agree with any edit, including the one that breaks
# upgrades forever. Two places that must agree is the point.
$ExpectedUpgradeCode = '{132275B3-F866-4BE4-BC2F-87090EAD2FB7}'
$ExpectedProductName = 'YubiKey Distribution Manager'
$ShortcutName = 'YubiKey Distribution Manager.lnk'
$Binary = 'yk-dist-manager'

$failed = $false
function Fail {
    param([string]$Message)
    Write-Host "FAIL: $Message"
    $script:failed = $true
}
function Pass {
    param([string]$Message)
    Write-Host "  ok    $Message"
}
function Warn {
    param([string]$Message)
    Write-Host "  warn  $Message"
}

$releaseVerify = $env:YKDM_VERIFY_RELEASE -eq '1'
$repo = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Push-Location $repo
try {
    $outDir = Join-Path $repo 'target\windows'
    if (-not $Msi) {
        $Msi = Get-ChildItem -Path (Join-Path $outDir '*.msi') -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
    }
    if (-not $Msi -or -not (Test-Path $Msi)) {
        throw "no package to check — run: powershell -File packaging\windows\msi.ps1"
    }
    $Msi = (Resolve-Path $Msi).Path
    Write-Host "checking $Msi"
    Pass 'package exists'

    # --- What the package says about itself ----------------------------------
    #
    # Read straight out of the MSI's Property table. The COM automation interface is
    # awkward (every call goes through InvokeMember) but it is the only way to ask an
    # MSI a question without installing it, and it needs nothing that is not already
    # on a Windows machine.
    #
    # The whole table is read in one query, which is not a micro-optimisation:
    #
    #   * the query is a constant, so no value is formatted into it — the same rule
    #     AGENTS.md §2 states for SQL, and MSI's query language is close enough to
    #     SQL to deserve it;
    #   * the database object holds the file open, and this way it is opened and
    #     released once, before msiexec is asked to install the same file.
    function Get-MsiProperties {
        param([string]$Path)
        $properties = @{}
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $database = $null
        $view = $null
        try {
            $database = $installer.GetType().InvokeMember(
                'OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, 0))
            $view = $database.GetType().InvokeMember(
                'OpenView', 'InvokeMethod', $null, $database, @('SELECT Property, Value FROM Property'))
            # Out-Null, and not because the value is uninteresting. A function in
            # PowerShell returns everything that reached the output stream, not what
            # `return` names — so an unassigned call here makes Get-MsiProperties
            # return an *array* whose last element is the hashtable, and the next
            # `$msiProperties['ProductVersion']` fails trying to convert the key to an
            # array index. `View.Execute` is documented as returning nothing, which is
            # exactly why it was left unassigned, and through InvokeMember it does not
            # return nothing.
            $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) |
                Out-Null
            while ($true) {
                $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
                if ($null -eq $record) { break }
                $name = $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @(1))
                $value = $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @(2))
                $properties[$name] = $value
                [System.Runtime.InteropServices.Marshal]::ReleaseComObject($record) | Out-Null
            }
        }
        finally {
            foreach ($comObject in @($view, $database, $installer)) {
                if ($comObject) {
                    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($comObject) | Out-Null
                }
            }
            [System.GC]::Collect()
            [System.GC]::WaitForPendingFinalizers()
        }
        return $properties
    }

    $msiProperties = Get-MsiProperties -Path $Msi

    # The same leak, caught where it can still say what happened. Without this the
    # symptom is "Cannot convert value 'ProductVersion' to type 'System.Int32'" from
    # the line below — a message about the *key*, twenty lines from the call that
    # actually put an extra value on the output stream.
    if ($msiProperties -isnot [hashtable]) {
        Fail ("the MSI property table came back as $($msiProperties.GetType().Name), not a hashtable — " +
            'something in Get-MsiProperties wrote to the output stream')
    }

    $match = Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version = "(.*)"' |
        Select-Object -First 1
    $cargoVersion = $match.Matches[0].Groups[1].Value

    $productVersion = $msiProperties['ProductVersion']
    if ($productVersion -ne $cargoVersion) {
        Fail "version drift: the MSI says $productVersion, Cargo.toml says $cargoVersion"
    }
    else {
        Pass "version matches Cargo.toml ($productVersion)"
    }

    $productName = $msiProperties['ProductName']
    if ($productName -ne $ExpectedProductName) {
        Fail "the MSI calls itself '$productName', expected '$ExpectedProductName'"
    }
    else {
        Pass "product name: $productName"
    }

    $upgradeCode = $msiProperties['UpgradeCode']
    if ($upgradeCode -ne $ExpectedUpgradeCode) {
        Fail "the UpgradeCode is $upgradeCode, expected $ExpectedUpgradeCode — changing it means the next release installs beside this one instead of upgrading it"
    }
    else {
        Pass 'UpgradeCode is unchanged'
    }

    # No institution's name, here of all places: packaging is where one can come back
    # with nothing in the test suite to notice (roadmap decision, 2026-08-11).
    $manufacturer = $msiProperties['Manufacturer']
    $expectedManufacturer = if ($env:YKDM_MANUFACTURER) { $env:YKDM_MANUFACTURER } else { 'yk-dist-manager maintainers' }
    if ($manufacturer -ne $expectedManufacturer) {
        if ($releaseVerify) {
            Fail "manufacturer drift: the MSI says '$manufacturer', this tree says '$expectedManufacturer'"
        }
        else {
            Warn "manufacturer: the MSI says '$manufacturer', this tree says '$expectedManufacturer'"
        }
    }
    else {
        Pass "manufacturer: $manufacturer"
    }

    # Authenticode. Blocked on a certificate (features/packaging-and-release.md
    # phase 4), so this reports rather than fails — including for a release, because
    # failing here would stop every release until procurement finishes.
    $signature = Get-AuthenticodeSignature -FilePath $Msi
    switch ($signature.Status) {
        'Valid' { Pass "signed: $($signature.SignerCertificate.Subject)" }
        'NotSigned' { Warn 'unsigned — SmartScreen warns on first run' }
        default { Warn "signature status: $($signature.Status)" }
    }

    # --- Install it ---------------------------------------------------------
    $elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
        ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

    if (-not $elevated) {
        if ($releaseVerify) {
            Fail 'not running as administrator, so the install could not be exercised — a release must not ship an installer nobody has installed'
        }
        else {
            Warn 'not running as administrator: skipping the install/uninstall check (run an elevated shell to exercise it)'
        }
    }
    else {
        $installDir = Join-Path $outDir 'msi-install-check'
        $installLog = Join-Path $outDir 'msi-install.log'
        $uninstallLog = Join-Path $outDir 'msi-uninstall.log'
        Remove-Item $installDir -Recurse -Force -ErrorAction SilentlyContinue

        function Invoke-Msiexec {
            param([string]$Arguments, [string]$What)
            # Start-Process, because msiexec returns immediately when it is invoked
            # any other way and the checks would then race the installer.
            $process = Start-Process -FilePath 'msiexec.exe' -ArgumentList $Arguments -Wait -PassThru
            # 3010 is "success, a reboot would be needed". Nothing here needs one, so
            # it is accepted and reported rather than treated as failure.
            if ($process.ExitCode -eq 3010) {
                Warn "$What asked for a reboot (exit 3010), which nothing in this package should need"
            }
            elseif ($process.ExitCode -ne 0) {
                throw "$What failed with exit code $($process.ExitCode) — see the log next to the package"
            }
        }

        Write-Host ''
        Write-Host "==> installing to $installDir"
        Invoke-Msiexec -What 'msiexec /i' -Arguments (
            "/i `"$Msi`" /qn /norestart INSTALLFOLDER=`"$installDir`" /l*v `"$installLog`"")

        try {
            $installedExe = Join-Path $installDir "$Binary.exe"
            if (-not (Test-Path $installedExe)) {
                throw "the install put no $Binary.exe in $installDir"
            }
            Pass 'the executable was installed'

            foreach ($expected in @('LICENSE.txt', 'CHANGELOG.md', 'README.install.txt')) {
                if (-not (Test-Path (Join-Path $installDir $expected))) {
                    Fail "$expected was not installed beside the executable"
                }
            }
            Pass 'the licence, the changelog and the install notes travel with it'

            # The licence installed next to the program must be this tree's licence,
            # not a copy that went stale.
            if (-not (Compare-Object (Get-Content (Join-Path $repo 'LICENSE')) `
                        (Get-Content (Join-Path $installDir 'LICENSE.txt')))) {
                Pass 'the installed licence matches LICENSE'
            }
            else {
                Fail 'the installed LICENSE.txt does not match LICENSE'
            }

            # The Start Menu entry. A perMachine install puts it under the all-users
            # Start Menu; this is the check that catches a shortcut whose component
            # or key path is wrong, which builds perfectly well.
            $shortcut = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\$ShortcutName"
            if (Test-Path $shortcut) {
                Pass 'Start Menu shortcut created'
                $target = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut).TargetPath
                # Normalised on both sides: the installer may write the path in a
                # different form from the one it was given, and a mismatch that is
                # only a trailing separator is not a finding.
                $target = [System.IO.Path]::GetFullPath($target)
                $installedExeFull = [System.IO.Path]::GetFullPath($installedExe)
                if ($target -ne $installedExeFull) {
                    Fail "the shortcut points at '$target', not at the installed executable"
                }
                else {
                    Pass 'the shortcut points at the installed executable'
                }
            }
            else {
                Fail "no Start Menu shortcut at $shortcut"
            }

            # The decisive check, and the same one the other two platforms make: the
            # installed binary is asked about itself.
            Write-Host ''
            Write-Host '  --diagnose, from the installed executable:'
            $report = & $installedExe --diagnose
            if ($LASTEXITCODE -ne 0) {
                throw "the installed binary exited $LASTEXITCODE when asked to diagnose itself"
            }
            $report | ForEach-Object { Write-Host "    $_" }
            Write-Host ''

            $reportText = $report -join "`n"
            if ($reportText -notmatch [regex]::Escape($cargoVersion)) {
                Fail "the installed binary does not report version $cargoVersion"
            }
            else {
                Pass "the installed binary reports $cargoVersion"
            }

            # Which commit it came from — a warning locally, a failure for a release,
            # the same rule the macOS and Linux verifiers apply.
            $commitLine = $report | Where-Object { $_ -match '^commit:' } | Select-Object -First 1
            if (-not $commitLine) {
                Fail 'the installed binary reports no commit'
            }
            else {
                $commit = ($commitLine -replace '^commit:\s*', '').Trim()
                if ($commit -eq 'unknown' -or $commit.EndsWith('-dirty')) {
                    if ($releaseVerify) {
                        Fail "this package reports commit '$commit', so it cannot be traced to a tag"
                    }
                    else {
                        Warn "commit $commit — fine for a local build, never for one that is installed"
                    }
                }
                else {
                    Pass "built from commit $commit"
                }
            }
        }
        finally {
            # Always uninstall, including after a failed check: leaving a test install
            # registered on the machine would make the next run's results meaningless.
            Write-Host '==> uninstalling'
            Invoke-Msiexec -What 'msiexec /x' -Arguments (
                "/x `"$Msi`" /qn /norestart /l*v `"$uninstallLog`"")
        }

        # What is left behind is the other half of an installer working.
        $leftovers = Get-ChildItem -Path $installDir -Recurse -File -ErrorAction SilentlyContinue
        if ($leftovers) {
            Fail "uninstall left $($leftovers.Count) file(s) in $installDir"
        }
        else {
            Pass 'uninstall removed the installed files'
        }
        $shortcut = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\$ShortcutName"
        if (Test-Path $shortcut) {
            Fail 'uninstall left the Start Menu shortcut behind'
        }
        else {
            Pass 'uninstall removed the Start Menu shortcut'
        }
    }

    Write-Host ''
    if ($failed) {
        throw "package NOT verified: $Msi"
    }
    Write-Host "package verified: $Msi"
}
finally {
    Pop-Location
}
