#!/usr/bin/env pwsh
# Real AWS CLI compatibility test.
#
# Unlike e2e-s3.ps1 (which signs requests by hand to prove our verifier against
# AWS's documented algorithm), this script drives the official `aws s3` and
# `aws s3api` commands, so it proves compatibility with a real SDK's own SigV4
# implementation, wire format expectations, and error handling.
#
# Usage:
#   $env:AWS_ACCESS_KEY_ID="HJ45..."; $env:AWS_SECRET_ACCESS_KEY="..."
#   ./scripts/e2e-awscli.ps1 -Endpoint http://127.0.0.1:9000/s3
param(
    [string]$Endpoint = "http://127.0.0.1:9000/s3",
    [string]$Region = "us-east-1",
    [string]$Profile = "hj45e2e"
)

$ErrorActionPreference = "Continue"
$aws = (Get-Command aws -ErrorAction SilentlyContinue)
if (-not $aws) {
    $candidates = @(
        "$env:LOCALAPPDATA\Programs\Amazon\AWSCLIV2\aws.exe",
        "C:\Program Files\Amazon\AWSCLIV2\aws.exe"
    )
    $aws = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $aws) { Write-Host "aws CLI not found" -ForegroundColor Red; exit 2 }
} else { $aws = $aws.Source }

$env:AWS_DEFAULT_REGION = $Region
$env:AWS_PAGER = ""
$env:AWS_EC2_METADATA_DISABLED = "true"

# Credentials come from the environment (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY),
# which every AWS SDK reads first. A private config file is used only for the
# one setting the CLI cannot take from the environment: path-style addressing.
$configRoot = Join-Path $env:TEMP ("hj45-aws-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force -Path $configRoot | Out-Null
$env:AWS_CONFIG_FILE = Join-Path $configRoot "config"
& $aws configure set default.s3.addressing_style path 2>&1 | Out-Null
& $aws configure set region $Region 2>&1 | Out-Null
& $aws configure set signature_version s3v4 2>&1 | Out-Null
if (-not $env:AWS_ACCESS_KEY_ID -or -not $env:AWS_SECRET_ACCESS_KEY) {
    Write-Host "AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY must be set" -ForegroundColor Red
    exit 2
}

$global:S3 = @("--endpoint-url", $Endpoint)
$script:fail = 0
$script:pass = 0

function Check([string]$Name, [scriptblock]$Test) {
    # `$LASTEXITCODE` is global and survives a scriptblock invocation, but a
    # block that captures it locally must be read *inside* the block. Capture
    # here immediately after the call so both styles work.
    $out = & $Test 2>&1
    # A block may signal failure either by throwing (preferred) or by returning
    # exactly `$false`. Exit codes are not used: a scriptblock's $LASTEXITCODE
    # does not propagate reliably to the caller in Windows PowerShell 5.1.
    $signalledFalse = ($out | Where-Object { $_ -is [bool] -and $_ -eq $false }) -ne $null
    if ($signalledFalse) {
        $script:fail++
        Write-Host "  [FAIL] $Name" -ForegroundColor Red
        ($out | Select-Object -Last 4) | ForEach-Object { Write-Host "         $_" -ForegroundColor DarkGray }
        return $null
    }
    $script:pass++
    Write-Host "  [PASS] $Name" -ForegroundColor Green
    return $out
}

$bucket = "cli-" + ([guid]::NewGuid().ToString("N").Substring(0, 10))
$work = Join-Path $configRoot "work"
New-Item -ItemType Directory -Force -Path $work | Out-Null

Write-Host "AWS CLI $(& $aws --version 2>&1) against $Endpoint (bucket $bucket)" -ForegroundColor Cyan

Check "s3 mb (create bucket)" { & $aws s3 mb "s3://$bucket" @S3 | Out-Null }
Check "s3api head-bucket" { & $aws s3api head-bucket --bucket $bucket @S3 | Out-Null }
Check "s3 ls (list buckets)" {
    $r = & $aws s3 ls @S3
    if ($r -notmatch [regex]::Escape($bucket)) { throw "bucket missing from listing" }
}

# --- uploads ---------------------------------------------------------------
$small = Join-Path $work "small.txt"
Set-Content -Path $small -Value "hello from the aws cli" -NoNewline -Encoding utf8
Check "s3 cp (single PUT)" { & $aws s3 cp $small "s3://$bucket/small.txt" @S3 | Out-Null }


# `s3 cp DIR s3://b/prefix/ --recursive` copies the *contents* of DIR under
# prefix, so point it at a directory that holds exactly the files to upload.
$uploadDir = Join-Path $work "up"
New-Item -ItemType Directory -Force -Path $uploadDir | Out-Null
Set-Content -Path (Join-Path $uploadDir "root.txt") -Value "root" -NoNewline -Encoding utf8
New-Item -ItemType Directory -Force -Path (Join-Path $uploadDir "nested") | Out-Null
Set-Content -Path (Join-Path $uploadDir "nested\deep.txt") -Value "deep" -NoNewline -Encoding utf8
Check "s3 cp --recursive (nested keys)" {
    & $aws s3 cp $uploadDir "s3://$bucket/upload/" --recursive @S3 | Out-Null
    $raw = & $aws s3api list-objects-v2 --bucket $bucket --prefix "upload/" @S3 | Out-String
    $r = $raw | ConvertFrom-Json
    $count = if ($null -eq $r.KeyCount) { @($r.Contents).Count } else { [int]$r.KeyCount }
    if ($count -ne 2) { throw "expected 2 keys under upload/, got $count (raw: $raw)" }
}

# A file large enough to force multipart (aws s3 cp switches above 8 MB).
$big = Join-Path $work "big.bin"
$fs = [System.IO.File]::Create($big)
$chunk = New-Object byte[] (1024 * 1024)
$rng = New-Object System.Random(42)
for ($i = 0; $i -lt 20; $i++) {
    $rng.NextBytes($chunk)
    $fs.Write($chunk, 0, $chunk.Length)
}
$fs.Close()
$bigSize = (Get-Item $big).Length
Write-Host ("  (multipart test file: {0:N1} MB)" -f ($bigSize / 1MB)) -ForegroundColor DarkGray
Check "s3 cp (multipart upload, 20 MiB)" { & $aws s3 cp $big "s3://$bucket/big.bin" @S3 | Out-Null }
Check "s3api head-object size matches" {
    $h = & $aws s3api head-object --bucket $bucket --key big.bin @S3 | ConvertFrom-Json
    if ([int64]$h.ContentLength -ne $bigSize) { throw "size $($h.ContentLength) != $bigSize" }
}

# --- reads -----------------------------------------------------------------
Check "s3 cp download and compare" {
    $back = Join-Path $work "big-back.bin"
    & $aws s3 cp "s3://$bucket/big.bin" $back @S3 | Out-Null
    $a = (Get-FileHash $big -Algorithm SHA256).Hash
    $b = (Get-FileHash $back -Algorithm SHA256).Hash
    if ($a -ne $b) { throw "download mismatch" }
}
Check "s3api range read" {
    $r = Join-Path $work "range.bin"
    & $aws s3api get-object --bucket $bucket --key big.bin --range "bytes=100-199" $r @S3 | Out-Null
    if ((Get-Item $r).Length -ne 100) { throw "range length $((Get-Item $r).Length)" }
}
Check "s3 ls --recursive" {
    $r = & $aws s3 ls "s3://$bucket/" --recursive @S3
    $n = ($r | Where-Object { $_ -match '\S' }).Count
    if ($n -lt 4) { throw "expected >= 4 objects, got $n" }
}
Check "s3api list-objects-v2 with prefix and delimiter" {
    $raw = & $aws s3api list-objects-v2 --bucket $bucket --prefix "upload/" --delimiter "/" @S3 | Out-String
    $r = $raw | ConvertFrom-Json
    $keys = @($r.Contents | Where-Object { $_ }).Count
    $prefixes = @($r.CommonPrefixes | Where-Object { $_ } | ForEach-Object { $_.Prefix })
    if ($keys -ne 1) { throw "expected 1 key, got $keys (raw: $raw)" }
    if ($prefixes -notcontains "upload/nested/") { throw "missing nested prefix (got $($prefixes -join ','))" }
}
Check "s3api list-objects (v1) with delimiter" {
    $raw = & $aws s3api list-objects --bucket $bucket --delimiter "/" @S3 | Out-String
    $r = $raw | ConvertFrom-Json
    $prefixes = @($r.CommonPrefixes | Where-Object { $_ } | ForEach-Object { $_.Prefix })
    if ($prefixes.Count -lt 1) { throw "expected CommonPrefixes (raw: $raw)" }
}
Check "s3 sync (upload)" {
    $syncDir = Join-Path $work "sync"
    New-Item -ItemType Directory -Force -Path $syncDir | Out-Null
    1..3 | ForEach-Object { Set-Content (Join-Path $syncDir "f$_.txt") "x$_" -NoNewline }
    & $aws s3 sync $syncDir "s3://$bucket/sync/" @S3 | Out-Null
    $raw = & $aws s3api list-objects-v2 --bucket $bucket --prefix "sync/" @S3 | Out-String
    $r = $raw | ConvertFrom-Json
    $count = if ($null -eq $r.KeyCount) { @($r.Contents).Count } else { [int]$r.KeyCount }
    if ($count -ne 3) { throw "sync uploaded $count" }
}
Check "s3 sync (download)" {
    $dest = Join-Path $work "sync-back"
    & $aws s3 sync "s3://$bucket/sync/" $dest @S3 | Out-Null
    if ((Get-ChildItem $dest -File).Count -ne 3) { throw "sync downloaded wrong count" }
}

# --- metadata / headers ----------------------------------------------------
Check "s3api put-object with metadata and content-type" {
    & $aws s3api put-object --bucket $bucket --key meta.txt --body $small `
        --content-type "text/markdown" --metadata "team=storage,env=test" @S3 | Out-Null
    $h = & $aws s3api head-object --bucket $bucket --key meta.txt @S3 | ConvertFrom-Json
    if ($h.ContentType -ne "text/markdown") { throw "content-type '$($h.ContentType)'" }
    if ($h.Metadata.team -ne "storage") { throw "metadata team '$($h.Metadata.team)'" }
    if ($h.Metadata.env -ne "test") { throw "metadata env '$($h.Metadata.env)'" }
}
Check "s3api copy-object" {
    & $aws s3api copy-object --bucket $bucket --key copied.txt --copy-source "$bucket/small.txt" @S3 | Out-Null
    $h = & $aws s3api head-object --bucket $bucket --key copied.txt @S3 | ConvertFrom-Json
    if ([int64]$h.ContentLength -ne (Get-Item $small).Length) { throw "copy size mismatch" }
}
Check "s3api delete-objects (batch)" {
    # Pass the document as a file: passing JSON inline is quoted differently by
    # cmd vs PowerShell and silently mangles it.
    # Write the document with an explicit BOM-less UTF-8 encoder: on Windows
    # PowerShell 5.1 `Set-Content -Encoding utf8` prepends a BOM, and the AWS
    # CLI's paramfile parser then fails with "Expected: '=', received: '\ufeff'".
    $doc = '{"Objects":[{"Key":"copied.txt"},{"Key":"meta.txt"}],"Quiet":false}'
    $del = Join-Path $work "delete.json"
    [System.IO.File]::WriteAllText($del, $doc, (New-Object System.Text.UTF8Encoding($false)))
    $raw = (& $aws s3api delete-objects --bucket $bucket --delete "file://$($del.Replace('\','/'))" @S3 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) { throw "delete-objects failed (exit $LASTEXITCODE): $raw" }
    $r = $raw | ConvertFrom-Json
    $deleted = @($r.Deleted | Where-Object { $_ })
    if ($deleted.Count -ne 2) { throw "expected 2 Deleted entries, got $($deleted.Count) (raw: $raw)" }
}

# --- presigned URL ---------------------------------------------------------
Check "presign then GET without credentials" {
    $url = (& $aws s3 presign "s3://$bucket/small.txt" --expires-in 300 @S3).Trim()
    if (-not $url) { throw "no URL returned" }
    $body = & curl.exe -s --fail "$url"
    if ($LASTEXITCODE -ne 0) { throw "GET on presigned URL failed" }
    if ($body -ne "hello from the aws cli") { throw "body '$body'" }
}

# --- error handling --------------------------------------------------------
Check "head-bucket on a missing bucket fails with 404" {
    & $aws s3api head-bucket --bucket "no-such-bucket-hj45" @S3 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { throw "expected a failure, got exit 0" }
}
Check "wrong secret produces 403 SignatureDoesNotMatch" {
    $good = $env:AWS_SECRET_ACCESS_KEY
    $env:AWS_SECRET_ACCESS_KEY = "definitely-not-the-secret"
    try {
        & $aws s3 ls "s3://$bucket/" @S3 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { throw "signed with a bad secret but succeeded" }
    } finally {
        $env:AWS_SECRET_ACCESS_KEY = $good
    }
}

# --- teardown --------------------------------------------------------------
Check "s3 rm --recursive then rb" {
    & $aws s3 rm "s3://$bucket/" --recursive @S3 | Out-Null
    & $aws s3 rb "s3://$bucket" @S3 | Out-Null
}

Write-Host ""
Write-Host "passed: $script:pass   failed: $script:fail" -ForegroundColor $(if ($script:fail) { "Red" } else { "Green" })
Remove-Item -Recurse -Force $configRoot -ErrorAction SilentlyContinue
if ($script:fail -gt 0) { exit 1 }
exit 0













