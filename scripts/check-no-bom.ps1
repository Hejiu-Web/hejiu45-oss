#!/usr/bin/env pwsh
# Fail if any tracked text file starts with a UTF-8 BOM (Windows twin of
# scripts/check-no-bom.sh). Windows PowerShell 5.1 adds a BOM for
# `Set-Content -Encoding utf8`, which breaks tools that parse Cargo.toml.
$ErrorActionPreference = "Stop"
$files = git ls-files
$bad = @()
foreach ($f in $files) {
    if ($f -match '\.(png|jpg|jpeg|ico|gz|zip|exe)$') { continue }
    if (-not (Test-Path $f)) { continue }
    $b = [System.IO.File]::ReadAllBytes((Resolve-Path $f))
    if ($b.Length -ge 3 -and $b[0] -eq 0xEF -and $b[1] -eq 0xBB -and $b[2] -eq 0xBF) { $bad += $f }
}
if ($bad.Count -gt 0) {
    Write-Host "BOM found in:" -ForegroundColor Red
    $bad | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "no BOMs found ($($files.Count) files checked)" -ForegroundColor Green
