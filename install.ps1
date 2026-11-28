<#
.SYNOPSIS
  Builds Sentinel and installs it to %ProgramFiles%\Sentinel with a Start menu shortcut.
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

# Program Files, because Sentinel runs elevated: no normal program may be able to replace it.
$dir = Join-Path $env:ProgramFiles "Sentinel"
$exe = Join-Path $dir "Sentinel.exe"
$src = (Resolve-Path target\release\sentinel.exe).Path

# Writing there, and stopping the elevated running copy, needs one UAC prompt.
$copy = @"
`$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force '$dir' | Out-Null
Get-Process Sentinel -ErrorAction SilentlyContinue | Where-Object { `$_.Path -eq '$exe' } | Stop-Process -Force
Start-Sleep -Milliseconds 500
Copy-Item '$src' '$exe' -Force
"@
$enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($copy))
$p = Start-Process powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru -ArgumentList "-NoProfile", "-EncodedCommand", $enc
if ($p.ExitCode -ne 0) { throw "Could not copy to $dir." }

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
