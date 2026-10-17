//! Names typed bare at the command line: aliases reach their commands' dialogs and palettes,
//! AutoCAD's window names work, and file commands ask for their file. JSON calls never do.

use std::cell::RefCell;
use std::rc::Rc;

use cadcraft_engine::Session;
use cadcraft_ui_egui::docking::Panel;
use cadcraft_ui_egui::{CadApp, Services, exchange};
use serde_json::json;

/// An app whose file dialogs record what they were asked (the open dialog: the kind of file it
/// listed) and cancel.
fn app() -> (CadApp, Rc<RefCell<Vec<String>>>) {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let (a, b) = (asked.clone(), asked.clone());
    let services = Services {
        pick_open: Some(Box::new(move || {
            a.borrow_mut().push(format!("open {}", exchange::open_kind().map_or("drawing", |(label, _)| label)));
            None
        })),
        pick_save: Some(Box::new(move |name: &str| {
            b.borrow_mut().push(name.to_string());
            None
        })),
    };
    (CadApp::new(Session::new(), services), asked)
}

fn dialog(app: &mut CadApp) -> Option<String> {
    app.ui.dialog.take()
}

#[test]
fn aliases_reach_their_commands_dialogs_and_palettes() {
    let (mut app, asked) = app();
    for (typed, shown) in [("DDLMODES", "layers"), ("ddunits", "units"), ("DDLTYPE", "linetype"), ("DDRMODES", "dsettings"), ("DDOSNAP", "dsettings")]
    {
        assert!(app.cmdline(typed).is_none(), "{typed}");
        assert_eq!(dialog(&mut app).as_deref(), Some(shown), "{typed}");
    }
    let properties = |app: &CadApp| app.ui.docking.layout.contains(&Panel::Properties);
    for typed in ["MO", "DDCHPROP", "ddmodify"] {
        app.cmdline("PRCLOSE");
        assert!(!properties(&app), "PRCLOSE closes the Properties palette");
        app.cmdline(typed);
        assert!(properties(&app), "{typed} opens the Properties palette");
    }
    // The name typed comes first: QC and QUICKCALC (aliases of CAL) open the QuickCalc palette.
    app.cmdline("QC");
    assert!(app.ui.docking.layout.contains(&Panel::QuickCalc));
    assert!(app.session.running.is_none());
    // SAVE (an alias of QSAVE) on an unnamed drawing asks where to save it.
    app.cmdline("save");
    assert_eq!(*asked.borrow(), ["Drawing1.dxf"]);
    // Dash forms are the command-line versions: -LT starts -LINETYPE's prompts, no dialog.
    app.cmdline("-LT");
    assert!(dialog(&mut app).is_none());
    assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("-linetype"));
    app.session.cancel();
    // A menu pick or AutoComplete starting an alias goes the same way.
    app.start("ddlmodes");
    assert_eq!(dialog(&mut app).as_deref(), Some("layers"));
    // JSON calls run the command.
    app.cmdline(r#"mo {}"#);
    assert!(dialog(&mut app).is_none());
}

#[test]
fn autocads_window_and_palette_names() {
    let (mut app, _) = app();
    for (typed, shown) in [("HELP", "commands"), ("about", "about"), ("BLOCKSPALETTE", "blocks")] {
        app.cmdline(typed);
        assert_eq!(dialog(&mut app).as_deref(), Some(shown), "{typed}");
    }
    app.cmdline("POFF");
    assert!(!app.ui.show_palettes && !app.ui.show_toolsets);
    app.cmdline("PON");
    assert!(app.ui.show_palettes && app.ui.show_toolsets);
    app.cmdline("HIDEPALETTES");
    assert!(!app.ui.show_palettes);
    app.cmdline("SHOWPALETTES");
    assert!(app.ui.show_palettes);
    app.cmdline("COMMANDLINECLOSE");
    assert!(!app.ui.show_command_line);
    app.cmdline("COMMANDLINE");
    assert!(app.ui.show_command_line);
    app.cmdline("TEXTSCR");
    assert!(app.cmd.expanded);
    app.cmdline("GRAPHSCR");
    assert!(!app.cmd.expanded);
    app.cmdline("LA");
    assert_eq!(app.ui.dialog.as_deref(), Some("layers"));
    app.cmdline("LAYERCLOSE");
    assert!(app.ui.dialog.is_none());
    app.ui.show_toolsets = false;
    app.cmdline("TP");
    assert!(app.ui.show_toolsets);
    app.cmdline("TOOLPALETTES");
    assert!(app.ui.show_toolsets, "TOOLPALETTES opens Tool Sets, it doesn't toggle them");
    assert!(!app.session.log.iter().any(|l| l.contains("unknown command")), "{:?}", app.session.log);
}

#[test]
fn file_commands_ask_for_their_kind_of_file() {
    let (mut app, asked) = app();
    for typed in ["STEPIN", "stepout", "APPLOAD", "ap", "SCRIPT", "ALIASEDIT"] {
        app.cmdline(typed);
    }
    let expected = ["open STEP (STP, STEP)", "Drawing1.stp", "open LISP (LSP)", "open LISP (LSP)", "open Script (SCR)", "open PGP"];
    assert_eq!(*asked.borrow(), expected);
    assert!(exchange::open_kind().is_none(), "the hint ends with the dialog");
    // The menu items too (File > Import > STEP).
    app.start("stepin");
    assert_eq!(asked.borrow().len(), expected.len() + 1);
    assert!(app.session.running.is_none());

    // STLOUT with nothing selected asks for the objects first (typed and File > Export > STL);
    // its file prompt then asks with the file dialog.
    for start in [true, false] {
        if start {
            app.start("stlout");
        } else {
            app.cmdline("STLOUT");
        }
        assert_eq!(app.session.prompt_text(), "STLOUT Select solids or watertight meshes:");
        app.session.cancel();
    }

    // DATALINK links the selected table: without one it asks for the table, then for the CSV
    // file (the file dialog); with one it asks for the CSV at once.
    asked.borrow_mut().clear();
    let r = app.run("table", json!({ "at": [0, 0], "rows": 2, "cols": 2 })).unwrap();
    let h = r["handle"].as_str().unwrap().to_string();
    app.cmdline("DL");
    assert_eq!(app.session.prompt_text(), "DATALINK Select a table:");
    assert!(asked.borrow().is_empty());
    let handle = app.session.doc().unwrap().model.handles()[0];
    app.session.input(cadcraft_engine::Input::Pick(vec![handle])).unwrap();
    assert_eq!(app.session.prompt_text(), "DATALINK Enter name of CSV file to link:");
    exchange::file_prompt(&mut app);
    assert_eq!(*asked.borrow(), ["open CSV (CSV, TXT, TSV)"]);
    assert!(app.session.running.is_none(), "cancelling the file dialog cancels DATALINK");
    asked.borrow_mut().clear();
    app.run("select", json!({ "handles": [h] })).unwrap();
    app.cmdline("DATALINK");
    assert_eq!(*asked.borrow(), ["open CSV (CSV, TXT, TSV)"]);

    // JSON calls never open a file dialog.
    asked.borrow_mut().clear();
    for id in ["stepin", "stepout", "stlout", "datalink", "appload", "script"] {
        let _ = app.run(id, json!({}));
    }
    let _ = app.run("aliasedit", json!({}));
    assert!(asked.borrow().is_empty());
}

#[test]
fn without_a_file_dialog_appload_and_script_ask_on_the_command_line() {
    let mut app = CadApp::new(Session::new(), Services::default());
    app.cmdline("APPLOAD");
    assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("appload"));
    app.session.cancel();
    app.cmdline("SCRIPT");
    assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("script"));
    app.session.cancel();
    let e = app.cmdline("STEPIN").unwrap_or_default();
    assert!(e.is_empty(), "the UI answered: {e}");
    assert!(app.session.log.iter().any(|l| l.contains("STEPIN: there is no file dialog here")), "{:?}", app.session.log);
}

/// A file prompt asks with the file dialog once (a refused path leaves the prompt for typing),
/// enters the chosen path, and FILEDIA 0 keeps it on the command line.
#[test]
fn file_prompts_ask_with_the_file_dialog_once_unless_filedia_is_off() {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let a = asked.clone();
    let services = Services {
        pick_open: Some(Box::new(move || {
            a.borrow_mut().push(exchange::open_kind().map_or("drawing", |(label, _)| label).to_string());
            Some("no-such-dir/missing.stp".into())
        })),
        pick_save: None,
    };
    let mut app = CadApp::new(Session::new(), services);
    app.session.start("stepin").unwrap();
    exchange::file_prompt(&mut app);
    assert_eq!(*asked.borrow(), ["STEP (STP, STEP)"]);
    // The file can't be read: the prompt stays for typing and the dialog isn't shown again.
    assert_eq!(app.session.running.as_ref().map(|r| r.id.as_str()), Some("stepin"));
    exchange::file_prompt(&mut app);
    assert_eq!(asked.borrow().len(), 1);
    app.session.cancel();
    // FILEDIA 0: typed at the command line, whether STEPIN is typed or its prompt is reached.
    app.cmdline("filedia 0");
    assert!(!app.session.settings.filedia);
    for typed in ["STEPIN", "SCRIPT"] {
        app.cmdline(typed);
        exchange::file_prompt(&mut app);
        assert_eq!(asked.borrow().len(), 1, "{typed}");
        assert_eq!(app.session.running.as_ref().map(|r| r.id.to_ascii_uppercase()), Some(typed.to_string()));
        app.session.cancel();
    }
}

/// A command given its answers on the line (scripts, the control channel, the AI's command
/// line) runs its command-line form instead of opening its dialog and dropping them.
#[test]
fn answered_commands_run_their_command_line_form() {
    let (mut app, _) = app();
    app.cmdline("color red");
    assert_eq!(cadcraft_engine::sysvars::get(&app.session, "CECOLOR").and_then(|v| v.as_i64()), Some(1), "{:?}", app.session.log);
    assert!(app.ui.dialog.is_none());
    app.cmdline("lweight 0.30");
    assert!(app.ui.dialog.is_none());
    assert!(app.session.running.is_none(), "{}", app.session.prompt_text());
    // Typed alone, the dialog.
    app.cmdline("color");
    assert_eq!(app.ui.dialog.as_deref(), Some("color"));
    // UI names still take the line (AI opens the assistant whatever follows).
    app.ui.dialog = None;
    app.cmdline("PRCLOSE now");
    assert!(!app.session.log.iter().any(|l| l.contains("unknown command")), "{:?}", app.session.log);
}

/// The UCS icon shows while UCSICON is on: View > UCS Icon and the UCSICON command set the same
/// system variable.
#[test]
fn the_ucs_icon_menu_item_and_ucsicon_agree() {
    let (mut app, _) = app();
    app.run("ui.toggle.ucsicon", json!({ "on": false })).unwrap();
    assert_eq!(cadcraft_engine::sysvars::get(&app.session, "UCSICON"), Some(json!(0)));
    app.cmdline("UCSICON");
    app.cmdline("ON");
    assert!(app.session.settings.ucsicon);
}
