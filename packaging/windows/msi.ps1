<#
.SYNOPSIS
    Build the Windows installer from an already-compiled release binary.

.DESCRIPTION
    The MSI is the counterpart of the macOS .pkg, and it exists for the same reason:
    the zip beside it can only be unzipped by the person sitting at the machine,
    while an MSI installs to Program Files, registers in Programs and Features so a
    fleet can be asked what version it has, upgrades in place, and can be pushed by
    Group Policy or Intune with nobody at the keyboard.

    Like packaging/macos/pkg.sh, this script builds no code. It packages
    target\release\yk-dist-manager.exe and fails if that is missing or is a
    different version from Cargo.toml — a stale binary silently packaged is an
    artefact whose receipt lies about its own contents.

    WiX 6 does the authoring work (packaging\windows\Package.wxs). It is installed
    as a .NET global tool, pinned: an installer is not a place to find out what a
    new major version of a build tool changed. Set YKDM_WIX_VERSION to move it.

.PARAMETER CargoProfile
    Which cargo profile to package: release (default) or debug. Not named -Profile:
    $Profile is one of PowerShell's automatic variables, and shadowing it inside a
    script is a trap for whoever edits this next.

.PARAMETER Arch
    The WiX architecture: x64 (default) or arm64. Decides Program Files and the
    filename, and must match what cargo actually built.

.PARAMETER LinkOnly
    Link the authoring and throw the result away, against a placeholder in place of
    the compiled binary. It answers one question — does WiX accept Package.wxs? —
    and no other: the MSI it writes installs a text file and is deleted.

    It exists because the answer used to cost a version number. Half the errors in
    an installer are found by the *linker*, after every source is parsed, so the
    authoring is not proven by anything short of a build; and this build needed a
    release binary, which meant a tag, which meant the release. v0.16.0 died on a
    comment XML would not accept and v0.16.1 on a shortcut naming an icon that was
    not declared — two mistakes a linker finds in nine seconds, each found instead
    by a release. Under -LinkOnly, CI finds them on the commit that makes them.

    It is not a substitute for building the real MSI, and does not check that what
    is installed works: verify-msi.ps1 does that, from a tag.

.PARAMETER SignCertThumbprint
    SHA-1 thumbprint of an Authenticode certificate **already in this machine's
    certificate store**. When given, the executable is signed before it is packaged
    and the MSI is signed afterwards — both, because signing only the MSI leaves
    SmartScreen warning about the program it installed.

    A thumbprint rather than a .pfx and a password, deliberately: it is the exact
    analogue of the keychain identity bundle.sh takes, no private key or password
    goes on a command line or into a variable this script can log, and nothing
    resembling a credential can end up in the repository (AGENTS.md §2). Getting a
    certificate into a runner's store is the workflow's problem on the day one
    exists — and Azure Trusted Signing, which needs no local key at all, is worth
    comparing then.

.EXAMPLE
    powershell -File packaging\windows\msi.ps1
#>
param(
    [ValidateSet('release', 'debug')]
    [string]$CargoProfile = 'release',

    [ValidateSet('x64', 'arm64')]
    [string]$Arch = 'x64',

    [switch]$LinkOnly,

    [string]$SignCertThumbprint = '',

    [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# PowerShell does not stop on a non-zero exit from a native program, so every call
# to one is followed by this. Without it a failed `wix build` produces a cheerful
# script and no MSI.
function Assert-NativeSuccess {
    param([string]$What)
    if ($LASTEXITCODE -ne 0) {
        throw "$What failed with exit code $LASTEXITCODE"
    }
}

$repo = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Push-Location $repo
try {
    # Signing a placeholder would produce a signature over a file nobody installs,
    # and the only reason to ask for both is a mistake about what -LinkOnly does.
    if ($LinkOnly -and $SignCertThumbprint) {
        throw "-LinkOnly builds a throwaway MSI around a placeholder; there is nothing here worth signing"
    }

    $binary = 'yk-dist-manager'
    $exe = Join-Path $repo "target\$CargoProfile\$binary.exe"
    if (-not $LinkOnly -and -not (Test-Path $exe)) {
        throw "no binary at $exe — build it first: cargo build --$CargoProfile --features native-device,encrypted-db"
    }

    # Single source of truth for the version: the manifest.
    $cargoToml = Join-Path $repo 'Cargo.toml'
    $match = Select-String -Path $cargoToml -Pattern '^version = "(.*)"' | Select-Object -First 1
    if (-not $match) { throw "could not read the version from Cargo.toml" }
    $version = $match.Matches[0].Groups[1].Value

    # The binary has to agree, or what is being packaged is a leftover from an
    # earlier build. Asking it is cheap and it is the same interrogation the
    # verifiers make. There is nothing to ask under -LinkOnly, where the version
    # is only what the authoring is linked against.
    if (-not $LinkOnly) {
        $reported = & $exe --version
        Assert-NativeSuccess "$binary --version"
        if ($reported -notmatch [regex]::Escape($version)) {
            throw "version drift: Cargo.toml says $version, but the binary reports '$reported' — rebuild it"
        }
    }

    # An MSI ProductVersion is major.minor.build with a 255.255.65535 ceiling, and
    # everything after the third field is *ignored* when Windows compares versions.
    # A pre-release suffix would therefore make two different builds look identical
    # to an upgrade, so it is refused rather than silently truncated.
    if ($version -notmatch '^\d+\.\d+\.\d+$') {
        throw "MSI needs a three-part numeric version; Cargo.toml says '$version'. Windows ignores anything beyond the third field, so an upgrade could not tell two such builds apart."
    }

    $manufacturer = if ($env:YKDM_MANUFACTURER) { $env:YKDM_MANUFACTURER } else { 'yk-dist-manager maintainers' }

    $outDir = Join-Path $repo 'target\windows'
    $stage = Join-Path $outDir 'msi-stage'
    New-Item -ItemType Directory -Force -Path $outDir, $stage | Out-Null

    if ($LinkOnly) {
        # A File element needs a file to exist, and needs nothing else of it: what
        # is being asked is whether WiX accepts the authoring, not what the program
        # does. So the placeholder is a text file, and the MSI around it is deleted
        # at the end rather than kept where somebody could pick it up.
        $exe = Join-Path $stage "$binary.exe"
        Set-Content -Path $exe -Value "placeholder for -LinkOnly; not a program" -Encoding ASCII
        Write-Host "==> linking the authoring only ($binary $version, $Arch) — no MSI is kept"
    }
    else {
        Write-Host "==> packaging $binary $version ($CargoProfile, $Arch)"
    }

    # --- The licence, as RTF -------------------------------------------------
    #
    # The WixUI licence pane reads RTF and nothing else, so LICENSE is converted
    # rather than duplicated: a second copy of a licence is a copy that goes stale.
    #
    # Three characters mean something to RTF — \ { } — and everything above ASCII
    # has to be escaped as \uN. That is the same class of bug write-plist.sh exists
    # for on the macOS side: free text through a substitution that was never
    # escaped for it. LICENSE is ASCII today; this does not depend on that.
    $licenseRtf = Join-Path $stage 'License.rtf'
    $rtf = [System.Text.StringBuilder]::new()
    [void]$rtf.Append('{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fnil\fcharset0 Segoe UI;}}\fs18')
    foreach ($line in (Get-Content (Join-Path $repo 'LICENSE'))) {
        $escaped = ''
        foreach ($ch in $line.ToCharArray()) {
            switch ($ch) {
                '\' { $escaped += '\\' }
                '{' { $escaped += '\{' }
                '}' { $escaped += '\}' }
                default {
                    if ([int]$ch -gt 127) {
                        # \uN takes a *signed* 16-bit integer, and the '?' after it
                        # is what a reader that cannot do Unicode shows instead.
                        $code = [int]$ch
                        if ($code -gt 32767) { $code -= 65536 }
                        $escaped += '\u' + $code + '?'
                    }
                    else {
                        $escaped += $ch
                    }
                }
            }
        }
        [void]$rtf.Append("$escaped\par`r`n")
    }
    [void]$rtf.Append('}')
    # ASCII, because the escaping above has already turned everything else into
    # \uN sequences; writing UTF-8 here would put a BOM in front of {\rtf1 and no
    # reader would recognise the file.
    [System.IO.File]::WriteAllText($licenseRtf, $rtf.ToString(), [System.Text.Encoding]::ASCII)

    # --- The notes that travel with the artefact ----------------------------
    #
    # The Linux package's README.install lesson: the platform requirements have to
    # be next to the installed program, not only in docs/operations.md.
    $readme = Join-Path $stage 'README.install.txt'
    @"
yk-dist-manager $version — what this needs to work on Windows

1. Smartcards (the PIV applet):
     the *Smart Card* service must be running. Check it with
       sc query SCardSvr
     It is present on every supported Windows and starts on demand; a disabled
     one makes the PIV applet unreachable and looks exactly like broken hardware.

2. USB HID (FIDO2 and the OTP slots):
     nothing to install and no driver to sign. Windows may require the
     application to run elevated to talk to a FIDO2 device; if FIDO2 steps fail
     while PIV works, that is the thing to try.

3. Camera scanning (optional):
     reading a serial from a barcode with a webcam uses the camera Windows
     already knows about. Check Settings > Privacy & security > Camera if it
     finds none. A USB barcode scanner needs nothing: it types into the field.

4. SmartScreen:
     until this project has an Authenticode certificate the installer and the
     executable are unsigned, so SmartScreen warns the first time each runs:
     *More info* > *Run anyway*.

5. The register itself:
     one SQLite file that you choose or create. It can sit on an SMB share,
     which this tool can connect for you, or in a synchronising folder. The
     installer writes nothing outside its own directory.

Check what this build can reach on this machine:
     "%ProgramFiles%\YubiKey Distribution Manager\$binary.exe" --diagnose
"@ | Set-Content -Path $readme -Encoding UTF8

    # --- Authenticode, if there is a certificate ----------------------------
    #
    # The executable is signed before it goes into the MSI: an installer that is
    # itself signed but delivers an unsigned program moves the SmartScreen warning
    # rather than removing it. The signed copy is staged so target\release stays
    # exactly what cargo produced.
    $exeToPackage = $exe
    if ($SignCertThumbprint) {
        $signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
        if ($signtool) {
            $signtoolPath = $signtool.Source
        }
        else {
            # Not on PATH outside a Visual Studio developer prompt. Newest SDK wins.
            $signtoolPath = Get-ChildItem -Path "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\$Arch\signtool.exe" `
                -ErrorAction SilentlyContinue | Sort-Object FullName -Descending |
                Select-Object -First 1 -ExpandProperty FullName
            if (-not $signtoolPath) {
                throw "signtool.exe is not on PATH and no Windows SDK copy was found, but a signing certificate was given"
            }
        }

        $exeToPackage = Join-Path $stage "$binary.exe"
        Copy-Item $exe $exeToPackage -Force

        Write-Host "==> signing the executable"
        & $signtoolPath sign /sha1 $SignCertThumbprint /fd SHA256 `
            /tr $TimestampUrl /td SHA256 $exeToPackage
        Assert-NativeSuccess 'signtool sign (executable)'
    }
    elseif (-not $LinkOnly) {
        Write-Host "    note: no signing certificate given — the executable and the MSI will be unsigned, and SmartScreen will warn on first run"
    }

    # --- WiX ----------------------------------------------------------------
    #
    # A .NET global tool lands in ~\.dotnet\tools, which is on PATH for a *new*
    # shell and not for this one, so the directory goes on PATH before anything
    # looks for the tool. Skipping that step is how a script installs wix
    # successfully and then reports that wix cannot be found.
    $wixVersion = if ($env:YKDM_WIX_VERSION) { $env:YKDM_WIX_VERSION } else { '6.0.1' }
    $env:PATH = "$env:PATH;$env:USERPROFILE\.dotnet\tools"
    if (-not (Get-Command wix.exe -ErrorAction SilentlyContinue)) {
        Write-Host "==> installing WiX $wixVersion as a global tool"
        & dotnet tool install --global wix --version $wixVersion
        Assert-NativeSuccess 'dotnet tool install wix'
    }

    # The extension has to match the toolset that will load it, so the version comes
    # from the wix on PATH rather than from the pin above — which are the same thing
    # on a clean machine and are not on a developer's, and the mismatch is reported
    # by the extension loader in terms that do not mention versions at all.
    # `wix --version` prints "6.0.1+<commit>"; only the part before the + is a
    # package version.
    $wixReported = (& wix.exe --version | Select-Object -First 1).Trim()
    Assert-NativeSuccess 'wix --version'
    $wixActual = $wixReported.Split('+')[0]
    Write-Host "    WiX $wixReported"
    if ($wixActual -ne $wixVersion) {
        Write-Host "    note: this machine has WiX $wixActual, not the pinned $wixVersion"
    }

    # `extension add` is idempotent, so it runs unconditionally rather than after a
    # check that can disagree with the cache.
    Write-Host "==> WiX UI extension"
    & wix.exe extension add -g "WixToolset.UI.wixext/$wixActual"
    Assert-NativeSuccess 'wix extension add'

    $archTag = switch ($Arch) { 'x64' { 'x86_64' } 'arm64' { 'aarch64' } }
    # Under -LinkOnly the name says what the file is, because a file called
    # yk-dist-manager-0.16.2-x86_64.msi that installs a placeholder is the kind of
    # thing that reaches somebody's machine.
    $msi = if ($LinkOnly) {
        Join-Path $outDir 'link-check.msi'
    }
    else {
        Join-Path $outDir "$binary-$version-$archTag.msi"
    }
    Remove-Item $msi -Force -ErrorAction SilentlyContinue

    Write-Host "==> building $msi"
    & wix.exe build `
        -arch $Arch `
        -ext WixToolset.UI.wixext `
        -d "Version=$version" `
        -d "Manufacturer=$manufacturer" `
        -d "ExeFile=$exeToPackage" `
        -d "LicenseFile=$(Join-Path $repo 'LICENSE')" `
        -d "LicenseRtf=$licenseRtf" `
        -d "ChangelogFile=$(Join-Path $repo 'CHANGELOG.md')" `
        -d "ReadmeFile=$readme" `
        -d "IconFile=$(Join-Path $repo 'packaging\windows\icon.ico')" `
        -o $msi `
        (Join-Path $repo 'packaging\windows\Package.wxs')
    Assert-NativeSuccess 'wix build'

    if ($SignCertThumbprint) {
        Write-Host "==> signing the MSI"
        & $signtoolPath sign /sha1 $SignCertThumbprint /fd SHA256 `
            /tr $TimestampUrl /td SHA256 $msi
        Assert-NativeSuccess 'signtool sign (MSI)'
    }

    Write-Host ''
    if ($LinkOnly) {
        Remove-Item $msi -Force -ErrorAction SilentlyContinue
        Write-Host 'the authoring links: WiX accepts Package.wxs and every reference in it resolves'
        Write-Host 'that is all it says — the MSI it built installed a placeholder, and is deleted'
    }
    else {
        Write-Host "built: $msi"
        Write-Host 'check it with:  powershell -File packaging\windows\verify-msi.ps1'
    }
}
finally {
    Pop-Location
}
