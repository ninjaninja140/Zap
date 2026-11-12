param(
    [string]$Version = "latest"
)
$ErrorActionPreference = "Stop"

Write-Host "  ⚡  zap installer"
Write-Host "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔"

function Get-Arch {
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { return "x64" }
        "ARM64" { return "arm64" }
        default { Write-Error "unsupported windows arch: $env:PROCESSOR_ARCHITECTURE" }
    }
}

function Test-NpmZap {
    $cmd = Get-Command zap -ErrorAction SilentlyContinue
    if (-not $cmd) { return $false }
    $p = $cmd.Source
    if ($p -match 'node_modules') { return $true }
    try {
        if ((Get-Content $p -Raw -ErrorAction SilentlyContinue) -match '@zappkg/cli') { return $true }
    } catch {}
    return $false
}

function Test-ScriptZap {
    return (Test-Path "$HOME\.zap\bin\zap.exe") -or (Test-Path "$HOME\.zap\bin\zap")
}

if ($env:ZAP_INSTALLER_TEST -eq '1') { return }

$arch = Get-Arch
$dest = Join-Path $env:USERPROFILE ".zap\bin"
New-Item -ItemType Directory -Force -Path $dest | Out-Null

# explicit version pin replaces
if ($Version -ne "latest") {
    if (Test-NpmZap) {
        if (Get-Command npm -ErrorAction SilentlyContinue) {
            Write-Host "> removing npm install of zap"
            npm uninstall -g @zappkg/cli 2>$null
            if ($LASTEXITCODE -ne 0) {
                Write-Warning "npm uninstall failed, installing anyway"
            }
        } else {
            Write-Warning "npm-managed zap detected but npm is unavailable, installing anyway"
        }
    } elseif (Test-ScriptZap) {
        Write-Host "> removing previous script install"
        Remove-Item "$HOME\.zap\bin\zap.exe", "$HOME\.zap\bin\zap" -ErrorAction SilentlyContinue -Force
    }
}

$triple = if ($arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }

$tmp = Join-Path $env:TEMP "zap-install-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

$downloaded = $false
$url = "https://github.com/ninjaninja140/zap/releases/download/v$Version/zap-$triple.zip"
Write-Host "> downloading $url"

$job = Start-Job {
    param($u, $o)
    Invoke-WebRequest -Uri $u -OutFile $o -UseBasicParsing -TimeoutSec 60
} -ArgumentList $url, "$tmp\zap.zip"
$spin = '|/-\'; $i = 0
try {
    while ($job.State -eq 'Running') {
        Write-Host -NoNewline ("`b" + $spin[$i % 4])
        $i++
        Start-Sleep -Milliseconds 150
    }
} finally {
    Receive-Job -Job $job -Wait -ErrorAction SilentlyContinue | Out-Null
    Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
}
if (Test-Path "$tmp\zap.zip") {
    $downloaded = $true
} else {
    Write-Host ""
    Write-Host "> failed: $url"
}
if (-not $downloaded) {
    Write-Error "couldn't download zap $Version for windows-$arch from $url"
}

Expand-Archive -Path "$tmp\zap.zip" -DestinationPath $tmp\out -Force
$exe = Get-ChildItem -Recurse $tmp\out -Filter "zap.exe" | Select-Object -First 1
if (-not $exe) { Write-Error "the archive held no zap.exe" }
Copy-Item $exe.FullName "$dest\zap.exe" -Force
Remove-Item $tmp -Recurse -Force

Write-Host ""
Write-Host "zap installed to $dest\zap.exe" -ForegroundColor Green

# add to the USER path
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$dest*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dest", "User")
    Write-Host "added $dest to your user PATH" -ForegroundColor Green
    Write-Host "~ reopen your terminal so it picks the change up" -ForegroundColor Yellow
}
