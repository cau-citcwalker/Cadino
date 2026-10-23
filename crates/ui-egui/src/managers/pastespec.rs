//! Paste Special (PASTESPEC, PA; Edit ▸ Paste Special...): paste the clipboard as Cadino
//! objects (PASTECLIP), as a block (PASTEBLOCK), or text as multiline text. Each asks for the
//! insertion point on the command line as the paste commands do; text goes onto the drawing
//! clipboard as one multiline text object for that paste, and the clipboard comes back after.

use cadcraft_engine::cmd::clipboard::ClipSource;
use cadcraft_engine::doc::Entity;
use cadcraft_geom::Vec2;
use egui::RichText;

use super::{Action, set_state, set_temp, state, temp};
use crate::CadApp;
use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum As {
    #[default]
    Objects,
    Block,
    Text,
}

#[derive(Clone, Default)]
struct St {
    choice: Option<As>,
    text: String,
}

/// DXF text of one multiline text object holding `text` at the origin, `height` high (DXF
/// Reference: MTEXT; `\P` breaks paragraphs, group 3 carries all but the last 250 characters).
fn mtext_dxf(text: &str, height: f64) -> String {
    let mut body = String::new();
    for c in text.replace("\r\n", "\n").chars() {
        match c {
            '\\' => body.push_str("\\\\"),
            '{' => body.push_str("\\{"),
            '}' => body.push_str("\\}"),
            '\n' => body.push_str("\\P"),
            // MTEXT's tab.
            '\t' => body.push_str("^I"),
            c if c.is_control() => {}
            c => body.push(c),
        }
    }
    let chars: Vec<char> = body.chars().collect();
    let chunks: Vec<String> = chars.chunks(250).map(|c| c.iter().collect()).collect();
    let mut out = String::from(
        "0\nSECTION\n2\nHEADER\n9\n$INSBASE\n10\n0\n20\n0\n30\n0\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nMTEXT\n8\n0\n10\n0\n20\n0\n30\n0\n",
    );
    out.push_str(&format!("40\n{height}\n71\n1\n"));
    let n = chunks.len();
    for (i, c) in chunks.iter().enumerate() {
        out.push_str(if i + 1 < n { "3\n" } else { "1\n" });
        out.push_str(c);
        out.push('\n');
    }
    out.push_str("0\nENDSEC\n0\nEOF\n");
    out
}

/// Put `text` on the drawing clipboard as multiline text. False when it can't be read back.
fn text_to_clipboard(app: &mut CadApp, text: &str) -> bool {
    let height = app.session.doc().map(|d| d.header.f64("TEXTSIZE", 2.5)).ok().filter(|h| h.is_finite() && *h > 0.0).unwrap_or(2.5);
    cadcraft_engine::cmd::clipboard::load_system_text(&mut app.session, &mtext_dxf(text, height))
}

/// The drawing clipboard as it was before a text paste, put back once that paste ends.
type Saved = (Vec<Entity>, Vec2, Option<ClipSource>);
const SAVED: &str = "pastespec_saved";

/// Each frame: once the text paste is over, the clipboard is what it was before.
pub(super) fn restore_clipboard(app: &mut CadApp, ctx: &egui::Context) {
    if app.session.running.is_some() {
        return;
    }
    let Some((ents, base, source)) = temp::<Saved>(ctx, SAVED) else { return };
    ctx.data_mut(|d| d.remove::<Saved>(egui::Id::new(SAVED)));
    app.session.clipboard = ents;
    app.session.clipboard_base = base;
    app.session.clipboard_source = source;
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let objects = !app.session.clipboard.is_empty() && app.session.state().is_ok();
    let mut st = state(ctx, "pastespec_dlg", St::default);
    let choice = st.choice.unwrap_or(if objects { As::Objects } else { As::Text });
    let mut choice = if !objects && choice != As::Text { As::Text } else { choice };
    let source = match &app.session.clipboard_source {
        _ if !objects => crate::tl!("The clipboard holds no drawing objects."),
        Some(c) if c.uid.is_none() => crate::tl!("Objects from another Cadino window"),
        _ if cadcraft_engine::cmd::clipboard::from_active(&app.session) => crate::tl!("Objects from this drawing"),
        _ => crate::tl!("Objects from another drawing"),
    };
    let (mut ok, mut cancel) = (false, false);
    egui::Window::new(crate::tl!("Paste Special")).id(egui::Id::new("Paste Special")).open(open).collapsible(false).resizable(false).show(
        ctx,
        |ui| {
            ui.horizontal(|ui| {
                ui.label(crate::tl!("Source:"));
                ui.label(RichText::new(source).color(t.text_dim));
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    let _ = ui.radio(true, crate::tl!("Paste"));
                    ui.add_enabled(false, egui::RadioButton::new(false, crate::tl!("Paste Link")));
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(crate::tl!("As:"));
                    ui.add_enabled_ui(objects, |ui| {
                        ui.selectable_value(&mut choice, As::Objects, crate::tl!("Cadino Drawing Objects"));
                        ui.selectable_value(&mut choice, As::Block, crate::tl!("Cadino Drawing Objects as a Block"));
                    });
                    ui.selectable_value(&mut choice, As::Text, crate::tl!("Text"));
                });
            });
            if choice == As::Text {
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut st.text)
                        .hint_text(crate::tl!("Paste the text here (Ctrl+V)"))
                        .desired_rows(4)
                        .desired_width(360.0),
                );
            }
            ui.separator();
            ui.label(RichText::new(crate::tl!("Result")).strong());
            let result = match choice {
                As::Objects => crate::tl!("Pastes the objects; you then pick the insertion point."),
                As::Block => crate::tl!("Pastes the objects as one block reference; you then pick the insertion point."),
                As::Text => crate::tl!("Pastes the text as multiline text; you then pick the insertion point."),
            };
            ui.add(egui::Label::new(RichText::new(result).color(t.text_dim)).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.set_min_width(360.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel = ui.button(crate::tl!("Cancel")).clicked();
                    let can = choice != As::Text || !st.text.trim().is_empty();
                    ok = ui.add_enabled(can, egui::Button::new(crate::tl!("OK"))).clicked();
                });
            });
        },
    );
    st.choice = Some(choice);
    set_state(ctx, "pastespec_dlg", st.clone());
    if cancel {
        *open = false;
    }
    if !ok {
        return None;
    }
    *open = false;
    match choice {
        As::Objects => app.start("pasteclip"),
        As::Block => app.start("pasteblock"),
        As::Text => {
            let saved: Saved = (app.session.clipboard.clone(), app.session.clipboard_base, app.session.clipboard_source.clone());
            if text_to_clipboard(app, &st.text) {
                set_temp(ctx, SAVED, saved);
                app.start("pasteclip");
            } else {
                app.session.echo("The text could not be pasted.");
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_one_multiline_text() {
        let long = "x".repeat(600);
        let text = format!("Room 101\r\n{{braces}} and \\ backslash\n{long}");
        let d = cadcraft_io::read_dxf(mtext_dxf(&text, 3.5).as_bytes()).unwrap();
        assert_eq!(d.model.len(), 1);
        let e = d.model.iter().next().unwrap();
        let cadcraft_engine::doc::EntityKind::MText(m) = &e.kind else { panic!("multiline text") };
        assert_eq!(m.contents, format!("Room 101\\P\\{{braces\\}} and \\\\ backslash\\P{long}"));
        assert!((m.height - 3.5).abs() < 1e-12);
        // A tab stays a tab (MTEXT's ^I), so pasted columns keep apart.
        let d = cadcraft_io::read_dxf(mtext_dxf("Room\t101", 1.0).as_bytes()).unwrap();
        let cadcraft_engine::doc::EntityKind::MText(m) = &d.model.iter().next().unwrap().kind else { panic!("multiline text") };
        assert_eq!(m.contents, "Room\t101");
    }

    #[test]
    fn a_text_paste_leaves_the_clipboard_as_it_was() {
        let mut app = CadApp::new(cadcraft_engine::Session::new(), crate::Services::default());
        app.run("line", serde_json::json!({ "points": [[0, 0], [1, 0]] })).unwrap();
        app.run("selectall", serde_json::json!({})).unwrap();
        app.run("copyclip", serde_json::json!({})).unwrap();
        let copied = app.session.clipboard.clone();
        assert_eq!(copied.len(), 1);
        let ctx = egui::Context::default();
        let saved: Saved = (app.session.clipboard.clone(), app.session.clipboard_base, app.session.clipboard_source.clone());
        // The text goes onto the clipboard for its paste (the file hooks may be missing in
        // tests, so another copy stands in for it here).
        app.run("line", serde_json::json!({ "points": [[0, 5], [1, 5]] })).unwrap();
        app.run("selectall", serde_json::json!({})).unwrap();
        app.run("copyclip", serde_json::json!({})).unwrap();
        set_temp(&ctx, SAVED, saved);
        app.start("pasteclip");
        assert!(app.session.running.is_some());
        restore_clipboard(&mut app, &ctx);
        assert_ne!(app.session.clipboard, copied, "kept while the paste runs");
        app.cmdline("0,0");
        assert!(app.session.running.is_none());
        restore_clipboard(&mut app, &ctx);
        assert_eq!(app.session.clipboard, copied);
    }
}
