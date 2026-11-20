<#
.SYNOPSIS
  Publishes a new Sentinel version. Installed copies update themselves (silently, while hidden).

.EXAMPLE
  .\release.ps1 1.1.0 "Faster startup and a new Disk view"

  1. Sets the version in Cargo.toml
  2. Checks that it builds and the tests pass
  3. Commits all changes, tags them v1.1.0 and pushes; the "Release" workflow on GitHub
     builds Sentinel.exe and publishes the release
#>
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Notes = "",
    [switch]$Wait   # wait for GitHub to finish building and show the result
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3" }
$tag = "v$Version"
if (git tag --list $tag) { throw "$tag already exists. Pick a higher version." }

$toml = Get-Content Cargo.toml -Raw
$current = [regex]::Match($toml, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if ([version]$Version -le [version]$current) { throw "New version $Version must be higher than $current." }

# 1. Version number (first `version =` line is the package's)
$toml = [regex]::Replace($toml, '(?m)^version\s*=\s*"[^"]+"', "version = `"$Version`"", 1)
Set-Content Cargo.toml $toml -NoNewline

# 2. Make sure it still builds and passes before publishing anything
cargo test --release --quiet
if ($LASTEXITCODE -ne 0) { throw "Tests failed - nothing was published." }
cargo build --release --quiet
if ($LASTEXITCODE -ne 0) { throw "Build failed - nothing was published." }

# 3. Commit, tag (the tag message becomes the release notes) and push
if (-not $Notes) { $Notes = "Sentinel $Version" }
git add -A
git commit -m "Release $tag" -m $Notes
git tag -a $tag -m $Notes
git push
git push origin $tag

Write-Host ""
Write-Host "Pushed $tag. GitHub is building the release:" -ForegroundColor Green
Write-Host "  https://github.com/JustSad42/Sentinel/actions"
if ($Wait) {
    Start-Sleep -Seconds 5
    gh run watch (gh run list --workflow release.yml --limit 1 --json databaseId -q ".[0].databaseId") --exit-status
    gh release view $tag
}
