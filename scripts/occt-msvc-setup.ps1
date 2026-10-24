# Prepare OpenCASCADE (opencascade-sys 0.3 / occt-sys 7.8.1) for MSVC builds. Run once per
# machine before `cargo build --features occt`, and again after the OCCT install in
# target/OCCT is rebuilt.
#  1. cxx-build includes "opencascade-sys/include/..." through the parent of the crate
#     directory, which needs a directory named `opencascade-sys` (symlinks need Developer
#     Mode, so a junction stands in).
#  2. On MSVC, OCCT defines Handle_X as a class derived from opencascade::handle<X> (for
#     C++/CLI); the bindings need the plain typedef that other compilers get. The class has
#     no members, so the layout is the same.
param([string]$Target = (Join-Path $PSScriptRoot '..\target'))
$ErrorActionPreference = 'Stop'
$registry = Get-ChildItem "$env:USERPROFILE\.cargo\registry\src" -Directory | Select-Object -First 1
$sys = Join-Path $registry.FullName 'opencascade-sys-0.3.0'
$link = Join-Path $registry.FullName 'opencascade-sys'
if (-not (Test-Path $link)) { cmd /c mklink /J "$link" "$sys" | Out-Null }

$from = '#if (defined(_MSC_VER) && _MSC_VER >= 1800) '
$to = '#if 0 // CAD-rhino (Cadino), changed 2026-10-15: typedef Handle_X on MSVC too (was: defined(_MSC_VER) && _MSC_VER >= 1800)'
$headers = @(
  (Join-Path $registry.FullName 'occt-sys-7.8.1\OCCT\src\Standard\Standard_Handle.hxx'),
  (Join-Path $Target 'OCCT\include\Standard_Handle.hxx')
)
foreach ($h in $headers) {
  if (Test-Path $h) {
    $t = [IO.File]::ReadAllText($h)
    if ($t.Contains($from)) { [IO.File]::WriteAllText($h, $t.Replace($from, $to)); "patched $h" } else { "ok $h" }
  }
}
