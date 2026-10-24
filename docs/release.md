# Building the Cadino Windows release

`packaging/windows/package.ps1` builds the Windows x64 release: the app with the OpenCASCADE 3D
kernel, the installer, the portable zip, the source archive and their checksums. It runs in
Windows PowerShell 5.1 or PowerShell 7.

## What you need

- Rust with the MSVC toolchain and the `x86_64-pc-windows-msvc` target.
- Visual Studio (2022 or later) with the C++ desktop tools and a Windows SDK (`cl.exe`,
  `rc.exe`, MSBuild). The script finds Visual Studio's bundled CMake when `cmake` is not on
  `PATH`; `CMAKE` can also name it.
- Git (the source archive is made with `git archive`).
- WiX Toolset 5.0.2 with `WixToolset.UI.wixext` 5.0.2. The script installs both when missing,
  which needs a .NET SDK (`dotnet tool install`).
- About 10 GB of free disk and 16 GB of RAM. The build uses `-j 6`; more parallel links run out
  of memory.

## Build

Commit your changes first. The build stamp (`CADCRAFT_BUILD_SHA`, shown by `Cadino.exe
--version`) names the commit, so the script refuses a tree with uncommitted or untracked changes.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File packaging\windows\package.ps1
```

| Option | Effect |
|---|---|
| `-AllowDirty` | build a tree with uncommitted changes anyway; the stamp becomes `<sha>-dirty` and the source archive holds the tree as it is |
| `-SkipBuild` | re-package the binaries already built (no cargo build) |
| `CARGO_TARGET_DIR` | the cargo target folder; default `<drive of the repo>:\ct` (MSBuild fails on paths over 260 characters, so keep it short) |
| `DIST` | the output folder; default `dist\release` |

The first build compiles OpenCASCADE 7.8.1 from source (about 15 of the roughly 25 minutes).
Later builds reuse it from `<target dir>\x86_64-pc-windows-msvc\OCCT` and take a few minutes.
OpenCASCADE's compiler flags (`CL`, set by the script) are read only when it is compiled: after
changing them, delete that `OCCT` folder so it is rebuilt.

What the script does, in order: checks the tree, installs WiX if needed, runs
`scripts\occt-msvc-setup.ps1` (the opencascade-sys junction and the `Standard_Handle.hxx`
change), runs `cargo build --release --locked -p cadcraft -p cadcraft-cli --features
cadcraft-engine/occt`, checks both executables' PE headers, stages the files, writes
`THIRD-PARTY-NOTICES.txt` (`cargo xtask notices`), signs when signing secrets are set, and builds
the MSI, the zip, the source archive and `SHA256SUMS.txt`. Last it runs `cadcraft-cli --version`.

Build settings it applies:

- **Static C runtime** (`+crt-static`, `/MT` for every C++ part, OpenCASCADE included), so no
  Visual C++ redistributable is needed.
- **Optimised OpenCASCADE**: `CL=/O2`, because cmake-rs passes no optimisation flag.
- **No builder paths**: rustc remaps `CARGO_HOME` (`--remap-path-prefix`) and `cl.exe` trims it
  from `__FILE__` (`/d1trimfile`), so the binaries don't carry the builder's home folder.

## What comes out

In `dist\release`:

| File | Contents |
|---|---|
| `Cadino-<ver>-windows-x64.msi` | per-machine installer |
| `Cadino-<ver>-windows-x64-portable.zip` | the same files under a top folder `Cadino-<ver>\` |
| `Cadino-<ver>-source.zip` | the source of this build (see below) |
| `SHA256SUMS.txt` | SHA-256 of the three files, in `sha256sum` format with LF line endings |

The MSI and the zip both hold:

| File | What it is |
|---|---|
| `Cadino.exe` | the app (cargo builds it as `cadcraft.exe`; it is renamed when staged) |
| `cadcraft-cli.exe` | command line and MCP server; it must stay next to `Cadino.exe` |
| `README-Cadino.txt` | how to start it, AI and MCP, where the docs and the source are |
| `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE` | Cadino's licence and CADCraft's notices |
| `THIRD-PARTY-NOTICES.txt` | Open CASCADE Technology, the fonts and every bundled Rust crate, with their licence texts |

The installer puts these in `Program Files\Cadino`, adds a Start Menu shortcut "Cadino", an
optional desktop shortcut (`DESKTOPSHORTCUT=1` for silent installs), App Paths for `Cadino.exe`
and the `Cadino.Drawing` ProgID under "Open with" for `.dxf` and `.dwg`. Its UpgradeCode
(`63081016-8F52-4054-9985-719FA0964FC3` in `packaging/windows/cadcraft.wxs`) is Cadino's own:
keep it, or later versions stop upgrading earlier ones. It is not CADCraft's, so a Cadino install
never touches a CADCraft one.

`Cadino-<ver>-source.zip` holds the tree that was built (tracked files plus untracked ones that
are not ignored, through a temporary git index; nothing is committed) and, in `third-party/`,
the crates.io archives of the LGPL-2.1 crates the executables link: `occt-sys` (the OpenCASCADE
sources), `opencascade-sys` and `opencascade`. Cargo.lock pins their SHA-256 checksums.

## Publishing

Publish all four files together. OpenCASCADE and its bindings are LGPL-2.1 and are linked
statically, so their source and the means to relink must be offered next to the binaries:
`THIRD-PARTY-NOTICES.txt` points to `Cadino-<ver>-source.zip` in the same place. Anyone who
redistributes the MSI or the zip must pass the source archive on with it.

To relink with a modified OpenCASCADE: unpack `third-party/occt-sys-<ver>.crate` (a `.tar.gz`),
change it, add `[patch.crates-io] occt-sys = { path = "<unpacked folder>" }` to the root
`Cargo.toml`, run `cargo update -p occt-sys` so Cargo.lock records it (the build uses
`--locked`), delete `<target dir>\x86_64-pc-windows-msvc\OCCT` and run the script with
`-AllowDirty`.

## Unsigned builds and SmartScreen

`packaging/windows/sign.ps1` signs when `WINDOWS_CERTIFICATE` (a base64 `.pfx`) with
`WINDOWS_CERTIFICATE_PASSWORD`, or the Azure Trusted Signing variables, are set. Without them it
warns and leaves the files unsigned. Windows SmartScreen then shows "Windows protected your PC"
on the first download and run (More info ▸ Run anyway) until the files are signed or have built
up reputation. Tell users to check the checksums.

## Checking the checksums

PowerShell, in the folder with the downloads:

```powershell
Get-Content SHA256SUMS.txt
Get-FileHash -Algorithm SHA256 Cadino-*.msi, Cadino-*.zip | Format-Table Hash, Path
```

The hashes print in upper case in PowerShell and in lower case in `SHA256SUMS.txt`; they are
the same values. With Git Bash, WSL, Linux or macOS:

```sh
sha256sum -c SHA256SUMS.txt     # macOS: shasum -a 256 -c SHA256SUMS.txt
```
