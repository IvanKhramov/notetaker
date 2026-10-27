[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$PayloadDir
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Version -cnotmatch '^\d+\.\d+\.\d+$') { throw 'Expected a stable numeric version' }
$repo = Split-Path $PSScriptRoot -Parent
$iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe'
if (-not (Test-Path $iscc)) {
    choco install innosetup -y --no-progress
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup installation failed' }
}
$payload = (Resolve-Path $PayloadDir).Path
& $iscc "/DAppVersion=$Version" "/DPayloadDir=$payload" (Join-Path $repo 'packaging/windows/notetaker.iss')
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed' }
$setup = Join-Path $repo "target/installer/notetaker-v$Version-windows-x64-setup.exe"
$checksum = "$setup.sha256"
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $(Split-Path $setup -Leaf)" | Set-Content -Encoding ascii $checksum
if ($env:GITHUB_OUTPUT) {
    "setup=$setup" >> $env:GITHUB_OUTPUT
    "setup_checksum=$checksum" >> $env:GITHUB_OUTPUT
}
Write-Host "Packaged $setup"
