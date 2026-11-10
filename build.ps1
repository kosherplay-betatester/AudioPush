# Builds the single self-contained AudioPush executable.
#
#   .\build.ps1            release build
#   .\build.ps1 -Debug     debug build (keeps the console window for logs)
#   .\build.ps1 -Test      run the unit tests first
#
# Output: target\release\audiopush.exe — copy that one file to either PC.

[CmdletBinding()]
param(
    [switch]$Debug,
    [switch]$Test
)

$ErrorActionPreference = 'Stop'
Set-Location -Path $PSScriptRoot

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Rust is not installed. Get it from https://rustup.rs then re-run this script."
}

Write-Host "Toolchain: $(rustc --version)"

if ($Test) {
    Write-Host "`nRunning tests..." -ForegroundColor Cyan
    cargo test
    if ($LASTEXITCODE -ne 0) { Write-Error "Tests failed." }
}

$profileName = if ($Debug) { 'debug' } else { 'release' }
Write-Host "`nBuilding ($profileName)..." -ForegroundColor Cyan

if ($Debug) {
    cargo build
} else {
    cargo build --release
}
if ($LASTEXITCODE -ne 0) { Write-Error "Build failed." }

$exe = Join-Path $PSScriptRoot "target\$profileName\audiopush.exe"
$size = [math]::Round((Get-Item $exe).Length / 1MB, 1)

Write-Host "`nDone." -ForegroundColor Green
Write-Host "  $exe  ($size MB)"
Write-Host "  Copy it to both PCs. No installer and no runtime needed."
