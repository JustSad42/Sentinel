<#
.SYNOPSIS
  Builds Sentinel and installs it to %LOCALAPPDATA%\Programs\Sentinel with a Start menu shortcut.
  Run it again to update a local install from source (GitHub releases update installed copies on their own).

.EXAMPLE
  .\install.ps1          # build, install, start (one UAC prompt the first time)
  .\install.ps1 -NoStart # install only
#>
param([switch]$NoStart)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

cargo build --release --quiet
if ($LASTEXITCODE -ne 0) { throw "Build failed." }

$dir = Join-Path $env:LOCALAPPDATA "Programs\Sentinel"
$exe = Join-Path $dir "Sentinel.exe"
New-Item -ItemType Directory -Force $dir | Out-Null

# Stop a running copy so the file can be replaced.
Get-Process Sentinel -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
Start-Sleep -Milliseconds 500
Copy-Item target\release\sentinel.exe $exe -Force

$lnk = Join-Path ([Environment]::GetFolderPath("Programs")) "Sentinel.lnk"
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($lnk)
$s.TargetPath = $exe
$s.WorkingDirectory = $dir
$s.Description = "Sentinel task manager"
$s.Save()

Write-Host "Installed to $exe" -ForegroundColor Green
if (-not $NoStart) {
    # First start asks for admin once; Sentinel then registers its own elevated sign-in task.
    Start-Process $exe
}
