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
    foreach ($exe in 'notetaker-app.exe', 'notetaker.exe', 'notetaker-asr.exe', 'notetaker-embed.exe') {
        $dependencies = & $dumpbin /nologo /dependents (Join-Path $BinaryDir $exe) 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) { throw "dumpbin failed for $exe" }
        if ($dependencies -match '(?im)^\s*(?:vcruntime[^\s]*|msvcp[^\s]*|ucrtbase|api-ms-win-crt-[^\s]*)\.dll\s*$') {
            throw "$exe imports a dynamic CRT: $($Matches[0].Trim())"
        }
        if ($dependencies -match '(?im)^\s*(?:onnxruntime[^\s]*|DirectML|sherpa-onnx[^\s]*)\.dll\s*$') {
            throw "$exe imports an optional ML runtime in the CPU package: $($Matches[0].Trim())"
        }
        if ($exe -ne 'notetaker-embed.exe') { Copy-Item (Join-Path $BinaryDir $exe) $stage.FullName }
    }
    # Optional GPU runtimes must never leak into the portable CPU payload.
    $dlls = @(Get-ChildItem $BinaryDir -Filter '*.dll' -File)
    if ($dlls | Where-Object Name -Match '^(?:onnxruntime.*|DirectML|sherpa-onnx.*)\.dll$') {
        throw 'Optional ML DLLs found in the CPU binary directory; use a separate addon directory'
    }
    $dlls | Copy-Item -Destination $stage.FullName
    Copy-Item README.md, LICENSE-*, THIRD-PARTY-NOTICES.md $stage.FullName

    # Run from the staged payload, not target/release, so missing runtime DLLs fail CI.
    Push-Location $stage.FullName
    try {
        foreach ($exe in 'notetaker.exe', 'notetaker-asr.exe') {
            & (Join-Path $stage.FullName $exe) --help
            if ($LASTEXITCODE -ne 0) { throw "$exe failed to start ($LASTEXITCODE)" }
        }
    } finally { Pop-Location }

    # Optional static worker stays out of the main ZIP/installer. Settings fetch it
    # explicitly, using the exact app version and the SHA-256 release sidecar.
    $embed = Join-Path $output.FullName "notetaker-embed-$Version-windows-x64.exe"
    Copy-Item (Join-Path $BinaryDir 'notetaker-embed.exe') $embed -Force
    & $embed --help
    if ($LASTEXITCODE -ne 0) { throw 'Optional embedding worker failed to start' }
    $embedChecksum = "$embed.sha256"
    $embedHash = (Get-FileHash $embed -Algorithm SHA256).Hash.ToLowerInvariant()
    "$embedHash  $(Split-Path $embed -Leaf)" | Set-Content -Encoding ascii $embedChecksum
    $zip = Join-Path $output.FullName "notetaker-$Version-windows-x64.zip"
    Compress-Archive -Path (Join-Path $stage.FullName '*') -DestinationPath $zip -Force
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    $checksum = "$zip.sha256"
    "$hash  $(Split-Path $zip -Leaf)" | Set-Content -Encoding ascii $checksum
    if ($env:GITHUB_OUTPUT) {
        "payload=$($stage.FullName)" >> $env:GITHUB_OUTPUT
        "zip=$zip" >> $env:GITHUB_OUTPUT
        "checksum=$checksum" >> $env:GITHUB_OUTPUT
        "embed=$embed" >> $env:GITHUB_OUTPUT
        "embed_checksum=$embedChecksum" >> $env:GITHUB_OUTPUT
    }
    Write-Host "Packaged $zip"
} finally { Pop-Location }
