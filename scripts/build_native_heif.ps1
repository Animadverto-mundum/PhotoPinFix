# Build-time only, for Windows MSVC. No Python/Perl or codec DLL is needed by
# the packaged application. Run from a VS developer shell or GitHub Actions.
param(
    [ValidateSet("x64", "arm64")][string]$Architecture = "x64",
    [string]$NativeRoot = ""
)
$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $NativeRoot) { $NativeRoot = Join-Path $ProjectRoot "tools/native-heif" }
New-Item -ItemType Directory -Force -Path $NativeRoot | Out-Null
$NativeRoot = (Resolve-Path $NativeRoot).Path
$VcpkgRoot = Join-Path $NativeRoot "vcpkg"
$VcpkgTag = "2026.07.29"
$Triplet = "$Architecture-windows-static"
if (-not (Test-Path (Join-Path $VcpkgRoot ".git"))) {
    & git clone --depth 1 --branch $VcpkgTag https://github.com/microsoft/vcpkg.git $VcpkgRoot
    if ($LASTEXITCODE -ne 0) { throw "vcpkg checkout failed" }
}
& git -C $VcpkgRoot describe --tags --exact-match HEAD
if ($LASTEXITCODE -ne 0) { throw "vcpkg checkout must remain pinned to $VcpkgTag" }
$CurrentTag = (& git -C $VcpkgRoot describe --tags --exact-match HEAD).Trim()
if ($CurrentTag -ne $VcpkgTag) { throw "Expected vcpkg $VcpkgTag, found $CurrentTag" }
& (Join-Path $VcpkgRoot "bootstrap-vcpkg.bat") -disableMetrics
if ($LASTEXITCODE -ne 0) { throw "vcpkg bootstrap failed" }
# In this pinned vcpkg release libde265 is a core dependency; the default 'hevc'
# feature enables x265 encoding, which this preview-only application does not use.
& (Join-Path $VcpkgRoot "vcpkg.exe") install "libheif[core]:$Triplet" --disable-metrics
if ($LASTEXITCODE -ne 0) { throw "static HEIF dependencies failed to build" }
$env:VCPKG_ROOT = $VcpkgRoot
$env:VCPKGRS_TRIPLET = $Triplet
$env:PCRE2_SYS_STATIC = "1"
Remove-Item Env:VCPKGRS_DYNAMIC -ErrorAction SilentlyContinue
$env:RUSTFLAGS = (($env:RUSTFLAGS + " -C target-feature=+crt-static").Trim())
@"
`$env:VCPKG_ROOT = Join-Path `$PSScriptRoot 'vcpkg'
`$env:VCPKGRS_TRIPLET = '$Triplet'
`$env:PCRE2_SYS_STATIC = '1'
Remove-Item Env:VCPKGRS_DYNAMIC -ErrorAction SilentlyContinue
`$env:RUSTFLAGS = ((`$env:RUSTFLAGS + ' -C target-feature=+crt-static').Trim())
"@ | Set-Content -Encoding UTF8 (Join-Path $NativeRoot "env.ps1")
Write-Host "Static HEIC dependencies built. Dot-source tools/native-heif/env.ps1 before cargo builds."
