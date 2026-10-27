# The portable release must never claim DirectML is available.
[CmdletBinding()]
param([string]$Binary = 'target/release/notetaker-asr.exe')
$ErrorActionPreference = 'Stop'
& $Binary check-provider cpu
if ($LASTEXITCODE -ne 0) { throw 'CPU ASR provider is unavailable' }
# Native stderr can become ErrorRecord under Windows PowerShell.
$output = & $Binary check-provider directml 2>&1 | Out-String
if ($LASTEXITCODE -eq 0 -or $output -notmatch 'недоступно в этой сборке') {
    throw "Portable ASR must explicitly reject DirectML: $output"
}
exit 0
