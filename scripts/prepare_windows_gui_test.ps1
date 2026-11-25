#Requires -Version 7.0
# Developer/CI-only software OpenGL. This does not build or run any binary,
# install a system driver, or put DLLs in the application/release directory.
param([string]$Destination = "")

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw "This preparation script is for Windows x64 GUI tests" }

$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $Destination) { $Destination = Join-Path $ProjectRoot "tools/gui-test/mesa-x64" }
$Destination = [IO.Path]::GetFullPath($Destination)
$Version = "25.1.9"
$ArchiveName = "mesa3d-$Version-release-msvc.7z"
$ReleaseUrl = "https://github.com/pal1000/mesa-dist-win/releases/tag/$Version"
$DownloadUrl = "https://github.com/pal1000/mesa-dist-win/releases/download/$Version/$ArchiveName"
# GitHub's digest on the fixed upstream release's asset page:
# https://github.com/pal1000/mesa-dist-win/releases/expanded_assets/25.1.9
$ArchiveSha256 = "dd83c3c4cc4d7706c9e21b277388447a2fc5367b0912f9ee45621e36aeb3b7d2"
# Static PE inspection of this verified archive: opengl32 imports
# libgallium_wgl; that DLL imports only Windows system libraries. LLVM is
# already linked into it, so no libglapi, LLVM, OpenCL or Vulkan DLL is needed.
$RequiredDlls = @("opengl32.dll", "libgallium_wgl.dll")
$ExpectedDllSha256 = @{
    "opengl32.dll" = "4fc2d5faa0b5472debecfe89afc4bf82467370f37f3e24a9c4e138ebbe0d6693"
    "libgallium_wgl.dll" = "b53bc66e4f935adfe29cac459c77b49dbe9c91d3388b1f7d824045a94eb2aedb"
}

$SevenZipCommand = Get-Command 7z -ErrorAction SilentlyContinue
$SevenZip = if ($SevenZipCommand) { $SevenZipCommand.Source } else { $null }
if (-not $SevenZip) {
    $SevenZip = Join-Path $env:ProgramFiles "7-Zip/7z.exe"
    if (-not (Test-Path -LiteralPath $SevenZip -PathType Leaf)) {
        throw "7-Zip is required to extract the pinned GUI test driver"
    }
}
$Dumpbin = Get-Command dumpbin -ErrorAction Stop

function Assert-X64Dll([string]$Path) {
    $Stream = [IO.File]::OpenRead($Path)
    $Reader = [IO.BinaryReader]::new($Stream)
    try {
        if ($Reader.ReadUInt16() -ne 0x5a4d) { throw "Missing PE DOS signature: $Path" }
        $Stream.Position = 0x3c
        $HeaderOffset = $Reader.ReadUInt32()
        if ($HeaderOffset -gt $Stream.Length - 24) { throw "Invalid PE header offset: $Path" }
        $Stream.Position = $HeaderOffset
        if ($Reader.ReadUInt32() -ne 0x4550 -or $Reader.ReadUInt16() -ne 0x8664) {
            throw "GUI test driver must contain only AMD64 PE binaries: $Path"
        }
    } finally {
        $Reader.Dispose()
    }
}

# A fresh staging directory prevents a previous install or partial download
# from supplying unverified files. All extracted paths are explicitly named.
$Stage = Join-Path ([IO.Path]::GetTempPath()) ("photopinfix-mesa-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Stage | Out-Null
try {
    $Archive = Join-Path $Stage $ArchiveName
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $Archive -TimeoutSec 300 -MaximumRetryCount 2
    if ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ArchiveSha256) {
        throw "Pinned Mesa archive SHA256 mismatch; no driver was installed"
    }
    $Extracted = Join-Path $Stage "x64"
    New-Item -ItemType Directory -Path $Extracted | Out-Null
    $Members = @($RequiredDlls | ForEach-Object { "x64/$_" })
    & $SevenZip e $Archive "-o$Extracted" -y @Members | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not extract the pinned Mesa x64 test DLLs" }

    $Files = [ordered]@{}
    $Imports = [ordered]@{}
    foreach ($Name in $RequiredDlls) {
        $File = Join-Path $Extracted $Name
        if (-not (Test-Path -LiteralPath $File -PathType Leaf)) { throw "Missing Mesa DLL: $Name" }
        $FileSha256 = (Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($FileSha256 -ne $ExpectedDllSha256[$Name]) { throw "Pinned Mesa DLL SHA256 mismatch: $Name" }
        Assert-X64Dll $File
        $DependencyOutput = & $Dumpbin.Source /DEPENDENTS $File 2>&1
        if ($LASTEXITCODE -ne 0) { throw "Cannot inspect Mesa dependencies: $Name" }
        $Dependencies = @($DependencyOutput | ForEach-Object {
            if ("$_" -match '^\s+([a-zA-Z0-9_.-]+\.dll)\s*$') { $Matches[1].ToLowerInvariant() }
        } | Sort-Object -Unique)
        if ($Dependencies.Count -eq 0) { throw "No PE dependencies found for $Name" }
        foreach ($Dependency in $Dependencies) {
            if ($RequiredDlls -contains $Dependency -or $Dependency -match '^(api-ms-win-|ext-ms-win-)') { continue }
            if (-not (Test-Path -LiteralPath (Join-Path $env:SystemRoot "System32/$Dependency") -PathType Leaf)) {
                throw "Mesa dependency is neither a staged DLL nor a Windows system library: $Name -> $Dependency"
            }
        }
        $Files[$Name] = $FileSha256
        $Imports[$Name] = $Dependencies
    }
    $Manifest = [ordered]@{
        schema_version = 1
        purpose = "ci-software-opengl"
        version = $Version
        architecture = "x86_64"
        driver = "llvmpipe"
        archive_sha256 = $ArchiveSha256
        release_url = $ReleaseUrl
        download_url = $DownloadUrl
        files = $Files
        imports = $Imports
    }
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    foreach ($Name in $RequiredDlls) {
        Copy-Item -LiteralPath (Join-Path $Extracted $Name) -Destination (Join-Path $Destination $Name) -Force
    }
    $Manifest | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8NoBOM (Join-Path $Destination "manifest.json")
    Write-Host "Prepared verified Mesa $Version x64 LLVMpipe for GUI tests only."
    Write-Host "Use capture_native_gui.py --windows-opengl-dir tools/gui-test/mesa-x64"
} finally {
    Remove-Item -LiteralPath $Stage -Recurse -Force
}
