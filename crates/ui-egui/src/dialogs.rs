//! Dialogs: Drafting Settings, Drawing Units, About, command reference, blocks; dispatches the
//! Layer Properties Manager ([`crate::layers`]), Quick Select ([`crate::quick`]), Parameters
//! Manager ([`crate::parametric`]) and the style and settings managers ([`crate::managers`]).

use egui::{RichText, vec2};
use serde_json::json;

use crate::CadApp;
use crate::theme::Tokens;

pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    crate::mtext_editor::show(app, ctx);
    crate::quick::quick_properties(app, ctx);
    crate::managers::end_hidden(app, ctx);
    let Some(d) = app.ui.dialog.clone() else { return };
    let mut open = true;
    match d.as_str() {
        "spacemouse" => crate::spacemouse::dialog(app, ctx, &mut open),
        "language" => language(app, ctx, &mut open),
        "layers" => crate::layers::dialog(app, ctx, &mut open),
        "qselect" => crate::quick::qselect_dialog(app, ctx, &mut open),
        "parameters" => crate::parametric::parameters_dialog(app, ctx, &mut open),
        o if o == "dsettings" || o.starts_with("dsettings:") => dsettings(app, ctx, o, &mut open),
        "units" => units(app, ctx, &mut open),
        "hatch" => crate::hatchdlg::dialog(app, ctx, &mut open),
        "about" => about(ctx, &mut open),
        "preview" => crate::exchange::dialog(app, ctx, &mut open),
        "commands" => commands(app, ctx, &mut open),
        "blocks" => blocks(app, ctx, &mut open),
        "bedit" => crate::bedit::dialog(app, ctx, &mut open),
        "authoring" => crate::bedit::authoring_dialog(app, ctx, &mut open),
        "stylesmanager" => crate::plotstyles::dialog(app, ctx, &mut open),
        "visualstyles" => crate::visualstyles::dialog(app, ctx, &mut open),
        "render" => crate::render_window::dialog(app, ctx, &mut open),
        o if o == "options" || o.starts_with("options:") => crate::options::dialog(app, ctx, o, &mut open),
        other => crate::managers::dialog(app, ctx, other, &mut open),
    }
    // A dialog that closed by opening another (Plot Preview back to Plot) keeps the new one.
    if !open && app.ui.dialog.as_deref() == Some(d.as_str()) {
        app.ui.dialog = None;
    }
}

/// The Drafting Settings tabs: (id in the dialog name, label). The open tab is part of the
/// dialog name (`"dsettings:osnap"`): DSETTINGS opens the first, OSNAP the Object Snap tab,
/// 3DOSNAP the 3D Object Snap tab.
const DSETTINGS_TABS: [(&str, &str); 5] =
    [("snap", "Snap and Grid"), ("polar", "Polar Tracking"), ("osnap", "Object Snap"), ("3dosnap", "3D Object Snap"), ("dyn", "Dynamic Input")];

/// Drafting Settings (`name` is `"dsettings"` or `"dsettings:<tab>"`). The tabs edit a copy of
/// the settings; every change goes through SETVAR ([`drafting_vars`]), as typed.
fn dsettings(app: &mut CadApp, ctx: &egui::Context, name: &str, open: &mut bool) {
    let tab = name.strip_prefix("dsettings:").and_then(|t| DSETTINGS_TABS.iter().find(|(id, _)| *id == t)).map_or("snap", |(id, _)| *id);
    let settings = app.session.drafting_settings();
    let (mut snapunit, mut gridunit) = (settings.snapunit, settings.gridunit);
    let before = app.session.settings.clone();
    let mut s = before.clone();
    let mut next_tab = None;
    egui::Window::new(crate::tl!("Drafting Settings")).id(egui::Id::new("Drafting Settings")).open(open).resizable(false).show(ctx, |ui| {
        ui.set_min_width(440.0);
        ui.horizontal_wrapped(|ui| {
            for (id, label) in DSETTINGS_TABS {
                if ui.selectable_label(tab == id, crate::i18n::t(label)).clicked() {
                    next_tab = Some(id);
                }
            }
        });
        ui.separator();
        match tab {
            "polar" => polar_tab(ui, &mut s),
            "osnap" => osnap_tab(ui, &mut s),
            "3dosnap" => osnap3d_tab(ui, &mut s),
            "dyn" => dyn_tab(ui, &mut s),
            _ => snap_tab(ui, &mut s, &mut snapunit, &mut gridunit),
        }
    });
    for ((name, was), (_, now)) in drafting_vars(&before).into_iter().zip(drafting_vars(&s)) {
        if was != now {
            let _ = app.run("setvar", json!({ "name": name, "value": now }));
        }
    }
    // Drawing edits go through the same command as the CLI, including dirty state and undo.
    for (name, before, after) in [("SNAPUNIT", settings.snapunit, snapunit), ("GRIDUNIT", settings.gridunit, gridunit)] {
        if before != after {
            let _ = app.run("setvar", json!({ "name": name, "value": [after.x, after.y] }));
        }
    }
    if let Some(t) = next_tab {
        app.ui.dialog = Some(format!("dsettings:{t}"));
    }
}

/// The system variables Drafting Settings edits, with their values in `s`.
fn drafting_vars(s: &cadcraft_engine::Settings) -> [(&'static str, serde_json::Value); 17] {
    let on = |b: bool| json!(i32::from(b));
    [
        ("SNAPMODE", on(s.snapmode)),
        ("SNAPSTYL", on(s.isodraft)),
        ("SNAPTYPE", on(s.polarsnap)),
        ("POLARDIST", json!(s.polardist)),
        ("GRIDMODE", on(s.gridmode)),
        ("GRIDMAJOR", json!(s.gridmajor)),
        ("ORTHOMODE", on(s.orthomode)),
        // Polar tracking (8) and object snap tracking (16); the marker bits are always on.
        ("AUTOSNAP", json!(37 + 8 * u32::from(s.polarmode) + 16 * u32::from(s.otrack))),
        ("POLARANG", json!(s.polarang.to_degrees())),
        ("POLARMODE", json!(s.polar_flags)),
        ("POLARADDANG", json!(cadcraft_engine::sysvars::format_angle_list(&s.polaraddang))),
        ("OSMODE", json!(s.osmode)),
        ("3DOSMODE", json!(s.osmode3d)),
        ("DYNMODE", on(s.dynmode)),
        ("DYNPIFORMAT", on(s.dynpi_cartesian)),
        ("DYNPICOORDS", on(s.dynpi_absolute)),
        ("DYNPROMPT", on(s.dynprompt)),
    ]
}

fn snap_tab(
    ui: &mut egui::Ui,
    s: &mut cadcraft_engine::Settings,
    snapunit: &mut cadcraft_engine::geom::Vec2,
    gridunit: &mut cadcraft_engine::geom::Vec2,
) {
    ui.checkbox(&mut s.snapmode, crate::tl!("Snap On (F9)"));
    // SNAPSTYL: the isometric snap and grid (ISODRAFT).
    ui.horizontal(|ui| {
        ui.radio_value(&mut s.isodraft, false, crate::tl!("Rectangular snap"));
        ui.radio_value(&mut s.isodraft, true, crate::tl!("Isometric snap"));
    });
    ui.horizontal(|ui| {
        ui.label(crate::tl!("Snap X spacing"));
        ui.add(egui::DragValue::new(&mut snapunit.x).speed(0.05).range(0.0001..=1e6).clamp_existing_to_range(false));
        ui.label(crate::tl!("Y"));
        ui.add(egui::DragValue::new(&mut snapunit.y).speed(0.05).range(0.0001..=1e6).clamp_existing_to_range(false));
    });
    // SNAPTYPE and POLARDIST.
    ui.horizontal(|ui| {
        ui.label(crate::tl!("Snap type"));
        ui.radio_value(&mut s.polarsnap, false, crate::tl!("Grid snap"));
        ui.radio_value(&mut s.polarsnap, true, crate::tl!("PolarSnap"));
    });
    ui.add_enabled_ui(s.polarsnap, |ui| {
        ui.horizontal(|ui| {
            ui.label(crate::tl!("Polar distance"));
            ui.add(egui::DragValue::new(&mut s.polardist).speed(0.05).range(0.0..=1e6));
        });
    });
    ui.separator();
    ui.checkbox(&mut s.gridmode, crate::tl!("Grid On (F7)"));
    ui.horizontal(|ui| {
        ui.label(crate::tl!("Grid spacing"));
        ui.add(egui::DragValue::new(&mut gridunit.x).speed(0.05).range(0.0..=1e6).clamp_existing_to_range(false));
        ui.label(crate::tl!("Major line every"));
        ui.add(egui::DragValue::new(&mut s.gridmajor).range(1..=100));
    });
    ui.separator();
    ui.checkbox(&mut s.orthomode, crate::tl!("Ortho (F8)"));
}

fn polar_tab(ui: &mut egui::Ui, s: &mut cadcraft_engine::Settings) {
    ui.checkbox(&mut s.polarmode, crate::tl!("Polar Tracking On (F10)"));
    let was = (s.polarang.to_degrees() * 1e6).round() / 1e6;
    let mut deg = was;
    ui.horizontal(|ui| {
        ui.label(crate::tl!("Increment angle"));
        egui::ComboBox::from_id_salt("polarang").selected_text(format!("{deg}")).show_ui(ui, |ui| {
            for a in [90.0, 45.0, 30.0, 22.5, 18.0, 15.0, 10.0, 5.0] {
                ui.selectable_value(&mut deg, a, format!("{a}"));
            }
        });
    });
    // Only a pick changes it: radians to degrees and back would drift.
    if deg != was {
        s.polarang = deg.to_radians();
    }
    tracking_options(ui, s);
}

/// The Object Snap tab: running snaps on (F3: OSMODE's 16384 bit clear), object snap tracking
/// (F11) and the OSMODE modes, each beside its marker, in AutoCAD's two columns.
fn osnap_tab(ui: &mut egui::Ui, s: &mut cadcraft_engine::Settings) {
    use cadcraft_engine::snap::mode;
    let modes = mode::ALL.iter().fold(0, |a, (b, _)| a | b);
    ui.horizontal(|ui| {
        let mut on = s.osmode & mode::OFF == 0;
        if ui.checkbox(&mut on, crate::tl!("Object Snap On (F3)")).changed() {
            s.osmode ^= mode::OFF;
        }
        ui.add_space(24.0);
        ui.checkbox(&mut s.otrack, crate::tl!("Object Snap Tracking On (F11)"));
    });
    ui.separator();
    ui.horizontal_top(|ui| {
        let (left, right) = mode::ALL.split_at(8);
        for column in [left, right] {
            ui.vertical(|ui| {
                ui.set_min_width(170.0);
                for (bit, name) in column {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
                        crate::canvas::snap_glyph(ui.painter(), r.center(), *bit, 5.0, egui::Stroke::new(1.5, Tokens::get().snap));
                        let mut v = s.osmode & bit != 0;
                        if ui.checkbox(&mut v, crate::i18n::t(name)).changed() {
                            s.osmode ^= bit;
                        }
                    });
                }
            });
        }
        ui.vertical(|ui| {
            if ui.button(crate::tl!("Select All")).clicked() {
                s.osmode |= modes;
            }
            if ui.button(crate::tl!("Clear All")).clicked() {
                s.osmode &= !modes;
            }
        });
    });
}

/// The 3D Object Snap tab: 3D object snaps on (F4: 3DOSMODE's bit 1 clear) and the 3DOSMODE
/// modes, each beside its marker.
fn osnap3d_tab(ui: &mut egui::Ui, s: &mut cadcraft_engine::Settings) {
    use cadcraft_engine::cmd::osnap3d::mode3d;
    let mut on = s.osmode3d & mode3d::OFF == 0;
    if ui.checkbox(&mut on, crate::tl!("3D Object Snap On (F4)")).changed() {
        s.osmode3d ^= mode3d::OFF;
    }
    ui.separator();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_min_width(220.0);
            for (bit, name, _, marker) in mode3d::ALL {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
                    crate::canvas::snap_glyph(ui.painter(), r.center(), marker, 5.0, egui::Stroke::new(1.5, Tokens::get().snap));
                    let mut v = s.osmode3d & bit != 0;
                    if ui.checkbox(&mut v, crate::i18n::t(name)).changed() {
                        s.osmode3d ^= bit;
                    }
                });
            }
        });
        ui.vertical(|ui| {
            if ui.button(crate::tl!("Select All")).clicked() {
                s.osmode3d |= mode3d::MODES;
            }
            if ui.button(crate::tl!("Clear All")).clicked() {
                s.osmode3d &= !mode3d::MODES;
            }
        });
    });
}

fn dyn_tab(ui: &mut egui::Ui, s: &mut cadcraft_engine::Settings) {
    ui.checkbox(&mut s.dynmode, crate::tl!("Enable Dynamic Input (F12)"));
    // Pointer input format for second and next points (DYNPIFORMAT, DYNPICOORDS).
    ui.indent("dynpi", |ui| {
        ui.add_enabled_ui(s.dynmode, |ui| {
            ui.label(crate::tl!("Second and next points:"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.dynpi_cartesian, false, crate::tl!("Polar (distance < angle)"));
                ui.radio_value(&mut s.dynpi_cartesian, true, crate::tl!("Cartesian (x, y)"));
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.dynpi_absolute, false, crate::tl!("Relative"));
                ui.radio_value(&mut s.dynpi_absolute, true, crate::tl!("Absolute"));
            });
            // DYNPROMPT: the prompt and its options (Down Arrow) beside the cursor.
            ui.checkbox(&mut s.dynprompt, crate::tl!("Show command prompting near the crosshairs"));
        });
    });
}

/// The polar and object snap tracking options of Drafting Settings: additional angles
/// (POLARMODE bit 4, POLARADDANG), tracking on every polar angle (bit 2) and angles relative to
/// the last segment (bit 1).
fn tracking_options(ui: &mut egui::Ui, s: &mut cadcraft_engine::Settings) {
    use cadcraft_engine::snap::tracking::polarmode;
    /// A radio button for POLARMODE `bit` being `on`.
    fn flag(ui: &mut egui::Ui, flags: &mut u32, bit: u32, on: bool, label: &str) {
        if ui.radio((*flags & bit != 0) == on, label).clicked() {
            *flags = if on { *flags | bit } else { *flags & !bit };
        }
    }
    let mut additional = s.polar_flags & polarmode::ADDITIONAL != 0;
    ui.horizontal(|ui| {
        if ui.checkbox(&mut additional, crate::tl!("Additional angles")).changed() {
            s.polar_flags ^= polarmode::ADDITIONAL;
        }
        // The list as typed (degrees separated by `;`), kept while it doesn't parse yet.
        let id = ui.id().with("polaraddang");
        let shown = cadcraft_engine::sysvars::format_angle_list(&s.polaraddang);
        let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| shown.clone());
        let resp = ui.add_enabled(additional, egui::TextEdit::singleline(&mut text).desired_width(140.0));
        if resp.changed()
            && let Some(angles) = cadcraft_engine::sysvars::parse_angle_list(&text)
        {
            s.polaraddang = angles;
        }
        if resp.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, text));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
    });
    ui.label(crate::tl!("Object Snap Tracking Settings"));
    ui.horizontal(|ui| {
        flag(ui, &mut s.polar_flags, polarmode::TRACK_POLAR, false, crate::tl!("Track orthogonally only"));
        flag(ui, &mut s.polar_flags, polarmode::TRACK_POLAR, true, crate::tl!("Track using all polar angle settings"));
    });
    ui.label(crate::tl!("Polar angle measurement"));
    ui.horizontal(|ui| {
        flag(ui, &mut s.polar_flags, polarmode::RELATIVE, false, crate::tl!("Absolute"));
        flag(ui, &mut s.polar_flags, polarmode::RELATIVE, true, crate::tl!("Relative to last segment"));
    });
}

/// LUNITS values in the order the Type list shows them.
const LENGTH_TYPES: [(i64, &str); 5] = [(4, "Architectural"), (2, "Decimal"), (3, "Engineering"), (5, "Fractional"), (1, "Scientific")];
/// AUNITS values.
const ANGLE_TYPES: [(i64, &str); 5] = [(0, "Decimal Degrees"), (1, "Deg/Min/Sec"), (2, "Grads"), (3, "Radians"), (4, "Surveyor's Units")];
/// INSUNITS names, indexed by value (DXF Reference, header group code 70).
const INSERTION_UNITS: [&str; 25] = [
    "Unitless",
    "Inches",
    "Feet",
    "Miles",
    "Millimeters",
    "Centimeters",
    "Meters",
    "Kilometers",
    "Microinches",
    "Mils",
    "Yards",
    "Angstroms",
    "Nanometers",
    "Microns",
    "Decimeters",
    "Decameters",
    "Hectometers",
    "Gigameters",
    "Astronomical Units",
    "Light Years",
    "Parsecs",
    "US Survey Feet",
    "US Survey Inch",
    "US Survey Yard",
    "US Survey Mile",
];

/// A combo box over `(value, label)` options.
fn pick(ui: &mut egui::Ui, salt: &str, value: &mut i64, options: &[(i64, String)]) {
    let current = options.iter().find(|o| o.0 == *value).map(|o| o.1.clone()).unwrap_or_default();
    egui::ComboBox::from_id_salt(salt).selected_text(current).width(180.0).show_ui(ui, |ui| {
        for (k, label) in options {
            ui.selectable_value(value, *k, label);
        }
    });
}

/// Drawing Units (UNITS): edits a copy of LUNITS/LUPREC/AUNITS/AUPREC/INSUNITS and applies it
/// through the `units` command on OK.
fn units(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    use cadcraft_engine::units::{format_angle, format_distance};
    let id = egui::Id::new("units_dialog_values");
    let mut v = ctx.data_mut(|d| d.get_temp::<[i64; 5]>(id)).unwrap_or_else(|| {
        app.session
            .doc()
            .map(|d| {
                let h = &d.header;
                [h.i64("LUNITS", 2), h.i64("LUPREC", 4), h.i64("AUNITS", 0), h.i64("AUPREC", 0), h.i64("INSUNITS", 1)]
            })
            .unwrap_or([2, 4, 0, 0, 1])
    });
    let (mut apply, mut cancel) = (false, false);
    egui::Window::new("Drawing Units").open(open).resizable(false).collapsible(false).show(ctx, |ui| {
        let [lu, lp, au, ap, ins] = &mut v;
        let types = |t: &[(i64, &str)]| t.iter().map(|(k, l)| (*k, l.to_string())).collect::<Vec<_>>();
        egui::Grid::new("units_grid").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
            ui.strong("Length");
            ui.end_row();
            ui.label("Type");
            pick(ui, "units_lunits", lu, &types(&LENGTH_TYPES));
            ui.end_row();
            ui.label("Precision");
            pick(ui, "units_luprec", lp, &(0..=8).map(|p| (p, format_distance(1.5, *lu, p))).collect::<Vec<_>>());
            ui.end_row();
            ui.strong("Angle");
            ui.end_row();
            ui.label("Type");
            pick(ui, "units_aunits", au, &types(&ANGLE_TYPES));
            ui.end_row();
            ui.label("Precision");
            pick(ui, "units_auprec", ap, &(0..=8).map(|p| (p, format_angle(45f64.to_radians(), *au, p))).collect::<Vec<_>>());
            ui.end_row();
            ui.strong("Insertion scale");
            ui.end_row();
            ui.label("Units to scale inserted content");
            let names = INSERTION_UNITS.iter().enumerate().map(|(k, l)| (k as i64, l.to_string())).collect::<Vec<_>>();
            pick(ui, "units_insunits", ins, &names);
            ui.end_row();
            ui.strong("Sample output");
            ui.end_row();
            ui.label("");
            ui.label(format!("{},{},{}", format_distance(1.5, *lu, *lp), format_distance(2.0039, *lu, *lp), format_distance(0.0, *lu, *lp)));
            ui.end_row();
            ui.label("");
            ui.label(format_angle(45f64.to_radians(), *au, *ap));
            ui.end_row();
        });
        ui.separator();
        ui.horizontal(|ui| {
            apply = ui.button("OK").clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if apply {
        let [lu, lp, au, ap, ins] = v;
        let _ = app.run("units", json!({ "lunits": lu, "luprec": lp, "aunits": au, "auprec": ap, "insunits": ins }));
    }
    if apply || cancel || !*open {
        ctx.data_mut(|d| d.remove::<[i64; 5]>(id));
        *open = false;
    } else {
        ctx.data_mut(|d| d.insert_temp(id, v));
    }
}

fn about(ctx: &egui::Context, open: &mut bool) {
    let t = Tokens::get();
    let tab_id = egui::Id::new("about_tab");
    egui::Window::new(crate::tl!("About Cadino"))
        .id(egui::Id::new("About Cadino"))
        .open(open)
        .default_size(vec2(640.0, 420.0))
        .collapsible(false)
        .show(ctx, |ui| {
            let mut tab = ui.data_mut(|d| d.get_temp::<u8>(tab_id)).unwrap_or(0);
            ui.horizontal(|ui| {
                for (i, l) in ["About", "Contributors", "Models"].iter().enumerate() {
                    if ui.selectable_label(tab == i as u8, crate::i18n::t(l)).clicked() {
                        tab = i as u8;
                    }
                }
            });
            ui.data_mut(|d| d.insert_temp(tab_id, tab));
            ui.separator();
            match tab {
                1 => crate::credits::contributors_ui(ui),
                2 => crate::credits::models_ui(ui),
                _ => {
                    ui.heading(crate::tl!("Cadino"));
                    ui.label(crate::tf!("Version {version}", version = env!("CARGO_PKG_VERSION")));
                    ui.label(crate::tl!("Computer-aided design and drafting: an open-source, clean-room CAD application written in pure Rust."));
                    ui.add_space(6.0);
                    ui.label(RichText::new(crate::tl!("One of the Crafting Apps by the ArtCraft team and community.")).color(t.text_dim));
                    ui.hyperlink_to("getartcraft.com/apps/cadcraft", "https://getartcraft.com/apps/cadcraft");
                    ui.hyperlink_to(crate::tl!("Join us on Discord"), "https://discord.gg/artcraft");
                    ui.add_space(6.0);
                    ui.label(RichText::new(crate::tl!("MIT OR Apache-2.0. Not affiliated with Autodesk, Inc.")).small().color(t.text_faint));
                }
            }
        });
}

fn commands(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let mut start = None;
    egui::Window::new(crate::tl!("Command Reference")).id(egui::Id::new("Command Reference")).open(open).default_size(vec2(640.0, 480.0)).show(
        ctx,
        |ui| {
            let id = ui.id().with("cmdfilter");
            let mut filter = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(crate::tl!("Filter"));
                ui.text_edit_singleline(&mut filter);
            });
            ui.data_mut(|d| d.insert_temp(id, filter.clone()));
            let f = filter.to_lowercase();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("cmdref").striped(true).num_columns(3).show(ui, |ui| {
                    for c in cadcraft_engine::command_specs() {
                        if !f.is_empty()
                            && !c.id.contains(&f)
                            && !c.label.to_ascii_lowercase().contains(&f)
                            && !crate::i18n::t(c.label).to_lowercase().contains(&f)
                        {
                            continue;
                        }
                        if ui.link(c.id.to_ascii_uppercase()).clicked() {
                            start = Some(c.id);
                        }
                        ui.label(crate::i18n::t(c.label));
                        ui.label(RichText::new(if c.aliases.is_empty() { String::new() } else { c.aliases.join(", ").to_ascii_uppercase() }).small());
                        ui.end_row();
                    }
                });
            });
        },
    );
    if let Some(c) = start {
        app.ui.dialog = None;
        app.start(c);
    }
}

fn blocks(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    let mut pick = None;
    egui::Window::new(crate::tl!("Blocks")).id(egui::Id::new("Blocks")).open(open).default_size(vec2(320.0, 360.0)).show(ctx, |ui| {
        let Ok(d) = app.session.doc() else { return };
        let names: Vec<&String> = d.blocks.keys().filter(|k| !k.starts_with('*')).collect();
        if names.is_empty() {
            ui.label(crate::tl!("No blocks defined in this drawing."));
        }
        for n in names {
            if ui.button(n).on_hover_text(crate::tl!("Insert this block")).clicked() {
                pick = Some(n.clone());
            }
        }
    });
    if let Some(n) = pick {
        insert_block(app, &n);
    }
}

/// Start INSERT with `name` already given at its block name prompt, so the next prompt asks for
/// the insertion point.
pub(crate) fn insert_block(app: &mut CadApp, name: &str) {
    app.start("insert");
    if app.session.running.as_ref().is_none_or(|r| r.id != "insert") {
        return;
    }
    if let Err(e) = app.session.input(cadcraft_engine::Input::Text(name.to_string())) {
        app.session.echo(e.to_string());
    }
}

fn language(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) {
    egui::Window::new(crate::tl!("Interface Language")).id(egui::Id::new("interface_language")).open(open).resizable(false).show(ctx, |ui| {
        egui::ComboBox::from_id_salt("interface_language_choice").selected_text(app.ui.interface_language.name()).show_ui(ui, |ui| {
            for preference in crate::i18n::Preference::ALL {
                if ui.selectable_label(app.ui.interface_language == preference, preference.name()).clicked() {
                    let _ = app.run("ui.language", serde_json::json!({"lang": preference.code()}));
                    ui.ctx().request_repaint();
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use cadcraft_engine::doc::EntityKind;
    use cadcraft_engine::geom::Vec2;
    use cadcraft_engine::{Input, Session};
    use serde_json::json;

    use crate::{CadApp, Services};

    #[test]
    fn picking_a_block_starts_insert_with_its_name() {
        let mut app = CadApp::new(Session::new(), Services::default());
        let c = app.session.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap();
        let h = c["handle"].as_str().unwrap().to_string();
        app.session.execute("block", &json!({"name": "Door 2", "base": [0, 0], "handles": [h], "keep": "delete"})).unwrap();
        // A command already running is replaced, as when a command is typed.
        app.start("line");

        super::insert_block(&mut app, "Door 2");
        assert!(app.session.running.as_ref().is_some_and(|r| r.id == "insert"));
        assert_eq!(app.session.current_prompt().map(|p| p.message), Some("Specify insertion point".to_string()));

        app.session.input(Input::Point(Vec2::new(5.0, 6.0))).unwrap();
        // X scale, Y scale and rotation take their defaults.
        for _ in 0..3 {
            app.session.input(Input::Enter).unwrap();
        }
        assert!(app.session.running.is_none());
        let d = app.session.doc().unwrap();
        let mut ins = Vec::new();
        for e in d.model.iter() {
            if let EntityKind::Insert(i) = &e.kind {
                ins.push(i.clone());
            }
        }
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].block, "Door 2");
        assert!((ins[0].insert.x - 5.0).abs() < 1e-9 && (ins[0].insert.y - 6.0).abs() < 1e-9);
    }

    #[test]
    fn picking_a_missing_block_keeps_asking_for_a_name() {
        let mut app = CadApp::new(Session::new(), Services::default());
        super::insert_block(&mut app, "Nope");
        assert!(app.session.running.as_ref().is_some_and(|r| r.id == "insert"));
        assert_eq!(app.session.current_prompt().map(|p| p.message), Some("Enter block name".to_string()));
    }

    /// One frame of the app in a 1600×1000 window with `events`; the text painted, with where.
    fn frame(app: &mut CadApp, ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<(String, egui::Rect)> {
        fn collect(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(s) => out.push((s.galley.text().to_string(), s.visual_bounding_rect())),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
            events,
            ..Default::default()
        };
        app.logic(ctx);
        let mut output = ctx.run_ui(input, |ui| app.ui(ui));
        output.textures_delta.clear();
        let mut out = Vec::new();
        output.shapes.iter().for_each(|s| collect(&s.shape, &mut out));
        out
    }

    /// Click the text `label` painted in the window.
    fn click(app: &mut CadApp, ctx: &egui::Context, label: &str) {
        let shown = frame(app, ctx, Vec::new());
        let at = shown.iter().find(|(t, _)| t == label).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("no {label} in {shown:?}"));
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(app, ctx, vec![egui::Event::PointerMoved(at)]);
        frame(app, ctx, vec![button(true)]);
        frame(app, ctx, vec![button(false)]);
    }

    /// OSNAP typed (OS, DDOSNAP, the snap menu's Osnap Settings) opens Drafting Settings on the
    /// Object Snap tab; -OSNAP asks on the command line, and the JSON form (F3, the status bar
    /// button) toggles the running snaps without a dialog.
    #[test]
    fn typed_osnap_opens_the_object_snap_tab_and_f3_toggles() {
        use cadcraft_engine::snap::mode::OFF;
        let mut app = CadApp::new(Session::new(), Services::default());
        for typed in ["osnap", "OS", "ddosnap"] {
            app.ui.dialog = None;
            assert_eq!(app.cmdline(typed), None);
            assert_eq!(app.ui.dialog.as_deref(), Some("dsettings:osnap"), "{typed}");
        }
        app.ui.dialog = None;
        crate::context_menu::choose_snap(&mut app, "osnap");
        assert_eq!(app.ui.dialog.as_deref(), Some("dsettings:osnap"));
        app.ui.dialog = None;
        app.cmdline("-osnap");
        assert!(app.ui.dialog.is_none() && app.session.running.as_ref().is_some_and(|r| r.id == "-osnap"));
        app.session.cancel();
        let on = app.session.settings.osmode & OFF == 0;
        app.run("osnap", json!({})).unwrap();
        assert_eq!((app.ui.dialog.as_deref(), app.session.settings.osmode & OFF == 0), (None, !on));
        // DSETTINGS keeps its first tab.
        assert_eq!(app.cmdline("ds"), None);
        assert_eq!(app.ui.dialog.as_deref(), Some("dsettings"));
    }

    /// The Object Snap tab sets OSMODE and AUTOSNAP through SETVAR: Clear All, a mode, Select All,
    /// Object Snap Tracking; its tabs switch by name.
    #[test]
    fn the_object_snap_tab_sets_osmode_and_tracking() {
        use cadcraft_engine::snap::mode;
        let mut app = CadApp::new(Session::new(), Services::default());
        app.session.settings.osmode = mode::END | mode::OFF;
        app.session.settings.otrack = false;
        app.ui.dialog = Some("dsettings:osnap".into());
        let ctx = egui::Context::default();
        // A window lays itself out on its first frame and shows on the next.
        frame(&mut app, &ctx, Vec::new());
        let shown: Vec<String> = frame(&mut app, &ctx, Vec::new()).into_iter().map(|(t, _)| t).collect();
        for label in ["Object Snap On (F3)", "Object Snap Tracking On (F11)", "Apparent Intersection", "Parallel", "Select All", "Clear All"] {
            assert!(shown.iter().any(|t| t == label), "no {label} in {shown:?}");
        }
        click(&mut app, &ctx, "Clear All");
        assert_eq!(app.session.settings.osmode, mode::OFF);
        click(&mut app, &ctx, "Nearest");
        click(&mut app, &ctx, "Object Snap On (F3)");
        assert_eq!(app.session.settings.osmode, mode::NEA);
        click(&mut app, &ctx, "Select All");
        assert_eq!(app.session.settings.osmode, mode::ALL.iter().fold(0, |a, (b, _)| a | b));
        click(&mut app, &ctx, "Object Snap Tracking On (F11)");
        assert!(app.session.settings.otrack);
        assert_eq!(cadcraft_engine::sysvars::get(&app.session, "AUTOSNAP"), Some(json!(37 + 16 + 8 * u32::from(app.session.settings.polarmode))));
        click(&mut app, &ctx, "Polar Tracking");
        assert_eq!(app.ui.dialog.as_deref(), Some("dsettings:polar"));
        let shown: Vec<String> = frame(&mut app, &ctx, Vec::new()).into_iter().map(|(t, _)| t).collect();
        assert!(shown.iter().any(|t| t == "Polar Tracking On (F10)"), "{shown:?}");
    }

    /// 3DOSNAP typed opens the 3D Object Snap tab; F4 (its JSON form) toggles the 3D object snaps.
    /// The tab sets 3DOSMODE through SETVAR: Clear All, a mode, On.
    #[test]
    fn the_3d_object_snap_tab_sets_3dosmode_and_f4_toggles() {
        use cadcraft_engine::cmd::osnap3d::mode3d;
        let mut app = CadApp::new(Session::new(), Services::default());
        assert_eq!(app.cmdline("3dosnap"), None);
        assert_eq!(app.ui.dialog.as_deref(), Some("dsettings:3dosnap"));
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, Vec::new());
        let shown: Vec<String> = frame(&mut app, &ctx, Vec::new()).into_iter().map(|(t, _)| t).collect();
        for label in
            ["3D Object Snap", "3D Object Snap On (F4)", "Vertex", "Midpoint on edge", "Center of face", "Knot", "Perpendicular", "Nearest to face"]
        {
            assert!(shown.iter().any(|t| t == label), "no {label} in {shown:?}");
        }
        click(&mut app, &ctx, "Clear All");
        assert_eq!(app.session.settings.osmode3d, mode3d::OFF);
        click(&mut app, &ctx, "Nearest to face");
        click(&mut app, &ctx, "3D Object Snap On (F4)");
        assert_eq!(app.session.settings.osmode3d, mode3d::NEAREST);
        assert_eq!(cadcraft_engine::sysvars::get(&app.session, "3DOSMODE"), Some(json!(mode3d::NEAREST)));
        app.ui.dialog = None;
        app.run("3dosnap", json!({})).unwrap();
        assert_eq!((app.ui.dialog.as_deref(), app.session.settings.osmode3d), (None, mode3d::OFF | mode3d::NEAREST));
    }

    /// Showing a tab changes nothing (no SETVAR per frame from rounding), and each variable the
    /// dialog edits is SETVAR's.
    #[test]
    fn drafting_settings_variables_round_trip_through_setvar() {
        let mut app = CadApp::new(Session::new(), Services::default());
        app.session.settings.polarang = 7.0_f64.to_radians();
        let before = app.session.settings.clone();
        let ctx = egui::Context::default();
        for tab in ["dsettings", "dsettings:polar", "dsettings:osnap", "dsettings:3dosnap", "dsettings:dyn"] {
            app.ui.dialog = Some(tab.into());
            frame(&mut app, &ctx, Vec::new());
            frame(&mut app, &ctx, Vec::new());
            assert_eq!(super::drafting_vars(&app.session.settings), super::drafting_vars(&before), "{tab}");
        }
        for (name, value) in super::drafting_vars(&before) {
            assert_eq!(cadcraft_engine::sysvars::get(&app.session, name), Some(value), "{name}");
        }
    }
}
