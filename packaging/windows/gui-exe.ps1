<#
.SYNOPSIS
    Run the application's own executable from PowerShell and actually get its
    output and its exit code back.

.DESCRIPTION
    Dot-source this file and call Invoke-GuiExe. It exists for one reason, and the
    reason is not obvious from any call site:

    Since 0.18.3 the release binary is linked into the *Windows* subsystem, so no
    console flashes before the egui window appears (`src/main.rs`). That is
    invisible to everything but PowerShell, which treats a GUI-subsystem image
    differently from a console one — it starts the process, does **not** wait for
    it, and does not set $LASTEXITCODE. So the obvious interrogation,

        $reported = & $exe --version
        Assert-NativeSuccess "$binary --version"

    captures nothing, closes the pipe it was reading while the child is still
    writing to it (the child panics: "failed printing to stdout: The pipe is being
    closed. (os error 232)"), and then dies under Set-StrictMode on a
    $LASTEXITCODE that was never set. That is exactly how the Windows leg of
    releases v0.18.3 and v0.19.0 died at "Build the installer" — after the tag
    existed, which is the expensive place to find it.

    Start-Process -Wait waits whatever the subsystem is, and -PassThru returns a
    process whose ExitCode is really the child's. Output goes through temporary
    files because file redirection is the only kind Start-Process offers; the
    files are this script's, they never hold a secret, and they are removed
    whatever happens.

    Console programs — dotnet, wix, signtool, msiexec — do not need this. Call
    them with `&` and Assert-NativeSuccess as before.

.PARAMETER Exe
    The executable to run. Resolved to a full path first: Start-Process resolves a
    relative one against the *process* working directory, which is not necessarily
    where Push-Location left PowerShell.

.PARAMETER Arguments
    The argv to pass, as separate elements — never one pre-joined string.

.EXAMPLE
    . (Join-Path $PSScriptRoot 'gui-exe.ps1')
    $asked = Invoke-GuiExe -Exe $exe -Arguments '--version'
    if ($asked.ExitCode -ne 0) { throw "--version exited $($asked.ExitCode)" }
    $asked.Output   # string[], one element per line
#>

function Invoke-GuiExe {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Exe,

        [string[]]$Arguments = @()
    )

    $resolved = (Resolve-Path -LiteralPath $Exe).ProviderPath

    $stdout = New-TemporaryFile
    $stderr = New-TemporaryFile
    try {
        $start = @{
            FilePath               = $resolved
            Wait                   = $true
            PassThru               = $true
            NoNewWindow            = $true
            RedirectStandardOutput = $stdout.FullName
            RedirectStandardError  = $stderr.FullName
        }
        # An empty -ArgumentList is a parameter-validation error rather than an
        # empty command line, so it is left off entirely when there is nothing
        # to pass.
        if ($Arguments.Count -gt 0) { $start['ArgumentList'] = $Arguments }

        $process = Start-Process @start

        # UTF-8 named explicitly: the reports these commands print contain em
        # dashes, and Windows PowerShell 5.1 would otherwise read the file back
        # in the machine's ANSI code page and mangle them.
        [pscustomobject]@{
            ExitCode = $process.ExitCode
            Output   = @(Get-Content -LiteralPath $stdout.FullName -Encoding UTF8)
            Error    = @(Get-Content -LiteralPath $stderr.FullName -Encoding UTF8)
        }
    }
    finally {
        Remove-Item -LiteralPath $stdout.FullName, $stderr.FullName `
            -Force -ErrorAction SilentlyContinue
    }
}
