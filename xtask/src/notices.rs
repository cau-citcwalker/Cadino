//! `cargo xtask notices <out-dir>`: write `THIRD-PARTY-NOTICES.txt` for the Windows release.
//!
//! Covers Cadino's own licence and NOTICE (with the upstream CADCraft notices), Open CASCADE
//! Technology (LGPL-2.1 with the OCCT exception, source location, the source archive), the fonts,
//! and every crate in the normal-dependency graph of `cadcraft` + `cadcraft-cli` as built for
//! x86_64-pc-windows-msvc with `cadcraft-engine/occt`: name, version, licence, repository,
//! authors and the full licence texts. Texts come from each crate's own LICENSE/COPYING/NOTICE
//! files in the cargo registry; a crate that ships none gets the standard text of the licences
//! its expression names (`xtask/licenses/`). Identical texts are printed once.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

const TARGET: &str = "x86_64-pc-windows-msvc";
const FEATURES: &str = "cadcraft-engine/occt";
const ROOTS: [&str; 2] = ["cadcraft", "cadcraft-cli"];
const RULE: &str = "================================================================================";

/// Licence files a crate keeps outside its root: egui's bundled fonts.
const EXTRA_FILES: &[(&str, &[&str])] =
    &[("epaint_default_fonts", &["fonts/UFL.txt", "fonts/OFL.txt", "fonts/Hack-Regular.txt", "fonts/emoji-icon-font-mit-license.txt"])];

/// Standard licence texts for crates that ship no licence file. Add one when the generator asks.
fn standard(id: &str) -> Option<&'static str> {
    match id {
        "MIT" => Some(include_str!("../licenses/MIT.txt")),
        "Apache-2.0" => Some(include_str!("../../LICENSE-APACHE")),
        "BSL-1.0" => Some(include_str!("../licenses/BSL-1.0.txt")),
        "LGPL-2.1" | "LGPL-2.1-only" | "LGPL-2.1-or-later" => Some(include_str!("../licenses/LGPL-2.1.txt")),
        _ => None,
    }
}

struct Crate {
    name: String,
    version: String,
    license: String,
    repository: String,
    authors: String,
    /// (file label, normalized text)
    texts: Vec<(String, String)>,
    standard: bool,
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_string()
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map(|t| normalize(&t)).map_err(|e| format!("read {}: {e}", path.display()))
}

fn is_licence_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_ascii_lowercase();
    path.is_file() && ["license", "licence", "copying", "notice", "unlicense", "copyright", "authors"].iter().any(|k| name.starts_with(k))
}

/// The crate's own licence files: root LICENSE*/COPYING*/NOTICE*/COPYRIGHT*/AUTHORS*, a LICENSES/
/// directory, the manifest's `license-file`, [`EXTRA_FILES`], and for a crate inside a git
/// repository (`repo`, its checkout root) the root's files, which often hold the copyright.
fn own_licence_files(dir: &Path, name: &str, license_file: Option<&str>, repo: Option<&Path>) -> Vec<(String, String)> {
    let repo = repo.filter(|r| *r != dir);
    let licenses = dir.join("LICENSES");
    let mut files = BTreeSet::new();
    for sub in [dir.to_path_buf(), licenses.clone()].into_iter().chain(repo.map(Path::to_path_buf)) {
        for entry in std::fs::read_dir(&sub).into_iter().flatten().flatten() {
            if is_licence_file(&entry.path()) || (sub == licenses && entry.path().is_file()) {
                files.insert(entry.path());
            }
        }
    }
    files.extend(license_file.map(|f| dir.join(f)));
    files.extend(EXTRA_FILES.iter().filter(|(n, _)| *n == name).flat_map(|(_, fs)| fs.iter().map(|f| dir.join(f))));
    // Binary or unreadable files are skipped; the standard-text fallback covers a crate left with none.
    files
        .into_iter()
        .filter_map(|p| {
            // Relative labels only: an absolute path would print the builder's home folder.
            let label = match (p.strip_prefix(dir), repo.and_then(|r| p.strip_prefix(r).ok())) {
                (Ok(rel), _) => rel.to_string_lossy().into_owned(),
                (Err(_), Some(rel)) => format!("repository root/{}", rel.to_string_lossy()),
                (Err(_), None) => p.file_name()?.to_string_lossy().into_owned(),
            }
            .replace('\\', "/");
            read(&p).ok().filter(|t| !t.is_empty()).map(|t| (label, t))
        })
        .collect()
}

fn standard_texts(license: &str) -> Vec<(String, String)> {
    // ponytail: no SPDX parser; every embedded text the expression names is included (for
    // `A OR B` that is more than needed, which is harmless).
    license
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+')))
        .filter(|id| !id.is_empty() && !["OR", "AND", "WITH"].contains(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|id| standard(id).map(|t| (format!("{id} (standard text)"), normalize(t))))
        .collect()
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn metadata(root: &Path) -> Result<Value, String> {
    let out = crate::cargo()
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET, "--features", FEATURES])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: bad JSON: {e}"))
}

/// Registry/git packages reachable from [`ROOTS`] through normal dependencies, sorted by name.
fn release_crates(meta: &Value) -> Result<Vec<&Value>, String> {
    let packages: HashMap<&str, &Value> =
        meta["packages"].as_array().ok_or("cargo metadata: no packages")?.iter().map(|p| (str_field(p, "id"), p)).collect();
    let nodes: HashMap<&str, &Value> =
        meta["resolve"]["nodes"].as_array().ok_or("cargo metadata: no resolve graph")?.iter().map(|n| (str_field(n, "id"), n)).collect();
    let mut stack: Vec<&str> =
        packages.values().filter(|p| ROOTS.contains(&str_field(p, "name")) && p["source"].is_null()).map(|p| str_field(p, "id")).collect();
    if stack.len() != ROOTS.len() {
        return Err(format!("cargo metadata: expected the workspace packages {ROOTS:?}"));
    }
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for dep in nodes.get(id).and_then(|n| n["deps"].as_array()).into_iter().flatten() {
            let normal = dep["dep_kinds"].as_array().into_iter().flatten().any(|k| k["kind"].is_null());
            if normal {
                stack.push(str_field(dep, "pkg"));
            }
        }
    }
    let mut out: Vec<&Value> = seen.iter().filter_map(|id| packages.get(id).copied()).filter(|p| !p["source"].is_null()).collect();
    out.sort_by(|a, b| (str_field(a, "name"), str_field(a, "version")).cmp(&(str_field(b, "name"), str_field(b, "version"))));
    Ok(out)
}

fn describe(p: &Value) -> Result<Crate, String> {
    let (name, version, license) = (str_field(p, "name"), str_field(p, "version"), str_field(p, "license"));
    let dir = Path::new(str_field(p, "manifest_path")).parent().map(Path::to_path_buf).unwrap_or_default();
    // A git dependency's checkout root, which cargo marks with `.cargo-ok`.
    let repo = str_field(p, "source").starts_with("git+").then(|| dir.ancestors().find(|a| a.join(".cargo-ok").is_file())).flatten();
    let mut texts = own_licence_files(&dir, name, p["license_file"].as_str(), repo);
    let standard = texts.is_empty();
    if standard {
        texts = standard_texts(license);
    }
    if texts.is_empty() {
        return Err(format!(
            "{name} {version}: no licence file in {} and no embedded standard text for `{license}`; add one to xtask/licenses/",
            dir.display()
        ));
    }
    let repository = [str_field(p, "repository"), str_field(p, "homepage")].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
    let authors = p["authors"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", ");
    Ok(Crate {
        name: name.into(),
        version: version.into(),
        license: if license.is_empty() { "(see licence file)".into() } else { license.into() },
        repository: if repository.is_empty() { format!("https://crates.io/crates/{name}") } else { repository.into() },
        authors,
        texts,
        standard,
    })
}

fn section(out: &mut String, title: &str) {
    let _ = write!(out, "\n\n{RULE}\n{title}\n{RULE}\n\n");
}

fn file_block(out: &mut String, label: &str, text: &str) {
    let _ = write!(out, "----- {label} -----\n\n{text}\n\n");
}

/// The whole notices text for the workspace at `root`.
pub fn generate(root: &Path) -> Result<String, String> {
    if std::env::var_os("CRAFT_FONTS_DIR").is_some_and(|d| !d.is_empty()) {
        return Err("CRAFT_FONTS_DIR is set: section 3 says no craft-fonts are embedded; add their licences first".into());
    }
    let meta = metadata(root)?;
    let packages = meta["packages"].as_array().ok_or("cargo metadata: no packages")?;
    let version =
        packages.iter().find(|p| str_field(p, "name") == "cadcraft" && p["source"].is_null()).map(|p| str_field(p, "version")).unwrap_or("?");
    let deps = release_crates(&meta)?;
    if !deps.iter().any(|p| str_field(p, "name") == "opencascade-sys") {
        return Err(format!("opencascade-sys is not in the dependency graph: `{FEATURES}` no longer reaches cadcraft"));
    }
    // occt-sys builds OCCT from its bundled sources (a build dependency of opencascade-sys); the
    // resulting static libraries are linked into the executables.
    let occt = packages.iter().find(|p| str_field(p, "name") == "occt-sys").ok_or("occt-sys (the OCCT sources) not found in cargo metadata")?;
    let occt_dir = Path::new(str_field(occt, "manifest_path")).parent().map(|d| d.join("OCCT")).unwrap_or_default();
    let crates = deps.into_iter().map(describe).collect::<Result<Vec<_>, _>>()?;

    let mut out = String::new();
    let _ = write!(
        out,
        "Cadino {version}: licences and third-party notices\n\n\
         Cadino.exe and cadcraft-cli.exe contain Cadino's own code (based on CADCraft), Open CASCADE\n\
         Technology and the Rust crates listed below. Generated by `cargo xtask notices` from Cargo.lock\n\
         (packages {}, features {FEATURES}, target {TARGET}, normal dependencies).\n\n\
         Contents\n  1. Cadino and CADCraft\n  2. Open CASCADE Technology {}\n  3. Fonts\n  4. Rust crates ({})\n  5. Licence texts",
        ROOTS.join(" + "),
        str_field(occt, "version").split('+').next().unwrap_or_default(),
        crates.len()
    );

    section(&mut out, "1. Cadino and CADCraft");
    for f in ["NOTICE", "LICENSE-MIT", "LICENSE-APACHE"] {
        file_block(&mut out, f, &read(&root.join(f))?);
    }

    section(&mut out, &format!("2. Open CASCADE Technology {}", str_field(occt, "version")));
    let _ = write!(
        out,
        "This program makes use of and is based on facilities provided by the Open CASCADE Technology\n\
         software (https://dev.opencascade.org/). Open CASCADE Technology is Copyright (c) Open CASCADE\n\
         SAS and is licensed under the GNU Lesser General Public License version 2.1 with the Open\n\
         CASCADE exception; both texts follow.\n\n\
         It is compiled from source and linked statically into the executables, together with the\n\
         opencascade and opencascade-sys Rust bindings (LGPL-2.1, https://github.com/bschwind/opencascade-rs).\n\n\
         Source code used:\n\
         - Open CASCADE Technology {occt_v}: the occt-sys {occt_v} crate, https://crates.io/crates/occt-sys/{occt_v}\n\
         \x20 (OCCT's own sources: https://github.com/Open-Cascade-SAS/OCCT).\n\
         - One change is made before building on Windows (MSVC), by scripts/occt-msvc-setup.ps1 in the\n\
         \x20 Cadino sources: in src/Standard/Standard_Handle.hxx the MSVC-only Handle_X class definition\n\
         \x20 is switched off so Handle_X is the plain typedef that other compilers already get.\n\
         - The bindings: opencascade 0.3.0 and opencascade-sys 0.3.0 from crates.io.\n\n\
         Corresponding source (LGPL-2.1 section 6): Cadino-{version}-source.zip is published in the same\n\
         place as this build, next to the installer and the portable zip and listed in the same\n\
         SHA256SUMS.txt. It holds the complete source of this build of Cadino (with Cargo.lock and\n\
         scripts/occt-msvc-setup.ps1, which makes the change above) and, in third-party/, the unmodified\n\
         crates.io archives of occt-sys, opencascade-sys and opencascade, whose SHA-256 checksums\n\
         Cargo.lock pins. docs/release.md in it explains how to rebuild Cadino, and so how to relink it\n\
         with a modified Open CASCADE Technology or modified bindings. Whoever passes this build on\n\
         must pass that archive on with it or offer it from the same place.\n\n",
        occt_v = str_field(occt, "version")
    );
    file_block(&mut out, "OCCT_LGPL_EXCEPTION.txt", &read(&occt_dir.join("OCCT_LGPL_EXCEPTION.txt"))?);
    file_block(&mut out, "LICENSE_LGPL_21.txt", &read(&occt_dir.join("LICENSE_LGPL_21.txt"))?);

    section(&mut out, "3. Fonts");
    out.push_str(
        "This build embeds no font files of its own. It was built without the optional craft-fonts input\n\
         (CRAFT_FONTS_DIR). The drafting font \"CADCraft Stroke\" is defined in code (MIT OR Apache-2.0).\n\
         The interface and drawing text use the fonts installed on the computer (Segoe UI, Arial and the\n\
         CJK fonts Windows provides), which are not distributed with Cadino. egui's fallback fonts\n\
         (Ubuntu Light, Hack, Noto Emoji, emoji-icon-font) are embedded through the epaint_default_fonts\n\
         crate; their licences are listed with that crate in sections 4 and 5.",
    );

    // Distinct texts (ignoring whitespace layout) in first-use order; crates refer to them by number.
    let mut texts: Vec<(String, Vec<String>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut refs: Vec<Vec<String>> = Vec::new();
    for c in &crates {
        let mut r = Vec::new();
        for (label, text) in &c.texts {
            let i = *index.entry(text.split_whitespace().collect::<Vec<_>>().join(" ")).or_insert_with(|| {
                texts.push((text.clone(), Vec::new()));
                texts.len() - 1
            });
            if let Some(t) = texts.get_mut(i) {
                t.1.push(format!("{} {} ({label})", c.name, c.version));
            }
            r.push(format!("[{}] {label}", i + 1));
        }
        refs.push(r);
    }

    section(&mut out, &format!("4. Rust crates ({})", crates.len()));
    for (c, r) in crates.iter().zip(&refs) {
        let _ = writeln!(out, "{} {}\n  Licence: {}\n  Repository: {}", c.name, c.version, c.license, c.repository);
        if !c.authors.is_empty() {
            let _ = writeln!(out, "  Authors: {}", c.authors);
        }
        let note = if c.standard { " (the crate ships no licence file; standard text, copyright its authors)" } else { "" };
        let _ = writeln!(out, "  Licence text: {}{note}\n", r.join(", "));
    }

    section(&mut out, &format!("5. Licence texts ({})", texts.len()));
    for (i, (text, users)) in texts.iter().enumerate() {
        let _ = write!(out, "----- [{}] used by: {} -----\n\n{text}\n\n", i + 1, users.join(", "));
    }
    Ok(out)
}

/// The crates.io archives (`.crate`, as cargo downloaded them) of the LGPL crates in the build: the
/// bindings, and occt-sys, whose OCCT sources are compiled into the executables. They go into the
/// source archive section 2 describes.
fn lgpl_archives(root: &Path) -> Result<Vec<PathBuf>, String> {
    let meta = metadata(root)?;
    let mut packages = release_crates(&meta)?;
    packages.extend(meta["packages"].as_array().into_iter().flatten().filter(|p| str_field(p, "name") == "occt-sys"));
    let mut out = BTreeSet::new();
    for p in packages.into_iter().filter(|p| str_field(p, "license").contains("LGPL")) {
        let (name, version) = (str_field(p, "name"), str_field(p, "version"));
        // <registry>/src/<index>/<name>-<version>/Cargo.toml -> <registry>/cache/<index>/<name>-<version>.crate
        let index = Path::new(str_field(p, "manifest_path")).parent().and_then(Path::parent);
        let archive = index.and_then(|i| Some(i.parent()?.parent()?.join("cache").join(i.file_name()?).join(format!("{name}-{version}.crate"))));
        match archive.filter(|a| a.is_file()) {
            Some(a) => out.insert(a),
            None => return Err(format!("{name} {version}: no downloaded .crate archive (run `cargo fetch --locked`)")),
        };
    }
    if out.len() < 3 {
        return Err(format!("expected occt-sys, opencascade and opencascade-sys among the LGPL crates, found {out:?}"));
    }
    Ok(out.into_iter().collect())
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let (dir, sources) = match args {
        [dir] => (PathBuf::from(dir), None),
        [dir, sources] => (PathBuf::from(dir), Some(PathBuf::from(sources))),
        _ => return Err("usage: cargo xtask notices <out-dir> [<dir for the LGPL crates' .crate archives>]".into()),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let text = generate(root)?;
    let path = dir.join("THIRD-PARTY-NOTICES.txt");
    // CRLF so Notepad on older Windows shows the lines.
    std::fs::write(&path, text.replace('\n', "\r\n")).map_err(|e| format!("write {}: {e}", path.display()))?;
    println!("{}: {} bytes", path.display(), text.len());
    if let Some(sources) = sources {
        std::fs::create_dir_all(&sources).map_err(|e| format!("create {}: {e}", sources.display()))?;
        for a in lgpl_archives(root)? {
            let to = sources.join(a.file_name().unwrap_or_default());
            std::fs::copy(&a, &to).map_err(|e| format!("copy {} to {}: {e}", a.display(), to.display()))?;
            println!("{}", to.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn notices_cover_occt_egui_and_licence_texts() {
        let text = super::generate(&crate::root()).unwrap();
        for needle in [
            "Cadino",
            "Copyright (c) 2026 ArtCraft Team and the CADCraft contributors",
            "Open CASCADE exception",
            "GNU LESSER GENERAL PUBLIC LICENSE",
            "Corresponding source (LGPL-2.1 section 6): Cadino-",
            // craft-ui (a git dependency) keeps its copyright at the repository root.
            "\ncraft-ui 0.1.0\n",
            "repository root/LICENSE-MIT",
            "Copyright (c) 2026 ArtCraft Team and the craft-libs contributors",
            "\nopencascade-sys 0.3.0\n  Licence: LGPL-2.1",
            "\negui ",
            "\nepaint_default_fonts ",
            "UBUNTU FONT LICENCE",
            "Apache License",
            "Permission is hereby granted",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
        // No absolute paths (the builder's home folder) in the shipped text.
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
        assert!(home.len() < 4 || !text.contains(&home), "the notices name {home}");
        // Every listed crate points at at least one licence text.
        let crates = text.split("\n4. Rust crates").nth(1).and_then(|s| s.split("\n5. Licence texts").next()).unwrap();
        assert!(crates.matches("\n  Licence: ").count() > 100);
        assert_eq!(crates.matches("\n  Licence: ").count(), crates.matches("\n  Licence text: [").count());
    }

    #[test]
    fn the_source_archive_gets_the_kernel_crates() {
        let names: Vec<String> =
            super::lgpl_archives(&crate::root()).unwrap().iter().filter_map(|p| Some(p.file_name()?.to_str()?.to_string())).collect();
        for want in ["occt-sys-", "opencascade-sys-", "opencascade-0"] {
            assert!(names.iter().any(|n| n.starts_with(want) && n.ends_with(".crate")), "{want}: {names:?}");
        }
    }

    #[test]
    fn standard_texts_follow_the_expression() {
        let ids: Vec<String> = super::standard_texts("(MIT OR Apache-2.0) AND BSL-1.0").into_iter().map(|t| t.0).collect();
        assert_eq!(ids, ["Apache-2.0 (standard text)", "BSL-1.0 (standard text)", "MIT (standard text)"]);
        assert!(super::standard_texts("Unknown-1.0").is_empty());
    }
}
