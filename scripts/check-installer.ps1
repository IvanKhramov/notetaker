# Exercise the installed GUI and an upgrade from the latest published release.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$SetupExe)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$SetupExe = (Resolve-Path $SetupExe).Path
$root = (New-Item -ItemType Directory (Join-Path ([IO.Path]::GetTempPath()) "notetaker install $([guid]::NewGuid())")).FullName
$install = Join-Path $root 'app'
$deadline = [DateTime]::UtcNow.AddSeconds(170)
$repo = 'IvanKhramov/notetaker'

function Wait-SmokeProcess($Process, [int]$Seconds) {
    $remaining = [int]($deadline - [DateTime]::UtcNow).TotalMilliseconds
    if ($remaining -le 0 -or -not $Process.WaitForExit([Math]::Min($Seconds * 1000, [Math]::Max(1, $remaining)))) {
        if (-not $Process.HasExited) { $Process.Kill($true) }
        throw 'Installer smoke test timed out'
    }
    $Process.Refresh()
    if ($Process.ExitCode -ne 0) { throw "Process failed: $($Process.ExitCode)" }
}
function Install-Smoke([string]$Installer, [string]$Phase) {
    $log = Join-Path $root "$Phase.log"
    $process = Start-Process $Installer -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCLOSEAPPLICATIONS', "/DIR=`"$install`"", "/LOG=`"$log`"", '/MERGETASKS=autostart' -PassThru
    Wait-SmokeProcess $process 30
    foreach ($exe in 'notetaker-app.exe', 'notetaker.exe', 'notetaker-asr.exe', 'unins000.exe') {
        if (-not (Test-Path (Join-Path $install $exe))) { throw "Missing installed $exe" }
    }
}
function Get-InstalledVersion {
    $version = & (Join-Path $install 'notetaker.exe') --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI --version failed' }
    return "$version".Trim()
}
function Test-InstalledGui {
    $gui = Start-Process (Join-Path $install 'notetaker-app.exe') -PassThru
    try {
        # No audio devices required: just start the normal GUI without recording.
        for ($i = 0; $i -lt 10; $i++) {
            Start-Sleep -Seconds 1
            $gui.Refresh()
            if ($gui.HasExited) { throw "GUI exited during startup: $($gui.ExitCode)" }
        }
        $quit = Start-Process (Join-Path $install 'notetaker-app.exe') -ArgumentList '--quit' -PassThru
        Wait-SmokeProcess $quit 10
        Wait-SmokeProcess $gui 10
    } finally {
        if (-not $gui.HasExited) { $gui.Kill($true); $gui.WaitForExit() }
    }
}
function Uninstall-Smoke([string]$Phase) {
    $log = Join-Path $root "$Phase.log"
    $process = Start-Process (Join-Path $install 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$log`"" -PassThru
    Wait-SmokeProcess $process 20
}
try {
    # Download failures alone are optional. A downloaded but invalid checksum fails.
    $previous = Join-Path $root 'previous'
    New-Item -ItemType Directory $previous | Out-Null
    $downloaded = $false
    try {
        $download = Start-Process gh -ArgumentList 'release', 'download', '--repo', $repo, '--pattern', '*-windows-x64-setup.exe*', '--dir', "`"$previous`"" -PassThru
        Wait-SmokeProcess $download 30
        $downloaded = $true
    } catch {
        Write-Warning "Previous release unavailable; skipping upgrade check: $_"
    }
    $oldSetup = $null
    if ($downloaded) {
        $setups = @(Get-ChildItem $previous -Filter '*-setup.exe')
        if ($setups.Count -ne 1) { throw 'Expected one previous release installer' }
        $oldSetup = $setups[0].FullName
        $checksum = (Get-Content -Raw "$oldSetup.sha256").Trim().Split()[0]
        if ($checksum -notmatch '^[0-9a-fA-F]{64}$' -or (Get-FileHash $oldSetup -Algorithm SHA256).Hash -ne $checksum) {
            throw 'Previous release installer checksum mismatch'
        }
    }
    Install-Smoke $SetupExe 'fresh'
    $freshVersion = Get-InstalledVersion
    Test-InstalledGui
    if ($oldSetup) {
        # Compare release filenames before installing: the latest tag may be this build.
        if ((Split-Path $oldSetup -Leaf) -eq (Split-Path $SetupExe -Leaf)) {
            Write-Warning 'Latest published installer has the same version; skipping upgrade check'
        } else {
            Uninstall-Smoke 'remove-fresh'
            Install-Smoke $oldSetup 'previous'
            $oldVersion = Get-InstalledVersion
            Install-Smoke $SetupExe 'upgrade'
            $newVersion = Get-InstalledVersion
            if ($newVersion -eq $oldVersion -or $newVersion -ne $freshVersion) {
                throw "Installed version did not upgrade: $oldVersion -> $newVersion (expected $freshVersion)"
            }
            Write-Host "Upgrade verified: $oldVersion -> $newVersion"
            Test-InstalledGui
        }
    }
    $shortcut = Join-Path $env:APPDATA 'Microsoft/Windows/Start Menu/Programs/notetaker.lnk'
    if (-not (Test-Path $shortcut)) { throw 'Start menu shortcut missing' }
    $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if ((Get-ItemPropertyValue $run -Name notetaker) -ne "`"$install\notetaker-app.exe`"") { throw 'Autostart command mismatch' }
    Uninstall-Smoke 'uninstall'
    if (Get-ItemProperty $run -Name notetaker -ErrorAction SilentlyContinue) { throw 'Autostart value survived uninstall' }
    Write-Host 'Installer and GUI smoke checks passed'
} catch {
    Get-ChildItem -LiteralPath $root -Filter '*.log' | ForEach-Object {
        Write-Host "Inno Setup log: $($_.FullName)"
        Get-Content -LiteralPath $_.FullName -Tail 80
    }
    throw
}
