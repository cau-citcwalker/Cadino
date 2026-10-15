fn main() {
    // OpenCASCADE's TKernel calls Win32 security and registry functions (advapi32), which
    // opencascade-sys doesn't link.
    let occt = std::env::var_os("CARGO_FEATURE_OCCT").is_some();
    if occt && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
