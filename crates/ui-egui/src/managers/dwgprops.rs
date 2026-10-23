//! Drawing Properties (DWGPROPS; File ▸ Drawing Properties...): General (the file's name, type,
//! folder, size and dates, read-only), Summary (title, subject, author, keywords, comments,
//! hyperlink base), Statistics (last saved by, revision number) and Custom (name and value
//! pairs). It reads and writes through the `dwgprops` command (no parameters reads); OK sends
//! what changed.

use egui::{RichText, vec2};
use serde_json::{Map, Value, json};

use super::{Action, no_drawing, set_state, state};
use crate::CadApp;
use crate::theme::Tokens;

/// The summary fields: the command's key and the label.
const SUMMARY: &[(&str, &str)] = &[
    ("title", "Title:"),
    ("subject", "Subject:"),
    ("author", "Author:"),
    ("keywords", "Keywords:"),
    ("comments", "Comments:"),
    ("hyperlinkBase", "Hyperlink base:"),
];
const STATISTICS: &[(&str, &str)] = &[("lastSavedBy", "Last saved by:"), ("revisionNumber", "Revision number:")];
/// Custom properties the drawing keeps.
const MAX_CUSTOM: usize = 10;

#[derive(Clone, Copy, Default, PartialEq)]
enum Tab {
    #[default]
    General,
    Summary,
    Statistics,
    Custom,
}

#[derive(Clone, Default)]
struct St {
    tab: Tab,
    /// The properties as read, and as edited.
    read: Value,
    fields: Map<String, Value>,
    custom: Vec<(String, String)>,
    sel: Option<usize>,
    /// The General tab's rows, read once (the file's size and dates).
    general: Vec<(&'static str, String)>,
}

fn fresh(app: &mut CadApp) -> St {
    let read = app.session.execute("dwgprops", &json!({})).unwrap_or_default();
    let fields = read.as_object().cloned().unwrap_or_default();
    let custom = read["custom"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect();
    St { read, fields, custom, general: general(app), ..St::default() }
}

/// Two custom properties with one name (any case): OK waits until they differ.
fn duplicate(custom: &[(String, String)]) -> bool {
    let mut seen = std::collections::HashSet::new();
    custom.iter().map(|(n, _)| n.trim().to_lowercase()).filter(|n| !n.is_empty()).any(|n| !seen.insert(n))
}

/// The first "Property n" name no custom property has.
fn new_name(custom: &[(String, String)]) -> String {
    (1..=custom.len() + 1)
        .map(|i| format!("Property {i}"))
        .find(|n| !custom.iter().any(|(c, _)| c.trim().eq_ignore_ascii_case(n)))
        .unwrap_or_default()
}

/// The `dwgprops` parameters for what `st` changed (`None`: nothing).
fn changes(st: &St) -> Option<Value> {
    let mut p = Map::new();
    for (k, _) in SUMMARY.iter().chain(STATISTICS) {
        let now = st.fields.get(*k).and_then(Value::as_str).unwrap_or_default().replace(['\n', '\r'], " ");
        if st.read.get(*k).and_then(Value::as_str).unwrap_or_default() != now {
            p.insert((*k).into(), json!(now));
        }
    }
    let custom: Map<String, Value> = st.custom.iter().filter(|(n, _)| !n.trim().is_empty()).map(|(n, v)| (n.trim().to_string(), json!(v))).collect();
    if st.read.get("custom").and_then(Value::as_object) != Some(&custom) {
        p.insert("custom".into(), Value::Object(custom));
    }
    (!p.is_empty()).then_some(Value::Object(p))
}

/// The General tab's rows: (label, value).
fn general(app: &CadApp) -> Vec<(&'static str, String)> {
    let Ok(st) = app.session.state() else { return Vec::new() };
    let path = st.path.as_deref().map(std::path::Path::new);
    let kind = match path.and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("dwg") => crate::tl!("Drawing (DWG)"),
        Some("dwt") => crate::tl!("Drawing template"),
        Some(_) => crate::tl!("Drawing (DXF)"),
        None => crate::tl!("Not saved yet"),
    };
    let mut rows = vec![("Name:", st.title.clone()), ("Type:", kind.to_string())];
    if let Some(p) = path {
        rows.push(("Location:", p.parent().map(|d| d.display().to_string()).unwrap_or_default()));
        if let Ok(m) = std::fs::metadata(p) {
            rows.push(("Size:", format!("{} KB ({} bytes)", m.len().div_ceil(1024), m.len())));
            let secs = |t: std::io::Result<std::time::SystemTime>| {
                t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| format!("{} UTC", crate::autosave::utc(d.as_secs())))
            };
            if let Some(c) = secs(m.created()) {
                rows.push(("Created:", c));
            }
            if let Some(c) = secs(m.modified()) {
                rows.push(("Modified:", c));
            }
        }
    }
    rows
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    if app.session.state().is_err() {
        no_drawing(ctx, "Drawing Properties", open);
        return None;
    }
    let mut st = state(ctx, "dwgprops_dlg", || fresh(app));
    let (mut ok, mut cancel) = (false, false);
    let title = crate::tf!("{name} Properties", name = app.session.state().map(|s| s.title.clone()).unwrap_or_default());
    egui::Window::new(title)
        .id(egui::Id::new("Drawing Properties"))
        .open(open)
        .default_size(vec2(440.0, 360.0))
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (Tab::General, crate::tl!("General")),
                    (Tab::Summary, crate::tl!("Summary")),
                    (Tab::Statistics, crate::tl!("Statistics")),
                    (Tab::Custom, crate::tl!("Custom")),
                ] {
                    ui.selectable_value(&mut st.tab, tab, label);
                }
            });
            ui.separator();
            // The tabs share one size, so OK and Cancel stay put.
            ui.vertical(|ui| {
                ui.set_min_size(vec2(420.0, 220.0));
                match st.tab {
                    Tab::General => {
                        egui::Grid::new("dwgprops_general").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                            for (k, v) in &st.general {
                                ui.label(crate::i18n::t(k));
                                ui.label(RichText::new(v).color(t.text_dim));
                                ui.end_row();
                            }
                        });
                    }
                    Tab::Summary | Tab::Statistics => {
                        let rows = if st.tab == Tab::Summary { SUMMARY } else { STATISTICS };
                        egui::Grid::new("dwgprops_fields").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                            for (k, label) in rows {
                                ui.label(crate::i18n::t(label));
                                let mut v = st.fields.get(*k).and_then(Value::as_str).unwrap_or_default().to_string();
                                if ui.add(egui::TextEdit::singleline(&mut v).desired_width(280.0)).changed() {
                                    st.fields.insert((*k).into(), json!(v));
                                }
                                ui.end_row();
                            }
                        });
                    }
                    Tab::Custom => {
                        ui.label(RichText::new(crate::tl!("Custom properties")).strong());
                        egui::Grid::new("dwgprops_custom").num_columns(2).striped(true).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                            ui.label(RichText::new(crate::tl!("Name")).strong());
                            ui.label(RichText::new(crate::tl!("Value")).strong());
                            ui.end_row();
                            for (i, (n, v)) in st.custom.iter_mut().enumerate() {
                                let r = ui.add(egui::TextEdit::singleline(n).desired_width(140.0));
                                let r2 = ui.add(egui::TextEdit::singleline(v).desired_width(200.0));
                                if r.gained_focus() || r2.gained_focus() {
                                    st.sel = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.add_enabled(st.custom.len() < MAX_CUSTOM, egui::Button::new(crate::tl!("Add..."))).clicked() {
                                st.custom.push((new_name(&st.custom), String::new()));
                            }
                            let sel = st.sel.filter(|i| *i < st.custom.len());
                            if ui.add_enabled(sel.is_some(), egui::Button::new(crate::tl!("Delete"))).clicked()
                                && let Some(i) = sel
                            {
                                st.custom.remove(i);
                                st.sel = None;
                            }
                        });
                        if duplicate(&st.custom) {
                            ui.label(RichText::new(crate::tl!("Two custom properties have the same name.")).color(t.warn));
                        }
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel = ui.button(crate::tl!("Cancel")).clicked();
                    ok = ui.add_enabled(!duplicate(&st.custom), egui::Button::new(crate::tl!("OK"))).clicked();
                });
            });
        });
    set_state(ctx, "dwgprops_dlg", st.clone());
    if cancel {
        *open = false;
    }
    if !ok {
        return None;
    }
    *open = false;
    changes(&st).map(|p| ("dwgprops", p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_writes_only_what_changed_and_reads_back() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        let mut st = fresh(&mut app);
        assert_eq!(changes(&st), None, "nothing edited");
        st.fields.insert("title".into(), json!("Kitchen"));
        st.fields.insert("comments".into(), json!("two\nlines"));
        st.custom.push(("Client".into(), "Kim".into()));
        let p = changes(&st).unwrap();
        assert_eq!(p, json!({ "title": "Kitchen", "comments": "two lines", "custom": { "Client": "Kim" } }));
        app.run("dwgprops", p).unwrap();
        let st = fresh(&mut app);
        assert_eq!(st.fields.get("title"), Some(&json!("Kitchen")));
        assert_eq!(st.custom, vec![("Client".to_string(), "Kim".to_string())]);
        assert_eq!(changes(&st), None);
    }

    #[test]
    fn new_custom_names_are_free_and_duplicates_are_caught() {
        let mut custom = Vec::new();
        for _ in 0..2 {
            custom.push((new_name(&custom), String::new()));
        }
        custom.remove(0);
        // "Property 2" is left: the next is "Property 1", not a second "Property 2".
        assert_eq!(new_name(&custom), "Property 1");
        assert!(!duplicate(&custom));
        custom.push(("property 2".into(), "x".into()));
        assert!(duplicate(&custom));
    }
}
