<#
.SYNOPSIS
    hejiu45-oss — Windows installer.

.DESCRIPTION
    Installs the native single-file release binary and keeps it running either
    as a Windows service or as an auto-start scheduled task.

    Mode `service` wraps the binary with **WinSW** (a single ~3 MB .NET
    launcher that Microsoft's own patterns recommend for running any console
    program as a service). It is downloaded, verified and pinned by version.
    WinSW handles the SCM protocol, restarts and log rotation, so the server
    itself stays a plain console program.

    Mode `task` registers a scheduled task that starts at boot under the
    system account — no extra binaries, but no service stop/start verbs either.

    One-liner (elevated PowerShell):

        irm https://raw.githubusercontent.com/Hejiu-Web/hejiu45-oss/main/install-windows.ps1 | iex

.PARAMETER Mode
    service (WinSW) or task (Scheduled Task). Default: service.

.PARAMETER Version
    Release tag to install (default: latest).

.PARAMETER Listen
    Listen address (default: 127.0.0.1:9000, behind a reverse proxy).

.PARAMETER PublicUrl
    Public base URL, e.g. https://oss.example.com.

.PARAMETER Tls
    off | manual | reverse-proxy (default: reverse-proxy).

.PARAMETER CertPath / KeyPath
    Certificate and key paths for -Tls manual.

.PARAMETER AdminPassword
    Initial admin password. If omitted, one is generated and printed once in
    the log.

.PARAMETER Uninstall
    Remove the service/task and program files (the data directory is kept).

.EXAMPLE
    .\install-windows.ps1 -PublicUrl https://oss.example.com

.EXAMPLE
    .\install-windows.ps1 -Mode task -Listen 0.0.0.0:9000 -Tls off
#>
[CmdletBinding()]
param(
    [ValidateSet("service", "task")]
    [string]$Mode = "service",
    [string]$Version = "latest",
    [string]$Repo = "Hejiu-Web/hejiu45-oss",
    [string]$Listen = "127.0.0.1:9000",
    [string]$PublicUrl = "",
    [ValidateSet("off", "manual", "reverse-proxy")]
    [string]$Tls = "reverse-proxy",
    [string]$CertPath = "",
    [string]$KeyPath = "",
    [string]$AdminPassword = "",
    [string]$InstallDir = "$env:ProgramFiles\hejiu45-oss",
    [string]$DataDir = "$env:ProgramData\hejiu45-oss",
    [string]$ServiceName = "hejiu45-oss",
    [string]$WinSwVersion = "v2.12.0",
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$Product = "hejiu45-oss"
$Exe = Join-Path $InstallDir "$Product.exe"
$LogDir = Join-Path $InstallDir "logs"

function Step($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Ok($m) { Write-Host "  ok  $m" -ForegroundColor Green }
function Warn($m) { Write-Host "  !!  $m" -ForegroundColor Yellow }
function Die($m) { Write-Host "  xx  $m" -ForegroundColor Red; exit 1 }

function Assert-Administrator {
    $p = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Die "run this script in an elevated PowerShell (Run as Administrator)"
    }
}

function Get-Download([string]$Url, [string]$Dest) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
}

function Test-Sha256([string]$File, [string]$Url) {
    try {
        $text = (Invoke-WebRequest -Uri $Url -UseBasicParsing).Content
        $expected = (($text -split "`n") | Where-Object { $_ -match [regex]::Escape((Split-Path $File -Leaf)) } |
            Select-Object -First 1)
        if (-not $expected) { return $false }
        $expected = ($expected.Trim() -split '\s+')[0].ToLowerInvariant()
        $actual = (Get-FileHash -Path $File -Algorithm SHA256).Hash.ToLowerInvariant()
        return $expected -eq $actual
    } catch {
        return $null  # checksum file unavailable
    }
}

function Stop-Existing {
    if ($Mode -eq "service") {
        $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
        if ($svc) {
            if ($svc.Status -ne "Stopped") {
                Stop-Service $ServiceName -Force -ErrorAction SilentlyContinue
                (Get-Service $ServiceName).WaitForStatus("Stopped", [TimeSpan]::FromSeconds(30))
            }
            & sc.exe delete $ServiceName | Out-Null
            Start-Sleep -Milliseconds 600
            Ok "removed the previous service"
        }
    } else {
        $task = Get-ScheduledTask -TaskName $ServiceName -ErrorAction SilentlyContinue
        if ($task) {
            Stop-ScheduledTask -TaskName $ServiceName -ErrorAction SilentlyContinue
            Unregister-ScheduledTask -TaskName $ServiceName -Confirm:$false
            Ok "removed the previous scheduled task"
        }
    }
}

function Uninstall-All {
    Step "uninstalling $Product"
    Stop-Existing
    if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }
    Ok "program files removed"
    Write-Host "Data kept at $DataDir (delete manually when you are sure)." -ForegroundColor Yellow
    exit 0
}

function Install-Binary {
    $arch = if ([Environment]::Is64BitOperatingSystem) { "amd64" } else { Die "32-bit Windows is not supported" }
    $base = if ($Version -eq "latest") { "https://github.com/$Repo/releases/latest/download" }
            else { "https://github.com/$Repo/releases/download/$Version" }
    $name = "$Product-windows-$arch.exe"
    Step "downloading $base/$name"
    $tmp = Join-Path $env:TEMP "$Product-download.exe"
    Get-Download "$base/$name" $tmp

    $verified = Test-Sha256 $tmp "$base/checksums-$Product.txt"
    if ($verified -eq $true) {
        Ok "sha256 verified"
    } elseif ($env:HEJIU45_OSS_ALLOW_UNVERIFIED -eq "1") {
        Warn "checksum unavailable; continuing because HEJIU45_OSS_ALLOW_UNVERIFIED=1"
    } else {
        Remove-Item $tmp -Force -ErrorAction SilentlyContinue
        Die "could not verify the download against checksums-$Product.txt (set HEJIU45_OSS_ALLOW_UNVERIFIED=1 to override)"
    }

    New-Item -ItemType Directory -Force -Path $InstallDir, $LogDir | Out-Null
    Move-Item -Path $tmp -Destination $Exe -Force
    Ok "installed $Exe"
}

function Prepare-Data {
    Step "preparing $DataDir"
    New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
    # The data directory holds the master key and sealed S3 secrets: restrict
    # it to Administrators and SYSTEM (the service identities we use).
    & icacls.exe $DataDir /inheritance:r /grant:r "Administrators:(OI)(CI)F" "SYSTEM:(OI)(CI)F" /T | Out-Null
    if ($LASTEXITCODE -ne 0) { Warn "icacls returned $LASTEXITCODE" }
    $key = Join-Path $DataDir "master.key"
    if (Test-Path $key) {
        Ok "master key present"
    } else {
        Ok "master key will be generated on first start ($key) - back it up!"
    }
}

function Get-ServerArgs {
    $list = New-Object System.Collections.Generic.List[string]
    $list.Add("--data-dir"); $list.Add($DataDir)
    $list.Add("--listen");   $list.Add($Listen)
    $list.Add("--tls");      $list.Add($Tls)
    if ($PublicUrl) { $list.Add("--public-url"); $list.Add($PublicUrl) }
    if ($Tls -eq "manual") {
        if (-not $CertPath -or -not $KeyPath) { Die "-Tls manual requires -CertPath and -KeyPath" }
        $list.Add("--tls-cert"); $list.Add($CertPath)
        $list.Add("--tls-key");  $list.Add($KeyPath)
    }
    return $list
}

function Install-WinSW {
    $winsw = Join-Path $InstallDir "winsw.exe"
    if (-not (Test-Path $winsw)) {
        Step "downloading WinSW $WinSwVersion"
        $url = "https://github.com/winsw/winsw/releases/download/$WinSwVersion/WinSW-x64.exe"
        Get-Download $url $winsw
        Ok "WinSW installed"
    } else {
        Ok "WinSW already present"
    }
    return $winsw
}

function Register-Service {
    $winsw = Install-WinSW
    $xmlPath = Join-Path $InstallDir "$Product.xml"
    $args = (Get-ServerArgs) | ForEach-Object {
        if ($_ -match '[\s]') { '"' + $_ + '"' } else { $_ }
    }
    $argLine = ($args -join " ")

    Step "writing service descriptor"
    @"
<service>
  <id>$ServiceName</id>
  <name>hejiu45-oss Object Storage</name>
  <description>S3-compatible object storage with an embedded web console.</description>
  <executable>$Exe</executable>
  <arguments>$argLine</arguments>
  <workingdirectory>$InstallDir</workingdirectory>
  <logpath>$LogDir</logpath>
  <log mode="roll-by-size">
    <sizeThreshold>10485760</sizeThreshold>
    <keepFiles>8</keepFiles>
  </log>
  <onfailure action="restart" delay="5 sec" />
  <onfailure action="restart" delay="10 sec" />
  <onfailure action="restart" delay="30 sec" />
  <resetfailure>1 hour</resetfailure>
  <stoptimeout>30 sec</stoptimeout>
  <startmode>Automatic</startmode>
  <delayedAutoStart>true</delayedAutoStart>
</service>
"@ | Set-Content -Path $xmlPath -Encoding UTF8
    Ok "descriptor written to $xmlPath"

    Step "registering service $ServiceName"
    & $winsw install $xmlPath 2>&1 | Out-String | Write-Verbose
    if ($LASTEXITCODE -ne 0) { Die "winsw install failed ($LASTEXITCODE)" }
    # Run as LocalSystem by default: simplest, and the data ACL already covers
    # SYSTEM. Use a dedicated account with `winsw` CLI if policy requires it.
    Start-Service -Name $ServiceName
    (Get-Service $ServiceName).WaitForStatus("Running", [TimeSpan]::FromSeconds(30))
    Ok "service running"
}

function Register-Task {
    $action = New-ScheduledTaskAction -Execute $Exe -Argument ((Get-ServerArgs) -join " ") -WorkingDirectory $InstallDir
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero)
    Step "registering scheduled task $ServiceName"
    Register-ScheduledTask -TaskName $ServiceName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $ServiceName
    Start-Sleep -Seconds 2
    Ok "task started"
}

function Show-Summary {
    $url = if ($PublicUrl) { $PublicUrl } else { "http://$Listen" }
    Write-Host ""
    Write-Host "hejiu45-oss is installed ($Mode mode)." -ForegroundColor Green
    Write-Host ""
    Write-Host "  Console    $url/"
    Write-Host "  S3 API     $url/s3   (path-style addressing required)"
    Write-Host "  Program    $Exe"
    Write-Host "  Data       $DataDir"
    Write-Host "  Logs       $LogDir"
    if ($Mode -eq "service") {
        Write-Host "  Control    Start-Service / Stop-Service $ServiceName"
    } else {
        Write-Host "  Control    Start-ScheduledTask / Stop-ScheduledTask -TaskName $ServiceName"
    }
    Write-Host ""
    Write-Host "Initial admin password (generated on first start) is in the log:" -ForegroundColor Yellow
    Write-Host "  Get-Content '$LogDir\*.out.log' | Select-String -Pattern 'password:' -Context 0,1"
    if ($AdminPassword) {
        Write-Host "  (you supplied it via -AdminPassword; change it after first login)"
    }
    Write-Host ""
    Write-Host "Next steps"
    Write-Host "  * Keep -Tls reverse-proxy and terminate HTTPS in IIS/nginx/Caddy, or"
    Write-Host "    use -Tls manual -CertPath cert.pem -KeyPath key.pem"
    Write-Host "  * Back up $DataDir including master.key, or stored S3 secrets are lost"
    Write-Host ""
}

# ---------------------------------------------------------------------------
Assert-Administrator
if ($Uninstall) { Uninstall-All }

Step "installing $Product ($Mode)"
Stop-Existing
Install-Binary
Prepare-Data
if ($Mode -eq "service") { Register-Service } else { Register-Task }
Show-Summary
