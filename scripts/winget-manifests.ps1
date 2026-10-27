[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$SetupExe,
    [string]$OutputDir = 'target/winget'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Version = $Version -creplace '^v', ''
if ($Version -cnotmatch '^\d+\.\d+\.\d+$') { throw 'Expected a stable version, e.g. 0.8.0' }
$repo = Split-Path $PSScriptRoot -Parent
$name = "notetaker-v$Version-windows-x64-setup.exe"
$url = "https://github.com/IvanKhramov/notetaker/releases/download/v$Version/$name"
$temporary = $null
try {
    if (-not $SetupExe) {
        $temporary = New-Item -ItemType Directory (Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString()))
        $SetupExe = Join-Path $temporary.FullName $name
        Invoke-WebRequest $url -OutFile $SetupExe
        $checksum = (Invoke-WebRequest "$url.sha256").Content
        if ($checksum -is [byte[]]) { $checksum = [Text.Encoding]::ASCII.GetString($checksum) }
        $parts = $checksum.Trim() -split '\s+'
        if ($parts.Count -ne 2 -or $parts[1] -cne $name -or $parts[0] -notmatch '^[a-fA-F0-9]{64}$') { throw 'Invalid release checksum file' }
        if ((Get-FileHash $SetupExe -Algorithm SHA256).Hash -ine $parts[0]) { throw 'Release checksum mismatch' }
    }
    $hash = (Get-FileHash $SetupExe -Algorithm SHA256).Hash
    $output = New-Item -ItemType Directory -Force (Join-Path $OutputDir $Version)
    foreach ($template in Get-ChildItem (Join-Path $repo 'packaging/winget') -Filter '*.yaml.in') {
        $text = [IO.File]::ReadAllText($template.FullName).Replace('@VERSION@', $Version).Replace('@URL@', $url).Replace('@SHA256@', $hash)
        if ($text -match '@[A-Z]+@') { throw "Unresolved placeholder in $($template.Name)" }
        [IO.File]::WriteAllText((Join-Path $output.FullName ($template.Name -replace '\.in$', '')), $text, [Text.UTF8Encoding]::new($false))
    }
    Write-Host "Manifests: $($output.FullName)"
    if ($env:GITHUB_OUTPUT) {
        "manifests=$($output.FullName)" >> $env:GITHUB_OUTPUT
        "version=$Version" >> $env:GITHUB_OUTPUT
        "url=$url" >> $env:GITHUB_OUTPUT
    }
} finally {
    if ($temporary) { Remove-Item $temporary.FullName -Recurse -Force }
}
