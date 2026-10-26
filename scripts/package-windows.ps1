# Shared payload for CI artifacts and tagged releases. Run after cargo build --release.
[CmdletBinding()]
param(
    [string]$Version,
    [string]$BinaryDir = 'target/release',
    [string]$OutputDir = 'target/package'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
try {
    $metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $workspaceVersion = ($metadata.packages | Where-Object name -eq 'notetaker-app').version
    if (-not $PSBoundParameters.ContainsKey('Version')) { $Version = $workspaceVersion }
    if ($Version -cne $workspaceVersion) {
        throw "Tag version $Version does not match workspace version $workspaceVersion"
    }
    $output = New-Item -ItemType Directory -Force $OutputDir
    # Fresh staging directory prevents stale executables/DLLs entering a later package.
    $stage = New-Item -ItemType Directory (Join-Path $output.FullName ([guid]::NewGuid().ToString()))
    # CI runners include the redistributable; a successful launch there cannot
    # prove that the package starts on a clean Windows installation.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (-not (Test-Path $vswhere)) { throw 'vswhere is required to verify static CRT linkage' }
    $dumpbin = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC/Tools/MSVC/**/bin/Hostx64/x64/dumpbin.exe' | Select-Object -First 1
    if ($LASTEXITCODE -ne 0 -or -not $dumpbin) { throw 'dumpbin is required to verify static CRT linkage' }
    foreach ($exe in 'notetaker-app.exe', 'notetaker.exe', 'notetaker-asr.exe') {
        $dependencies = & $dumpbin /nologo /dependents (Join-Path $BinaryDir $exe) 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) { throw "dumpbin failed for $exe" }
        if ($dependencies -match '(?im)^\s*(?:vcruntime[^\s]*|msvcp[^\s]*|ucrtbase|api-ms-win-crt-[^\s]*)\.dll\s*$') {
            throw "$exe imports a dynamic CRT: $($Matches[0].Trim())"
        }
        Copy-Item (Join-Path $BinaryDir $exe) $stage.FullName
    }
    # Default CPU builds use static sherpa/onnxruntime. If a shared build places
    # runtime DLLs beside the executables, preserve them in both distributions.
    Get-ChildItem $BinaryDir -Filter '*.dll' -File | Copy-Item -Destination $stage.FullName
    Copy-Item README.md, LICENSE-*, THIRD-PARTY-NOTICES.md $stage.FullName

    # Run from the staged payload, not target/release, so missing runtime DLLs fail CI.
    Push-Location $stage.FullName
    try {
        foreach ($exe in 'notetaker.exe', 'notetaker-asr.exe') {
            & (Join-Path $stage.FullName $exe) --help
            if ($LASTEXITCODE -ne 0) { throw "$exe failed to start ($LASTEXITCODE)" }
        }
    } finally { Pop-Location }

    $zip = Join-Path $output.FullName "notetaker-$Version-windows-x64.zip"
    Compress-Archive -Path (Join-Path $stage.FullName '*') -DestinationPath $zip -Force
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    $checksum = "$zip.sha256"
    "$hash  $(Split-Path $zip -Leaf)" | Set-Content -Encoding ascii $checksum
    if ($env:GITHUB_OUTPUT) {
        "payload=$($stage.FullName)" >> $env:GITHUB_OUTPUT
        "zip=$zip" >> $env:GITHUB_OUTPUT
        "checksum=$checksum" >> $env:GITHUB_OUTPUT
    }
    Write-Host "Packaged $zip"
} finally { Pop-Location }
