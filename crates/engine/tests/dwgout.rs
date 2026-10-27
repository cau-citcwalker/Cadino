//! DWGOUT (File ▸ Export ▸ AutoCAD DWG…): a separate DWG copy at a chosen version, 2018 by
//! default. The drawing keeps its own file name, format and unsaved changes.

use cadcraft_engine::Session;
use cadcraft_engine::cmd::file::{IoHooks, base64_decode, set_io, set_write_as};
use cadcraft_engine::limits;
use serde_json::json;

fn install() {
    set_io(IoHooks {
        read: |b, name| cadcraft_io::read(b, name).map_err(|e| e.to_string()),
        write: |d, name| cadcraft_io::write(d, name).map_err(|e| e.to_string()),
        plot: None,
    });
    set_write_as(|d, name, o| {
        cadcraft_io::write_with(d, name, &cadcraft_io::SaveOptions { version: o.version, binary: o.binary }).map_err(|e| e.to_string())
    });
}

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cadrhino-dwgout-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn magic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..6]).into_owned()
}

#[test]
fn dwgout_writes_a_separate_copy_and_leaves_the_drawing_alone() {
    install();
    let dir = dir("copy");
    let own = dir.join("plan.dxf").to_string_lossy().into_owned();
    let mut s = Session::new();
    s.execute("circle", &json!({ "center": [0, 0], "radius": 5 })).unwrap();
    s.execute("saveas", &json!({ "path": own })).unwrap();
    s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
    assert!(s.state().unwrap().is_dirty());

    let copy = dir.join("for-autocad.dwg").to_string_lossy().into_owned();
    let r = s.execute("dwgout", &json!({ "path": copy, "version": 2013 })).unwrap();
    assert_eq!((r["version"].as_str(), r["path"].as_str()), (Some("2013"), Some(copy.as_str())), "{r}");
    let bytes = std::fs::read(&copy).unwrap();
    assert_eq!(magic(&bytes), "AC1027");
    let back = cadcraft_io::read(&bytes, "x.dwg").unwrap();
    assert_eq!(back.model.len(), 2, "the copy holds the drawing as it is now");
    // The drawing is still plan.dxf, with its unsaved line.
    let st = s.state().unwrap();
    assert_eq!((st.path.as_deref(), st.title.as_str()), (Some(own.as_str()), "plan.dxf"));
    assert!(st.is_dirty(), "a copy is not a save");
    assert_eq!(st.drawing().header.get("ACADVER").and_then(cadcraft_doc::HVal::as_str), Some("AC1032"), "nor a version change");

    // No path: the bytes come back, 2018 by default; a name without extension gets .dwg.
    let r = s.execute("dwgout", &json!({})).unwrap();
    assert_eq!(magic(&base64_decode(r["data"].as_str().unwrap()).unwrap()), "AC1032");
    let bare = dir.join("bare").to_string_lossy().into_owned();
    s.execute("dwgout", &json!({ "path": bare, "version": "R2000" })).unwrap();
    assert_eq!(magic(&std::fs::read(format!("{bare}.dwg")).unwrap()), "AC1015");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dwgout_refuses_bad_parameters_and_denied_files() {
    install();
    let dir = dir("bad");
    let mut s = Session::new();
    s.execute("circle", &json!({ "center": [0, 0], "radius": 5 })).unwrap();
    let path = |n: &str| dir.join(n).to_string_lossy().into_owned();
    let err = |s: &mut Session, p: serde_json::Value| s.execute("dwgout", &p).unwrap_err().to_string();
    assert!(err(&mut s, json!({ "path": path("a.dwg"), "version": 2005 })).contains("2018"));
    assert!(err(&mut s, json!({ "path": path("a.dxf") })).contains(".dwg"));
    assert!(err(&mut s, json!({ "path": path("a.dwg"), "binary": true })).contains("DXF"));
    {
        let _off = limits::deny_files("files are off");
        assert!(err(&mut s, json!({ "path": path("a.dwg") })).contains("files are off"));
    }
    assert!(!dir.join("a.dwg").exists() && !dir.join("a.dxf").exists(), "nothing is written");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn typed_dwgout_asks_for_the_version_then_the_file() {
    install();
    let dir = dir("typed");
    let mut s = Session::new();
    s.execute("circle", &json!({ "center": [0, 0], "radius": 5 })).unwrap();
    s.cmdline("dwgout").unwrap();
    let p = s.current_prompt().expect("version prompt");
    assert_eq!((p.keywords.first().map(String::as_str), p.default.as_deref()), (Some("2018"), Some("2018")), "{}", s.prompt_text());
    s.cmdline("2010").unwrap();
    let p = s.current_prompt().expect("file prompt");
    assert!(matches!(p.picker, Some(cadcraft_engine::Picker::File { save: true, .. })), "{p:?}");
    let out = dir.join("typed.dwg").to_string_lossy().into_owned();
    s.cmdline(&format!("\"{out}\"")).unwrap();
    assert!(s.running.is_none(), "{}", s.prompt_text());
    assert_eq!(magic(&std::fs::read(&out).unwrap()), "AC1024");
    let _ = std::fs::remove_dir_all(&dir);
}
