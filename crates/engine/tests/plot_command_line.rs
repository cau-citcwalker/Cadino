//! The command-line forms of the plotting and parametric dialogs: -PLOT, -PAGESETUP, -PLOTSTYLE,
//! -EXPORT, -PUBLISH, -PARAMETERS (-PAR) and -CONSTRAINTSETTINGS ask AutoCAD's questions in its
//! order, typed one answer at a time, on one line, or from a script, and end in the JSON forms.

use cadcraft_doc::{Drawing, Space};
use cadcraft_engine::Session;
use cadcraft_engine::cmd::file::{IoHooks, set_io};
use cadcraft_engine::cmd::plotters::{PlotterStore, set_plotter_store};
use serde_json::Value;

/// The plot hook writes the options it gets; the writer writes the file name it was asked for.
fn echo_plot(_: &Drawing, _: &Space, opts: &Value) -> Result<Vec<u8>, String> {
    Ok(opts.to_string().into_bytes())
}

fn session() -> Session {
    set_plotter_store(PlotterStore::memory());
    set_io(IoHooks { read: |_, _| Err("no reader in this test".into()), write: |_, name| Ok(name.as_bytes().to_vec()), plot: Some(echo_plot) });
    let mut s = Session::new();
    s.cmdline("line 0,0 100,0").unwrap();
    s.cmdline("").unwrap();
    s
}

fn temp(name: &str) -> String {
    std::env::temp_dir().join(format!("cadcraft-dashplot-{}-{name}", std::process::id())).to_string_lossy().into_owned()
}

/// The options a plot file holds (the echo hook's output).
fn plotted(path: &str) -> Value {
    let v = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let _ = std::fs::remove_file(path);
    v
}

fn logged(s: &Session, text: &str) -> bool {
    s.log.iter().any(|l| l.contains(text))
}

fn prompt(s: &Session) -> String {
    s.prompt_text()
}

#[test]
fn dash_plot_detailed_on_one_line() {
    let mut s = session();
    let path = temp("model.pdf");
    s.cmdline(&format!("-plot y model None A4 m landscape n window 0,0 100,50 1:2 center n y a {path} n y")).unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    assert!(logged(&s, &format!("Plotted to {path}.")), "{:?}", s.log);
    let o = plotted(&path);
    assert_eq!(o["layout"], "Model");
    assert_eq!(o["paper"], "ISO A4 (210.00 x 297.00 MM)");
    assert_eq!(o["landscape"], true);
    assert_eq!(o["plotArea"], "window");
    assert_eq!(o["window"], serde_json::json!([[0.0, 0.0], [100.0, 50.0]]));
    assert_eq!((o["fit"].as_bool(), o["scale"].as_f64()), (Some(false), Some(0.5)));
    assert_eq!(o["center"], true);
    assert_eq!(o["lineweights"], true);
    assert!(o.get("plotStyleTable").is_none());
}

#[test]
fn dash_plot_in_inches_with_an_offset() {
    let mut s = session();
    let path = temp("inches.pdf");
    // Scale and offset are typed in the paper units: 1=2 inches is 12.7 mm per unit.
    s.cmdline(&format!("-plot y model None letter i portrait n extents 1=2 1,0.5 y monochrome.ctb n a {path} n y")).unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    let o = plotted(&path);
    assert_eq!(o["landscape"], false);
    assert_eq!(o["plotArea"], "extents");
    assert!((o["scale"].as_f64().unwrap() - 12.7).abs() < 1e-9, "{o}");
    assert_eq!((o["center"].as_bool(), o["offset"].clone()), (Some(false), serde_json::json!([25.4, 12.7])));
    assert_eq!(o["plotStyleTable"], "monochrome.ctb");
    assert_eq!(o["lineweights"], false);
}

#[test]
fn dash_plot_lists_and_refuses_then_asks_again() {
    let mut s = session();
    s.cmdline("-plot").unwrap();
    assert!(prompt(&s).contains("Detailed plot configuration? [Yes/No] <No>"), "{}", prompt(&s));
    s.cmdline("y").unwrap();
    s.cmdline("?").unwrap();
    assert!(logged(&s, "Layout1"));
    s.cmdline("Nope").unwrap();
    assert!(logged(&s, "Layout \"Nope\" not found."));
    s.cmdline("model").unwrap();
    s.cmdline("?").unwrap();
    assert!(logged(&s, "CADCraft PDF"));
    s.cmdline("No Such Plotter").unwrap();
    assert!(prompt(&s).contains("Enter an output device name or [?]"), "{}", prompt(&s));
    s.cmdline("CADCraft SVG").unwrap();
    s.cmdline("Z12").unwrap();
    assert!(logged(&s, "has no paper `Z12`"), "{:?}", s.log);
    assert!(prompt(&s).contains("Enter paper size or [?]"));
    s.cancel();
    assert!(s.running.is_none());
}

#[test]
fn dash_plot_brief_on_a_layout_saves_the_page_setup() {
    let mut s = session();
    let base = temp("sheet");
    for line in ["-plot", "n", "Layout1", "", "CADCraft PNG", base.as_str(), "y", "y"] {
        s.cmdline(line).unwrap();
    }
    assert!(s.running.is_none(), "{}", prompt(&s));
    // The file name took the device's extension.
    let o = plotted(&format!("{base}.png"));
    assert_eq!((o["layout"].as_str(), o["output"].as_str()), (Some("Layout1"), Some("png")));
    assert!(o.get("plotArea").is_none(), "a layout plots its sheet");
    assert_eq!(s.doc().unwrap().layout("Layout1").unwrap().page.device, "CADCraft PNG");
    // The saved page setup is its own undo step: U takes back just that, not the LINE before.
    s.cmdline("u").unwrap();
    assert_ne!(s.doc().unwrap().layout("Layout1").unwrap().page.device, "CADCraft PNG");
    assert_eq!(s.doc().unwrap().model.len(), 1);
}

#[test]
fn dash_plot_from_a_script() {
    let mut s = session();
    let path = temp("script.svg");
    // Names with spaces go on lines of their own, as in AutoCAD scripts.
    let script = format!("-PLOT\nY\nModel\nCADCraft SVG\nISO A3 (297.00 x 420.00 MM)\nM\nL\nN\nE\nF\nC\nN\nY\nA\n{path}\nN\nY\n");
    s.script(&script).unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    let o = plotted(&path);
    assert_eq!((o["output"].as_str(), o["fit"].as_bool()), (Some("svg"), Some(true)));
    assert!(o.get("offset").is_none() && o.get("center").is_none(), "no offsets in SVG: {o}");
}

#[test]
fn dash_plot_respects_denied_files() {
    let mut s = session();
    let path = temp("denied.pdf");
    let guard = cadcraft_engine::limits::deny_files("files are off for this test");
    s.cmdline(&format!("-plot n model  None {path} n y")).unwrap();
    drop(guard);
    assert!(s.running.is_none(), "{}", prompt(&s));
    assert!(logged(&s, "files are off for this test"), "{:?}", s.log);
    assert!(!std::path::Path::new(&path).exists());
}

#[test]
fn dash_pagesetup_sets_the_layout_and_undoes() {
    let mut s = session();
    s.cmdline("-pagesetup").unwrap();
    assert!(s.running.is_none());
    assert!(logged(&s, "Model space page setups are not stored yet"));
    s.cmdline("layout set Layout1").unwrap();
    s.cmdline("-pagesetup None A3 m portrait n extents").unwrap();
    // A layout plots its whole sheet: only Layout is taken.
    assert!(prompt(&s).contains("Enter plot area"), "{}", prompt(&s));
    s.cmdline("layout 1:1 center y . n n n n").unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    let page = s.doc().unwrap().layout("Layout1").unwrap().page.clone();
    assert!(page.paper.starts_with("ISO A3"), "{}", page.paper);
    assert!(!page.landscape && !page.lineweights && page.center && page.plot_style_table.is_empty());
    assert_eq!(page.plot_area, "layout");
    s.undo().unwrap();
    assert!(s.doc().unwrap().layout("Layout1").unwrap().page.paper != page.paper);
}

#[test]
fn dash_plotstyle_lists_and_sets_the_current_style() {
    let mut s = session();
    s.cmdline("-plotstyle ?").unwrap();
    assert!(logged(&s, "Current plot style is \"ByLayer\""));
    assert!(logged(&s, "Normal") && logged(&s, "Thick"), "{:?}", s.log);
    assert!(prompt(&s).contains("Enter an option [?/Current] <Current>"), "{}", prompt(&s));
    s.cancel();
    s.cmdline("-plotstyle c byblock").unwrap();
    assert_eq!(s.doc().unwrap().header.str("CPLOTSTYLE", ""), "ByBlock");
    // The name must be one of the named tables' styles, and is kept as written there.
    s.cmdline("-plotstyle c NoSuchStyle").unwrap();
    assert!(logged(&s, "Plot style \"NoSuchStyle\" not found."));
    s.cmdline("screened 50%").unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    assert_eq!(s.doc().unwrap().header.str("CPLOTSTYLE", ""), "Screened 50%");
}

#[test]
fn dash_export_asks_format_then_file() {
    let mut s = session();
    let base = temp("export");
    s.cmdline(&format!("-export dxf {base}")).unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    let dxf = format!("{base}.dxf");
    assert_eq!(std::fs::read_to_string(&dxf).unwrap(), "export.dxf");
    assert!(logged(&s, &format!("Exported to {dxf}")));
    // A file name with a format's extension answers both questions.
    let svg = format!("{base}.svg");
    s.cmdline(&format!("-export {svg}")).unwrap();
    assert_eq!(std::fs::read_to_string(&svg).unwrap(), "export.svg");
    s.cmdline("-export").unwrap();
    assert!(prompt(&s).contains("Enter file format or [DXF/DWG/SVG/PNG/PDF] <PDF>"), "{}", prompt(&s));
    s.cmdline("txt").unwrap();
    assert!(s.running.is_some(), "an unknown format asks again");
    s.cancel();
    for p in [dxf, svg] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn dash_publish_from_a_sheet_list_or_the_current_sheets() {
    let mut s = session();
    let list = temp("sheets.txt");
    std::fs::write(&list, "; sheets\nLayout2\n\nModel\n").unwrap();
    let out = temp("set.pdf");
    s.cmdline(&format!("-publish {list} {out}")).unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    assert!(logged(&s, "Published 2 sheet(s)"), "{:?}", s.log);
    for sheet in ["Layout2", "Model"] {
        assert_eq!(plotted(&temp(&format!("set-{sheet}.pdf")))["layout"], sheet);
    }
    s.cmdline(&format!("-publish c {out}")).unwrap();
    assert!(logged(&s, "Published 3 sheet(s)"), "{:?}", s.log);
    for sheet in ["Model", "Layout1", "Layout2"] {
        let _ = std::fs::remove_file(temp(&format!("set-{sheet}.pdf")));
    }
    // A sheet list that isn't there asks again.
    s.cmdline(&format!("-publish {}", temp("missing.txt"))).unwrap();
    assert!(prompt(&s).contains("Enter name of sheet list file"), "{}", prompt(&s));
    s.cancel();
    let _ = std::fs::remove_file(list);
}

#[test]
fn dash_parameters_new_edit_rename_delete_list() {
    let mut s = session();
    s.cmdline("-parameters n width 100").unwrap();
    s.cmdline("-par n height width*2").unwrap();
    s.cmdline("-par e width 50").unwrap();
    s.cmdline("-par r width w").unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    s.cmdline("-par ? *").unwrap();
    assert!(logged(&s, "w = 50 = 50") && logged(&s, "height = w*2 = 100"), "{:?}", s.log);
    s.cmdline("-par n w").unwrap();
    assert!(logged(&s, "Parameter \"w\" already exists."));
    s.cancel();
    // Referenced parameters stay until what uses them goes.
    s.cmdline("-par d w").unwrap();
    assert!(logged(&s, "`w` is used by `height`"), "{:?}", s.log);
    s.cancel();
    s.cmdline("-par d height").unwrap();
    let names: Vec<String> = s.doc().unwrap().parametric.parameters.iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, ["w"]);
}

#[test]
fn parameters_rename_rewrites_whole_names_only() {
    let mut s = session();
    let set = |s: &mut Session, p: Value| s.execute("parameters", &p);
    set(&mut s, serde_json::json!({"name": "e5", "expr": "2"})).unwrap();
    set(&mut s, serde_json::json!({"name": "e55", "expr": "3"})).unwrap();
    set(&mut s, serde_json::json!({"name": "z", "expr": "1e5+e5*e55"})).unwrap();
    set(&mut s, serde_json::json!({"rename": "e5", "to": "k"})).unwrap();
    let z = s.doc().unwrap().parametric.parameters.iter().find(|p| p.name == "z").unwrap().expr.clone();
    assert_eq!(z, "1e5+k*e55");
    for bad in [
        serde_json::json!({"rename": "nope", "to": "a"}),
        serde_json::json!({"rename": "k", "to": "1x"}),
        serde_json::json!({"rename": "k", "to": "e55"}),
        serde_json::json!({"rename": "k"}),
    ] {
        assert!(set(&mut s, bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn dash_constraintsettings_sets_display_options() {
    let mut s = session();
    s.cmdline("-constraintsettings g y n 95").unwrap();
    assert!(logged(&s, "Requires a number between 0 and 90."), "{:?}", s.log);
    s.cmdline("30").unwrap();
    s.cmdline("d n a 0.5 10 x").unwrap();
    assert!(s.running.is_none(), "{}", prompt(&s));
    let set = s.doc().unwrap().parametric.settings.clone();
    assert!(set.infer && !set.bars_visible && !set.dims_visible);
    assert_eq!((set.bar_transparency, set.distance_tolerance, set.angle_tolerance), (30, 0.5, 10.0));
}

#[test]
fn json_forms_still_run_without_prompts() {
    let mut s = session();
    for (c, p) in [("plot", serde_json::json!({})), ("export", serde_json::json!({"format": "dxf"})), ("parameters", serde_json::json!({}))] {
        s.execute(c, &p).unwrap();
        assert!(s.running.is_none(), "{c}");
    }
}
