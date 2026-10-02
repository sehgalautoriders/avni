# Puts the current promoted AVNI Phone APK on https://sehgalautoriders.github.io/avni/ (GitHub release, asset AVNI-Phone.apk).
# Ravi 02-10-2026: "keep uploading any updates there". Runs from the scheduled task "\AVNI\AVNI Phone Download Page" in the
# DAY window (06:30-22:30) only. It never builds, promotes, versions or installs anything (Directive 0015): it only copies
# what AVNI Sentinel already promoted, after checking the file's sha256 against the canonical provenance.
param([switch]$Force)
$ErrorActionPreference = 'Stop'
$repo = 'sehgalautoriders/avni'
$wp = 'C:\02 - OUR FILES\Access to all drives\AVNI\90 - WORKTREES\Active\WP-0036 - AVNI PHONE V0.1'
$state = Join-Path $PSScriptRoot 'last-run.json'

function Save($o) { $o.at = (Get-Date -Format 'dd-MM-yyyy HH:mm:ss'); $o | ConvertTo-Json | Set-Content -Path $state -Encoding utf8 }

$now = Get-Date
$mins = $now.Hour * 60 + $now.Minute
if (-not $Force -and ($mins -lt 390 -or $mins -ge 1350)) { Save @{ result = 'night - nothing published (day window 06:30-22:30)' }; exit 0 }

$prov = Get-Content (Join-Path $wp 'releases\canonical\AVNI.apk.provenance.json') -Raw | ConvertFrom-Json
$ver = $prov.versionName
$src = Join-Path $wp "releases\AVNI-Phone-$ver.apk"
if (-not (Test-Path -LiteralPath $src)) { Save @{ result = "no releases\AVNI-Phone-$ver.apk for the canonical $ver" }; exit 1 }
if ((Get-FileHash -LiteralPath $src -Algorithm SHA256).Hash.ToLower() -ne $prov.sha256.ToLower()) {
    Save @{ result = "AVNI-Phone-$ver.apk does not match the canonical sha256 - not published" }; exit 1
}

$tag = "v$ver"
$have = (gh release view --repo $repo --json tagName -q .tagName 2>$null)
if ($have -eq $tag) { Save @{ result = "already published $tag" }; exit 0 }
if ($have) {
    $a = [version]($have.TrimStart('v')); $b = [version]$ver
    if ($a -gt $b) { Save @{ result = "page has $have, newer than canonical $ver - left alone" }; exit 0 }
}

$tmp = Join-Path $env:TEMP "avni-dl-$ver"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    $apk = Join-Path $tmp 'AVNI-Phone.apk'
    Copy-Item -LiteralPath $src -Destination $apk
    $notes = "AVNI Phone $ver (code $($prov.versionCode)), promoted $($prov.promoted_at) from $($prov.git_head.Substring(0,7)). sha256 $($prov.sha256)"
    gh release create $tag $apk --repo $repo --title "AVNI Phone $ver" --notes $notes --latest | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "gh release create failed ($LASTEXITCODE)" }
    $up = gh release view $tag --repo $repo --json assets -q '.assets[] | select(.name=="AVNI-Phone.apk") | .size'
    if ([int64]$up -ne (Get-Item -LiteralPath $src).Length) { throw "uploaded size $up differs from $((Get-Item -LiteralPath $src).Length)" }
    Save @{ result = "published $tag ($up bytes)" }
} catch {
    Save @{ result = "FAILED: $($_.Exception.Message)" }; exit 1
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
