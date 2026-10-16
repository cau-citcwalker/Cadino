//! While the app denies file access or closing drawings ([`cadcraft_engine::limits`], the AI
//! assistant's requests), no command reads or writes a file or closes a drawing, whatever leads
//! there: a JSON path, a file name typed at a running command's prompt, a script, LISP.

use cadcraft_engine::Session;
use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::doc::{Block, Drawing};
use cadcraft_engine::limits;
use serde_json::json;

/// A drawing file in a test format: any bytes, read as a drawing with block PART.
fn read(_bytes: &[u8], _name: &str) -> Result<Drawing, String> {
    let mut d = Drawing::new_imperial();
    d.blocks.insert("PART".into(), std::sync::Arc::new(Block::new("PART")));
    Ok(d)
}

#[test]
fn denied_file_access_fails_every_way_in() {
    set_io(IoHooks { read, write: |_, _| Ok(b"drawing".to_vec()), plot: None });
    let dir = std::env::temp_dir().join(format!("cadcraft-file-policy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let there = dir.join("there.dxf").to_string_lossy().into_owned();
    std::fs::write(&there, "x").unwrap();
    let out = dir.join("out.dxf").to_string_lossy().into_owned();
    let mut s = Session::new();
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    {
        let _off = limits::deny_files("files are off");
        let e = s.execute("saveas", &json!({"path": out})).unwrap_err().to_string();
        assert!(e.contains("files are off"), "{e}");
        assert!(s.execute("insert", &json!({"name": "A", "file": there, "at": [0, 0]})).is_err());
        assert!(s.execute("open", &json!({"path": there})).is_err());
        // INSERT's block name answered with a file at its prompt.
        s.cmdline("-insert").unwrap();
        let _ = s.cmdline(&there);
        s.cancel();
        // LISP's file functions see nothing.
        let found = s.execute("lisp", &json!({"code": format!("(findfile \"{}\")", there.replace('\\', "/"))})).unwrap();
        assert_eq!(found["value"], "nil", "{found}");
        assert_eq!(limits::files_denied(), Some("files are off"));
    }
    assert!(s.doc().unwrap().block("PART").is_none(), "nothing was read");
    assert!(!std::path::Path::new(&out).exists(), "nothing was written");
    // Allowed again once the guard is gone.
    assert_eq!(limits::files_denied(), None);
    s.execute("insert", &json!({"name": "A", "file": there, "at": [0, 0]})).unwrap();
    assert!(s.doc().unwrap().block("A").is_some() || s.doc().unwrap().block("PART").is_some());
    s.execute("saveas", &json!({"path": out})).unwrap();
    assert!(std::path::Path::new(&out).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn denied_closing_keeps_every_drawing_open() {
    let mut s = Session::new();
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    {
        let _keep = limits::deny_closing("closing is the user's");
        let e = s.execute("closeall", &json!({})).unwrap_err().to_string();
        assert!(e.contains("closing is the user's"), "{e}");
        assert!(s.execute("close", &json!({})).is_err());
        let _ = s.script("closeall\n");
        let _ = s.execute("lisp", &json!({"code": "(command \"close\")"}));
        assert_eq!(s.docs.len(), 1, "the drawing stays open");
    }
    s.execute("closeall", &json!({})).unwrap();
    assert!(s.docs.is_empty());
}
