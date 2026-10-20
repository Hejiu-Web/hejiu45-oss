# End-to-end S3 compatibility test for hejiu45-oss.
#
# Signs real SigV4 requests and asserts responses. Works on Windows PowerShell
# 5.1 and PowerShell 7: HTTP goes through curl.exe so behaviour does not depend
# on the shell's web cmdlets.
#
# Usage (server must be running and an access key provisioned):
#   $env:HEJIU45_OSS_AK = "HJ45..."
#   $env:HEJIU45_OSS_SK = "..."
#   ./scripts/e2e-s3.ps1 -Base http://127.0.0.1:9000
param(
    [string]$Base = "http://127.0.0.1:9000",
    [string]$AccessKey = $env:HEJIU45_OSS_AK,
    [string]$SecretKey = $env:HEJIU45_OSS_SK,
    [string]$Region = "us-east-1",
    # The console owns `/`, so S3 is served under a prefix by default.
    [string]$Prefix = "/s3"
)

$ErrorActionPreference = "Stop"
if (-not $AccessKey -or -not $SecretKey) {
    Write-Host "Set HEJIU45_OSS_AK and HEJIU45_OSS_SK (see --init-access-key)." -ForegroundColor Red
    exit 2
}
$curl = "curl.exe"

function SHA256-Hex { [CmdletBinding()] param([byte[]]$Bytes)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    (($sha.ComputeHash($Bytes)) | ForEach-Object { $_.ToString("x2") }) -join ""
}
function HMAC-Bytes([byte[]]$Key, [string]$Data) {
    $h = New-Object System.Security.Cryptography.HMACSHA256(,$Key)
    return $h.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($Data))
}
function Uri-Encode([string]$s) {
    # RFC3986: unreserved characters stay literal.
    return [System.Uri]::EscapeDataString($s).Replace("%7E", "~")
}

function Get-SignedRequest {
    [CmdletBinding()]
    param([string]$Method, [string]$Path, [string]$Query = "", [hashtable]$Extra = @{}, [byte[]]$Body = $null)
    if ($null -eq $Body -or $Body.Length -eq 0) { $Body = [byte[]]@() }
    $now = [DateTime]::UtcNow
    $amzDate = $now.ToString("yyyyMMddTHHmmssZ")
    $dateStamp = $now.ToString("yyyyMMdd")
    $payloadHash = SHA256-Hex $Body
    $hostHeader = ([uri]$Base).Authority
    # SigV4 signs the full path including the S3 mount prefix.

    $hdrs = [ordered]@{}
    foreach ($k in $Extra.Keys) { $hdrs[$k.ToLower()] = "$($Extra[$k])" }
    $hdrs["host"] = $hostHeader
    $hdrs["x-amz-date"] = $amzDate
    $hdrs["x-amz-content-sha256"] = $payloadHash

    $pairs = New-Object System.Collections.ArrayList
    if ($Query) {
        foreach ($kv in $Query.Split("&")) {
            if (-not $kv) { continue }
            $p = $kv.Split("=", 2)
            [void]$pairs.Add([pscustomobject]@{ K = (Uri-Encode ([uri]::UnescapeDataString($p[0]))); V = (Uri-Encode $(if ($p.Count -gt 1) { [uri]::UnescapeDataString($p[1]) } else { "" })) })
        }
    }
    $sortedPairs = $pairs | Sort-Object { "$($_.K)=$($_.V)" }
    $canonicalQuery = (($sortedPairs | ForEach-Object { "$($_.K)=$($_.V)" }) -join "&")

    $names = @($hdrs.Keys | Sort-Object)
    $canonicalHeaders = (($names | ForEach-Object { "$($_):$($hdrs[$_])" }) -join "`n")
    $signedHeaders = $names -join ";"

    $canonicalRequest = "$Method`n$Path`n$canonicalQuery`n$canonicalHeaders`n`n$signedHeaders`n$payloadHash"
    $scope = "$dateStamp/$Region/s3/aws4_request"
    $stringToSign = "AWS4-HMAC-SHA256`n$amzDate`n$scope`n" + (SHA256-Hex ([System.Text.Encoding]::UTF8.GetBytes($canonicalRequest)))

    $kDate = HMAC-Bytes ([System.Text.Encoding]::UTF8.GetBytes("AWS4$SecretKey")) $dateStamp
    $kRegion = HMAC-Bytes $kDate $Region
    $kService = HMAC-Bytes $kRegion "s3"
    $kSigning = HMAC-Bytes $kService "aws4_request"
    $sig = (HMAC-Bytes $kSigning $stringToSign | ForEach-Object { $_.ToString("x2") }) -join ""

    $hdrs["authorization"] = "AWS4-HMAC-SHA256 Credential=$AccessKey/$scope, SignedHeaders=$signedHeaders, Signature=$sig"

    return [pscustomobject]@{
        Method = $Method
        Url    = "$Base$Path" + $(if ($Query) { "?$Query" } else { "" })
        Headers = $hdrs
        Body   = $Body
        Signature = $sig
    }
}

function Invoke-Signed {
    [CmdletBinding()]
    param(
        [string]$Method,
        [string]$Path,
        [string]$Query = "",
        [hashtable]$Extra = @{},
        [string]$BodyText = $null,
        # Typed [object], not [byte[]]: PowerShell can silently coerce a
        # byte[] argument to an *empty* collection when a more specific
        # collection type is requested, leaving the body empty while the
        # signature still covers the empty hash.
        [object]$BodyBytes = $null
    )
    # Use [string]::IsNullOrEmpty, NOT `$null -ne $BodyText`: for a [string]
    # parameter that was not passed, PowerShell binds it to "" and `$null -ne ""`
    # is TRUE, so the guard fired and overwrote the binary body with the bytes
    # of an empty string.
    if (-not [string]::IsNullOrEmpty($BodyText)) {
        $BodyBytes = [System.Text.Encoding]::UTF8.GetBytes($BodyText)
    }
    if ($null -eq $BodyBytes) {
        $bytes = [byte[]]@()
    } elseif ($BodyBytes -is [byte[]]) {
        $bytes = $BodyBytes
    } else {
        $bytes = [byte[]]@($BodyBytes)
    }
    $req = Get-SignedRequest -Method $Method -Path ($Prefix + $Path) -Query $Query -Extra $Extra -Body $bytes
    $headerArgs = @()
    foreach ($k in $req.Headers.Keys) {
        if ($k -eq "host") { continue }
        $headerArgs += @("-H", "$k`: $($req.Headers[$k])")
    }
    $bodyFile = $null
    if ($req.Body.Length -gt 0) {
        $bodyFile = [System.IO.Path]::GetTempFileName()
        [System.IO.File]::WriteAllBytes($bodyFile, $req.Body)
        $headerArgs += @("--data-binary", "@$bodyFile")
    }
    $respHeaders = [System.IO.Path]::GetTempFileName()
    # `-X HEAD` makes curl wait for a body that a correct server never sends;
    # `-I` is the proper HEAD request and terminates immediately.
    $methodArgs = if ($Method -eq "HEAD") { @("-I") } else { @("-X", $Method) }
    # NB: never name this `$args` — it is a read-only automatic variable in
    # PowerShell, and assigning to it silently drops the curl arguments.
    $curlArgs = @("-s", "--max-time", "30", "-o", "-", "-D", $respHeaders, "-w", "`n%{http_code}") + $methodArgs + @($req.Url) + $headerArgs
    $raw = & $curl @curlArgs 2>&1
    $status = [int]($raw[-1].ToString().Trim())
    $body = ($raw[0..($raw.Count - 2)] -join "`n")
    $hdrs = @{}
    foreach ($line in (Get-Content $respHeaders -Encoding utf8)) {
        if ($line -match "^(?<name>[A-Za-z0-9\-]+):\s*(?<value>.*)$") { $hdrs[$Matches.name.ToLower()] = $Matches.value.Trim() }
    }
    if ($bodyFile) { Remove-Item $bodyFile -ErrorAction SilentlyContinue }
    Remove-Item $respHeaders -ErrorAction SilentlyContinue
    return [pscustomobject]@{ Status = $status; Body = $body; Headers = $hdrs; Signature = $req.Signature; Signed = $req }
}

$failCount = 0
function Check([string]$Name, [scriptblock]$Test) {
    try {
        & $Test
        Write-Host "  [PASS] $Name" -ForegroundColor Green
    } catch {
        $script:failCount++
        Write-Host "  [FAIL] $Name -> $($_.Exception.Message)" -ForegroundColor Red
    }
}

$bucket = "e2e-" + ([guid]::NewGuid().ToString("N").Substring(0, 10))
Write-Host "S3 end-to-end against $Base (bucket $bucket)" -ForegroundColor Cyan

Check "CreateBucket" {
    $r = Invoke-Signed -Method PUT -Path "/$bucket"
    if ($r.Status -ne 200) { throw "status $($r.Status): $($r.Body)" }
}
Check "HeadBucket" {
    $r = Invoke-Signed -Method HEAD -Path "/$bucket"
    if ($r.Status -ne 200) { throw "status $($r.Status)" }
}
Check "Duplicate CreateBucket -> 409" {
    $r = Invoke-Signed -Method PUT -Path "/$bucket"
    if ($r.Status -ne 409) { throw "status $($r.Status)" }
}
Check "ListBuckets contains bucket" {
    $r = Invoke-Signed -Method GET -Path "/"
    if ($r.Body -notmatch [regex]::Escape("<Name>$bucket</Name>")) { throw "bucket missing: $($r.Body)" }
}
Check "PutObject" {
    $r = Invoke-Signed -Method PUT -Path "/$bucket/hello.txt" -BodyText "hello world" -Extra @{ "content-type" = "text/plain" }
    if ($r.Status -ne 200) { throw "status $($r.Status): $($r.Body)" }
}
Check "GetObject body + ETag" {
    $r = Invoke-Signed -Method GET -Path "/$bucket/hello.txt"
    if ($r.Body -ne "hello world") { throw "body '$($r.Body)'" }
    if ($r.Headers["etag"] -ne '"5eb63bbbe01eeed093cb22bb8f5acdc3"') { throw "etag $($r.Headers['etag'])" }
}
Check "HeadObject metadata" {
    $r = Invoke-Signed -Method HEAD -Path "/$bucket/hello.txt"
    if ($r.Headers["content-length"] -ne "11") { throw "length $($r.Headers['content-length'])" }
    if ($r.Headers["content-type"] -ne "text/plain") { throw "content-type $($r.Headers['content-type'])" }
}
Check "Range GET bytes=6-10 -> 206" {
    $r = Invoke-Signed -Method GET -Path "/$bucket/hello.txt" -Extra @{ range = "bytes=6-10" }
    if ($r.Status -ne 206) { throw "status $($r.Status)" }
    if ($r.Body -ne "world") { throw "body '$($r.Body)'" }
    if ($r.Headers["content-range"] -ne "bytes 6-10/11") { throw "content-range $($r.Headers['content-range'])" }
}
Check "Overwrite object changes body" {
    $r = Invoke-Signed -Method PUT -Path "/$bucket/hello.txt" -BodyText "replaced"
    if ($r.Status -ne 200) { throw "status $($r.Status)" }
    $g = Invoke-Signed -Method GET -Path "/$bucket/hello.txt"
    if ($g.Body -ne "replaced") { throw "body '$($g.Body)'" }
}
Check "Unicode + space key" {
    $key = "dir/%E4%B8%AD%E6%96%87%20file.txt"
    $r = Invoke-Signed -Method PUT -Path "/$bucket/$key" -BodyText "unicode"
    if ($r.Status -ne 200) { throw "put $($r.Status): $($r.Body)" }
    $g = Invoke-Signed -Method GET -Path "/$bucket/$key"
    if ($g.Body -ne "unicode") { throw "get '$($g.Body)'" }
}
Check "Binary object with PNG magic" {
    $png = [byte[]](0x89,0x50,0x4E,0x47,0x0D,0x0A,0x1A,0x0A,1,2,3,4)
    $r = Invoke-Signed -Method PUT -Path "/$bucket/img.png" -BodyBytes $png
    if ($r.Status -ne 200) { throw "status $($r.Status)" }
    $h = Invoke-Signed -Method HEAD -Path "/$bucket/img.png"
    if ($h.Headers["content-type"] -ne "image/png") { throw "sniffed type '$($h.Headers['content-type'])'" }
}
Check "CopyObject" {
    $r = Invoke-Signed -Method PUT -Path "/$bucket/copy.txt" -Extra @{ "x-amz-copy-source" = "/$bucket/hello.txt" }
    if ($r.Status -ne 200) { throw "status $($r.Status): $($r.Body)" }
    $g = Invoke-Signed -Method GET -Path "/$bucket/copy.txt"
    if ($g.Body -ne "replaced") { throw "body '$($g.Body)'" }
}
Check "ListObjectsV2 delimiter groups keys" {
    $r = Invoke-Signed -Method GET -Path "/$bucket" -Query "list-type=2&prefix=&delimiter=%2F"
    if ($r.Body -notmatch "<CommonPrefixes>") { throw "no CommonPrefixes: $($r.Body)" }
}
Check "ListObjectsV2 pagination" {
    $r = Invoke-Signed -Method GET -Path "/$bucket" -Query "list-type=2&max-keys=1"
    if ($r.Body -notmatch "<IsTruncated>true</IsTruncated>") { throw "not truncated" }
    if ($r.Body -notmatch "<NextContinuationToken>") { throw "no token" }
}
Check "DeleteObjects batch" {
    $xml = "<Delete><Object><Key>hello.txt</Key></Object><Object><Key>copy.txt</Key></Object></Delete>"
    $r = Invoke-Signed -Method POST -Path "/$bucket" -Query "delete" -BodyText $xml
    if ($r.Status -ne 200) { throw "status $($r.Status): $($r.Body)" }
    if ($r.Body -notmatch "<Deleted><Key>hello.txt</Key></Deleted>") { throw "content $($r.Body)" }
}
Check "DeleteObject is idempotent -> 204" {
    $r1 = Invoke-Signed -Method DELETE -Path "/$bucket/hello.txt"
    $r2 = Invoke-Signed -Method DELETE -Path "/$bucket/hello.txt"
    if ($r1.Status -ne 204 -or $r2.Status -ne 204) { throw "statuses $($r1.Status)/$($r2.Status)" }
}
Check "Multipart upload round-trip" {
    $key = "multipart.bin"
    $r = Invoke-Signed -Method POST -Path "/$bucket" -Query "uploads&key=$key"
    if ($r.Status -ne 200) { throw "init $($r.Status): $($r.Body)" }
    $uploadId = [regex]::Match($r.Body, "<UploadId>(.+?)</UploadId>").Groups[1].Value
    if (-not $uploadId) { throw "no upload id" }
    $payload = [byte[]](1..1024 | ForEach-Object { [byte]($_ % 256) })
    $partsXml = @()
    foreach ($n in 1..2) {
        $pr = Invoke-Signed -Method PUT -Path "/$bucket/$key" -Query "partNumber=$n&uploadId=$uploadId" -BodyBytes $payload
        if ($pr.Status -ne 200) { throw "part $n -> $($pr.Status)" }
        $partsXml += "<Part><PartNumber>$n</PartNumber><ETag>$($pr.Headers['etag'])</ETag></Part>"
    }
    $complete = "<CompleteMultipartUpload>$($partsXml -join '')</CompleteMultipartUpload>"
    $cr = Invoke-Signed -Method POST -Path "/$bucket/$key" -Query "uploadId=$uploadId" -BodyText $complete
    if ($cr.Status -ne 200) { throw "complete $($cr.Status): $($cr.Body)" }
    $h = Invoke-Signed -Method HEAD -Path "/$bucket/$key"
    if ($h.Headers["content-length"] -ne "2048") { throw "size $($h.Headers['content-length'])" }
}
Check "Unsigned request -> 403" {
    $code = & $curl -s -o NUL -w "%{http_code}" "$Base$Prefix/$bucket/x.txt"
    if ($code -ne "403") { throw "status $code" }
}
Check "Tampered signature -> 403" {
    $req = Get-SignedRequest -Method GET -Path "$Prefix/$bucket/hello.txt"
    $auth = $req.Headers["authorization"] -replace "([0-9a-f]{64})$", ("0" * 63 + $(if ($req.Signature[-1] -eq "0") { "1" } else { "0" }))
    $code = & $curl -s -o NUL -w "%{http_code}" -H "authorization: $auth" -H "x-amz-date: $($req.Headers['x-amz-date'])" -H "x-amz-content-sha256: $($req.Headers['x-amz-content-sha256'])" -H "host: $($req.Headers['host'])" "$Base$Prefix/$bucket/hello.txt"
    if ($code -ne "403") { throw "status $code" }
}
Check "Declared body hash mismatch -> 400" {
    # Sign with the hash of "" (empty) but send a different body.
    $req = Get-SignedRequest -Method PUT -Path "$Prefix/$bucket/mismatch.txt" -Body ([byte[]]@())
    $tmp = [System.IO.Path]::GetTempFileName()
    [System.IO.File]::WriteAllText($tmp, "not empty")
    $code = & $curl -s -o NUL -w "%{http_code}" -X PUT --data-binary "@$tmp" -H "authorization: $($req.Headers['authorization'])" -H "x-amz-date: $($req.Headers['x-amz-date'])" -H "x-amz-content-sha256: $($req.Headers['x-amz-content-sha256'])" "$Base$Prefix/$bucket/mismatch.txt"
    Remove-Item $tmp -ErrorAction SilentlyContinue
    if ($code -ne "400") { throw "status $code" }
}
Check "Non-empty DeleteBucket -> 409" {
    Invoke-Signed -Method PUT -Path "/$bucket/keep.txt" -BodyText "x" | Out-Null
    $r = Invoke-Signed -Method DELETE -Path "/$bucket"
    if ($r.Status -ne 409) { throw "status $($r.Status)" }
}
Check "Err-missing-bucket -> 404" {
    $r = Invoke-Signed -Method GET -Path "/no-such-bucket-xyz" -Query "list-type=2"
    if ($r.Status -ne 404) { throw "status $($r.Status)" }
    if ($r.Body -notmatch "<Code>NoSuchBucket</Code>") { throw "body $($r.Body)" }
}
Check "DeleteBucket when empty -> 204" {
    foreach ($k in @("keep.txt", "img.png", "dir/%E4%B8%AD%E6%96%87%20file.txt", "multipart.bin")) {
        Invoke-Signed -Method DELETE -Path "/$bucket/$k" | Out-Null
    }
    $r = Invoke-Signed -Method DELETE -Path "/$bucket"
    if ($r.Status -ne 204) { throw "status $($r.Status): $($r.Body)" }
}

Write-Host ""
if ($failCount -gt 0) {
    Write-Host "$failCount check(s) failed" -ForegroundColor Red
    exit 1
}
Write-Host "All S3 end-to-end checks passed" -ForegroundColor Green
exit 0












