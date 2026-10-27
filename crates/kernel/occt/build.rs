//! Compiles our C++ (`src/ops.cpp`) against the OpenCASCADE opencascade-sys builds (into
//! `occt_sys::occt_path()`), and links the libraries of it opencascade-sys doesn't (TKHLR, and
//! TKCDF and TKService under the XDE documents STEP and IGES export write from).

use std::time::{Duration, Instant};

fn main() {
    for f in ["src/lib.rs", "src/ops.cpp", "include/cadrhino_occt.hxx"] {
        println!("cargo:rerun-if-changed={f}");
    }
    if std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "wasm32") {
        return;
    }
    let occt = occt_sys::occt_path();
    // opencascade-sys's build script builds OpenCASCADE, and nothing orders it before this one (it
    // declares no `links` key), so on a clean build this waits for its install to finish.
    // ponytail: polls the manifest CMake writes when the install is done; a `links` key upstream
    // would order the two build scripts instead.
    let installed = occt.join("build").join("install_manifest.txt");
    let start = Instant::now();
    while !installed.exists() {
        if start.elapsed() > Duration::from_secs(3 * 3600) {
            println!("cargo:warning=OpenCASCADE is not built at {}", occt.display());
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    println!("cargo:rustc-link-search=native={}", occt.join("lib").display());
    // Hidden-line removal (FLATSHOT); the other toolkits come with opencascade-sys.
    println!("cargo:rustc-link-lib=static=TKHLR");
    // The documents behind named, coloured STEP and IGES export (XDE).
    println!("cargo:rustc-link-lib=static=TKCDF");
    // Graphic3d, which XCAFDoc_VisMaterial (behind every XDE document) refers to.
    println!("cargo:rustc-link-lib=static=TKService");
    cxx_build::bridge("src/lib.rs")
        .file("src/ops.cpp")
        .include("include")
        .include(occt.join("include"))
        .define("_USE_MATH_DEFINES", "TRUE")
        .flag_if_supported("-std=c++17")
        .flag_if_supported("/std:c++17")
        .flag_if_supported("/bigobj")
        .warnings(false)
        .compile("cadrhino-occt");
}
