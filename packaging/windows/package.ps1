<#
.SYNOPSIS
  Build, sign and package Cadino for Windows.

.DESCRIPTION
  Produces, in $env:DIST (default: dist\release):
    Cadino-<version>-windows-<arch>.msi            per-machine installer (WiX v5, cadcraft.wxs)
    Cadino-<version>-windows-<arch>-portable.zip   the same files under a top folder Cadino-<version>\
    Cadino-<version>-source.zip                    the source of this build (LGPL-2.1 section 6, below)
    SHA256SUMS.txt                                 sha256sum format (LF line endings), for all three
  Both hold Cadino.exe (the app; cargo builds it as cadcraft.exe), cadcraft-cli.exe (CLI and MCP
  server; it keeps its name and sits next to the app, where Options > AI looks for it),
  README-Cadino.txt, LICENSE-MIT, LICENSE-APACHE, NOTICE and THIRD-PARTY-NOTICES.txt
  (cargo xtask notices), and skills\cadino, the AI skill for Claude and Codex (in both the MSI
  and the zip).

  3D kernel: built with --features cadcraft-engine/occt. opencascade-sys compiles OpenCASCADE
  7.8.1 from source with CMake + MSBuild; the first build takes a long time. scripts\
  occt-msvc-setup.ps1 runs first (the opencascade-sys junction and the Handle_X header patch).

  C runtime: static (/MT) everywhere, so neither the MSI nor the zip needs the VC++
  redistributable. rustc gets +crt-static through CARGO_TARGET_<triple>_RUSTFLAGS (scoped to the
  target, so host build scripts and proc-macros are unaffected). cc, which compiles the cxx
  bridges (opencascade-sys, crates/kernel/occt, cxx), reads crt-static from
  CARGO_CFG_TARGET_FEATURE and passes /MT; cmake-rs hands those same flags (with /MT) to
  OpenCASCADE's CMake as CMAKE_<LANG>_FLAGS and CMAKE_<LANG>_FLAGS_RELEASE, and with
  CMAKE_POLICY_VERSION_MINIMUM=3.5 (policy CMP0091 OLD) the flags pick the runtime, so the
  static libraries come out MultiThreaded too. A mismatch would fail the link with LNK2038
  (RuntimeLibrary); the fallback is to drop +crt-static and ship vcruntime140.dll,
  vcruntime140_1.dll and msvcp140.dll from the VC redist folder next to the exes.

  OpenCASCADE optimisation: cmake-rs's CMAKE_CXX_FLAGS_RELEASE carries no /O flag and OCCT's
  Default profile adds none, so MSBuild would compile OCCT unoptimised (/Od). CL=/O2 (cl.exe
  reads it before its command line, so an explicit /Od or /O1 still wins) restores /O2.

  Source archive: the exact tree that was built (tracked files plus untracked, not ignored ones,
  through a temporary git index, so nothing is committed or staged), with the crates.io .crate
  archives of the LGPL crates (occt-sys, opencascade-sys, opencascade) in third-party/. Statically
  linked OCCT needs its source and the means to relink offered next to the binaries;
  THIRD-PARTY-NOTICES.txt points there. The tree must be clean (it is what the build stamp
  CADCRAFT_BUILD_SHA names); -AllowDirty builds a dirty tree anyway and stamps <sha>-dirty.

  Builder paths: rustc remaps CARGO_HOME (panic locations of dependencies) and cl.exe trims it
  from __FILE__ (OCCT's assertion messages), so the binaries don't carry the builder's home
  folder. CL is read only when OCCT compiles: after changing it, delete <target dir>\<triple>\OCCT
  so OpenCASCADE is rebuilt.

  Paths: MSBuild's FileTracker fails on paths over 260 characters, so the build uses a short
  CARGO_TARGET_DIR: $env:CARGO_TARGET_DIR, else <drive of the repo>:\ct. OpenCASCADE is built in
  <dir>\<triple>\OCCT.

  Signing: sign.ps1 (leaves the files unsigned with a warning when no signing secrets are set).

  Needs: Rust (MSVC toolchain + the target), Visual Studio's C++ tools and CMake (CMAKE, cmake on
  PATH, or Visual Studio's bundled CMake), the Windows SDK (rc.exe), and WiX v5 with its UI
  extension. WiX is installed here when missing (needs a .NET SDK for `dotnet tool install`):
    dotnet tool install --global wix --version 5.0.2
    wix extension add -g WixToolset.UI.wixext/5.0.2

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File packaging\windows\package.ps1
  powershell -ExecutionPolicy Bypass -File packaging\windows\package.ps1 -SkipBuild    # repackage existing binaries
  powershell -ExecutionPolicy Bypass -File packaging\windows\package.ps1 -AllowDirty   # uncommitted changes: stamped <sha>-dirty
#>
param(
  [ValidateSet('x64', 'x86', 'arm64')] [string] $Arch = 'x64',
  [switch] $SkipBuild,
  [switch] $AllowDirty
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

function Invoke-Native([string] $What, [scriptblock] $Block) {
  Write-Output "==> $What"
  & $Block
  if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

# The version lives in one place: [workspace.package] version in the root Cargo.toml.
$Version = $env:CADCRAFT_VERSION
if (-not $Version) {
  $inPkg = $false
  foreach ($line in Get-Content (Join-Path $Root 'Cargo.toml')) {
    if ($line -match '^\s*\[') { $inPkg = ($line.Trim() -eq '[workspace.package]'); continue }
    if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
  }
}
if (-not $Version) { throw 'could not read [workspace.package] version from Cargo.toml' }
# MSI ProductVersion is numeric (major.minor.build); pre-release tags are dropped there.
$MsiVersion = ($Version -split '-')[0]

$Target = switch ($Arch) { 'x64' { 'x86_64-pc-windows-msvc' } 'x86' { 'i686-pc-windows-msvc' } 'arm64' { 'aarch64-pc-windows-msvc' } }
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist\release' }
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path (Split-Path -Qualifier $Root) 'ct' }
$TargetDir = $env:CARGO_TARGET_DIR
$Icon = Join-Path $Root 'assets\app-icon\cadino.ico'
if (-not (Test-Path $Icon)) { throw "app icon missing: $Icon" }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

# The build stamp names a commit, so the tree must be that commit (or say it isn't).
$Dirty = [bool](git -C $Root status --porcelain)
if ($LASTEXITCODE -ne 0) { throw 'git status failed' }
if ($Dirty -and -not $AllowDirty) {
  throw 'the working tree has uncommitted or untracked changes: commit them, or pass -AllowDirty (the build is then stamped <sha>-dirty)'
}
if (-not $env:CADCRAFT_BUILD_SHA) { $env:CADCRAFT_BUILD_SHA = (git -C $Root rev-parse HEAD) + $(if ($Dirty) { '-dirty' } else { '' }) }
if (-not $env:CADCRAFT_BUILD_DATE) { $env:CADCRAFT_BUILD_DATE = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd') }

Write-Output "Cadino $Version for Windows $Arch ($Target), target dir $TargetDir"

# WiX v5 + UI extension, installed when missing (before the long build, so a missing .NET SDK
# fails fast).
$wixTools = Join-Path $env:USERPROFILE '.dotnet\tools'
if (($env:PATH -split ';') -notcontains $wixTools) { $env:PATH = "$env:PATH;$wixTools" }
if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
  Invoke-Native 'install WiX 5.0.2 (needs a .NET SDK)' { dotnet tool install --global wix --version 5.0.2 }
}
if (-not ((wix extension list -g) -match 'WixToolset\.UI\.wixext 5\.0\.2')) {
  Invoke-Native 'add the WiX UI extension' { wix extension add -g WixToolset.UI.wixext/5.0.2 }
}

if (-not $SkipBuild) {
  if (-not $env:CMAKE -and -not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    $vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath
    $env:CMAKE = Join-Path "$vs" 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
    if (-not (Test-Path $env:CMAKE)) { throw 'CMake not found: install it, put it on PATH or set CMAKE' }
  }
  # OpenCASCADE's CMakeLists predate CMake 3.5; CMake 4 refuses them without this.
  if (-not $env:CMAKE_POLICY_VERSION_MINIMUM) { $env:CMAKE_POLICY_VERSION_MINIMUM = '3.5' }
  # Keep the builder's home folder (CARGO_HOME, where the dependency and OCCT sources are) out
  # of the binaries. Both flags are split on spaces, hence the check.
  $CargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
  if ($CargoHome -match '\s') { throw "CARGO_HOME ($CargoHome) contains a space; move it or set CARGO_HOME to a path without one" }
  $env:CL = "/O2 /d1trimfile:$CargoHome\ $env:CL".Trim()
  & (Join-Path $Root 'scripts\occt-msvc-setup.ps1') -Target (Join-Path $TargetDir $Target)

  $flagVar = 'CARGO_TARGET_' + ($Target.ToUpper() -replace '-', '_') + '_RUSTFLAGS'
  [Environment]::SetEnvironmentVariable($flagVar, "-C target-feature=+crt-static --remap-path-prefix=$CargoHome=cargo-home")
  # Fail the build (rather than warn) if the icon/VERSIONINFO can't be embedded.
  $env:CADCRAFT_REQUIRE_WINRES = '1'
  # -j 6: more parallel links run out of memory.
  Invoke-Native "cargo build ($Target)" {
    cargo build --release --locked -j 6 -p cadcraft -p cadcraft-cli --features cadcraft-engine/occt --target $Target
  }
}

$Bin = Join-Path $TargetDir "$Target\release"

# Check both binaries' PE headers before packaging. Machine (COFF header) must match -Arch, so an
# x64 build can never ship labelled arm64. Subsystem (optional header): 2 = Windows GUI, 3 = console.
# The app must be GUI (no console window opens with it); the CLI must stay console so its output
# reaches the terminal.
function Get-PeHeader([string] $Path) {
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  $pe = [BitConverter]::ToInt32($bytes, 0x3C)
  return @{ Machine = [BitConverter]::ToUInt16($bytes, $pe + 4); Subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C) }
}
$Machine = switch ($Arch) { 'x64' { 0x8664 } 'x86' { 0x14C } 'arm64' { 0xAA64 } }
foreach ($check in @(@('cadcraft.exe', 2), @('cadcraft-cli.exe', 3))) {
  $h = Get-PeHeader (Join-Path $Bin $check[0])
  if ($h.Machine -ne $Machine) { throw "$($check[0]) is for machine 0x$('{0:X}' -f $h.Machine), expected 0x$('{0:X}' -f $Machine) ($Arch)" }
  if ($h.Subsystem -ne $check[1]) { throw "$($check[0]) has PE subsystem $($h.Subsystem), expected $($check[1])" }
  Write-Output "ok $($check[0]): $Arch, PE subsystem $($h.Subsystem)"
}

# ---- stage: the installed / portable folder ------------------------------------------------------
$Stage = Join-Path $TargetDir "windows-package\$Arch\Cadino-$Version"
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item (Join-Path $Bin 'cadcraft.exe') (Join-Path $Stage 'Cadino.exe')
Copy-Item (Join-Path $Bin 'cadcraft-cli.exe') $Stage
Copy-Item (Join-Path $PSScriptRoot 'README-Cadino.txt') $Stage
foreach ($f in 'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE') { Copy-Item (Join-Path $Root $f) $Stage }
# The Cadino skill for Claude and Codex (skills\cadino\README.md says how to install it).
Copy-Item -Recurse (Join-Path $Root 'skills') $Stage
$ThirdParty = Join-Path $TargetDir "windows-package\$Arch\third-party"
Remove-Item -Recurse -Force $ThirdParty -ErrorAction SilentlyContinue
Invoke-Native 'cargo xtask notices' { cargo xtask notices $Stage $ThirdParty }
if (-not (Test-Path (Join-Path $Stage 'THIRD-PARTY-NOTICES.txt'))) { throw 'cargo xtask notices wrote no THIRD-PARTY-NOTICES.txt' }

& (Join-Path $PSScriptRoot 'sign.ps1') (Join-Path $Stage 'Cadino.exe') (Join-Path $Stage 'cadcraft-cli.exe')

# ---- MSI ---------------------------------------------------------------------------------------
$Msi = Join-Path $Dist "Cadino-$Version-windows-$Arch.msi"
Invoke-Native 'wix build' {
  wix build (Join-Path $PSScriptRoot 'cadcraft.wxs') -arch $Arch -ext WixToolset.UI.wixext `
    -d "Version=$MsiVersion" -d "BinDir=$Stage" -d "IconPath=$Icon" -o $Msi
}
# wix writes its debug symbols (.wixpdb) next to the MSI; keep them out of the release assets.
Remove-Item -Force -ErrorAction SilentlyContinue ([IO.Path]::ChangeExtension($Msi, '.wixpdb'))
& (Join-Path $PSScriptRoot 'sign.ps1') $Msi

# ---- portable zip ------------------------------------------------------------------------------
# ZipFile with the stage folder itself as the top folder. Windows PowerShell 5.1's .NET Framework
# writes '\' into entry names (as does its Compress-Archive) unless this switch says otherwise.
$Zip = Join-Path $Dist "Cadino-$Version-windows-$Arch-portable.zip"
Remove-Item -Force $Zip -ErrorAction SilentlyContinue
[AppContext]::SetSwitch('Switch.System.IO.Compression.ZipFile.UseBackslash', $false)
Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory($Stage, $Zip, [IO.Compression.CompressionLevel]::Optimal, $true)

# ---- source archive ------------------------------------------------------------------------------
# The working tree as built, through a temporary index (the real index and HEAD are untouched).
$Src = Join-Path $Dist "Cadino-$Version-source.zip"
$SrcIndex = Join-Path $TargetDir "windows-package\$Arch\source.index"
Remove-Item -Force $Src, $SrcIndex -ErrorAction SilentlyContinue
$env:GIT_INDEX_FILE = $SrcIndex
try {
  Invoke-Native 'git read-tree (temporary index)' { git -C $Root read-tree HEAD }
  # safecrlf off: no "LF will be replaced by CRLF" line per file (the archive is unaffected).
  Invoke-Native 'git add (temporary index)' { git -C $Root -c core.safecrlf=false add -A }
  $Tree = git -C $Root write-tree
  if ($LASTEXITCODE -ne 0) { throw 'git write-tree failed' }
} finally {
  Remove-Item Env:GIT_INDEX_FILE
  Remove-Item -Force $SrcIndex -ErrorAction SilentlyContinue
}
$top = "Cadino-$Version-source/"
$extra = @("--prefix=${top}third-party/") + (Get-ChildItem $ThirdParty -File | ForEach-Object { "--add-file=$($_.FullName)" })
Invoke-Native 'git archive (source)' { git -C $Root archive --format=zip @extra "--prefix=$top" -o $Src $Tree }

# ---- checksums ---------------------------------------------------------------------------------
# LF line endings: GNU sha256sum -c reads a CR as part of the file name.
$Sums = Join-Path $Dist 'SHA256SUMS.txt'
$lines = Get-Item $Msi, $Zip, $Src | ForEach-Object { "$((Get-FileHash -Algorithm SHA256 $_.FullName).Hash.ToLowerInvariant())  $($_.Name)" }
[IO.File]::WriteAllText($Sums, (($lines -join "`n") + "`n"))

# Smoke-test the CLI when this machine can run it. An ARM64 build made on an x64 runner can't run here.
$HostArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
if ($Arch -ne 'arm64' -or $HostArch -eq 'arm64') {
  Invoke-Native 'cadcraft-cli --version' { & (Join-Path $Stage 'cadcraft-cli.exe') --version }
} else {
  Write-Output "skipping cadcraft-cli --version: an $Arch build doesn't run on this $HostArch machine"
}
Write-Output 'Artifacts:'
Get-Item $Msi, $Zip, $Src, $Sums | ForEach-Object { Write-Output ("  {0}  ({1:N0} bytes)" -f $_.FullName, $_.Length) }
