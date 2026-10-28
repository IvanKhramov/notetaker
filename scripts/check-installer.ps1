# Exercise PrepareToInstall with no running app on both a fresh install and an upgrade.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SetupExe
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$SetupExe = (Resolve-Path $SetupExe).Path
$root = (New-Item -ItemType Directory (Join-Path ([IO.Path]::GetTempPath()) "notetaker install $([guid]::NewGuid())")).FullName
$install = Join-Path $root 'app'

try {
    foreach ($phase in 'install', 'upgrade') {
        $log = Join-Path $root "$phase.log"
        $process = Start-Process $SetupExe -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCLOSEAPPLICATIONS', "/DIR=`"$install`"", "/LOG=`"$log`"", '/MERGETASKS=autostart' -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "Installer $phase failed: $($process.ExitCode)" }
        foreach ($exe in 'notetaker-app.exe', 'notetaker.exe', 'notetaker-asr.exe', 'unins000.exe') {
            if (-not (Test-Path (Join-Path $install $exe))) { throw "Missing installed $exe" }
        }
        # GUI-subsystem executables must be waited for explicitly in PowerShell.
        $process = Start-Process (Join-Path $install 'notetaker-app.exe') -ArgumentList '--quit' -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "--quit with no running app failed after ${phase}: $($process.ExitCode)" }
    }
    $shortcut = Join-Path $env:APPDATA 'Microsoft/Windows/Start Menu/Programs/notetaker.lnk'
    if (-not (Test-Path $shortcut)) { throw 'Start menu shortcut missing' }
    $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if ((Get-ItemPropertyValue $run -Name notetaker) -ne "`"$install\notetaker-app.exe`"") { throw 'Autostart command mismatch' }
    $log = Join-Path $root 'uninstall.log'
    $process = Start-Process (Join-Path $install 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$log`"" -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Uninstaller failed: $($process.ExitCode)" }
    if (Get-ItemProperty $run -Name notetaker -ErrorAction SilentlyContinue) { throw 'Autostart value survived uninstall' }
    Write-Host 'Installer fresh install, --quit, upgrade and uninstall checks passed'
} catch {
    Get-ChildItem -LiteralPath $root -Filter '*.log' | ForEach-Object {
        Write-Host "Inno Setup log: $($_.FullName)"
        Get-Content -LiteralPath $_.FullName
    }
    throw
}
