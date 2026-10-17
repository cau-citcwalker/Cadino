//! The Dimension Style Manager (DIMSTYLE, D, DST, DDIM; Format and Dimension ▸ Dimension
//! Style...): the styles in a list with Set Current, New, Rename and Delete, a preview, and the
//! selected style's settings on AutoCAD's tabs (Lines, Symbols and Arrows, Text, Fit, Primary
//! Units). It changes the drawing only through `dimstyle`, `dimstyle.current`, `dimstyle.rename`
//! and `dimstyle.delete`; -DIMSTYLE stays the command-line form.

use cadcraft_color::{Color, aci_rgb};
use cadcraft_doc::{DimKind, DimStyle, Dimension};
use cadcraft_geom::{Vec2, Vec3};
use egui::{Color32, RichText, Sense, Stroke, pos2, vec2};
use serde_json::{Map, Value, json};

use super::{Action, no_drawing, num_field, set_state, state};
use crate::CadApp;
use crate::theme::Tokens;

/// The dialog's own state until it is opened again.
#[derive(Clone, Default)]
struct DlgState {
    sel: String,
    tab: usize,
    new_name: String,
    /// The style a new one starts from (empty or unknown: the selected one).
    based_on: String,
    /// The new name while renaming the selected style.
    renaming: Option<String>,
}

/// DIMBLK values offered and their names in the arrowhead lists.
const ARROWS: &[(&str, &str)] = &[
    ("", "Closed filled"),
    ("_CLOSEDBLANK", "Closed blank"),
    ("_CLOSED", "Closed"),
    ("_DOT", "Dot"),
    ("_ARCHTICK", "Architectural tick"),
    ("_OBLIQUE", "Oblique tick"),
    ("_OPEN", "Open arrow"),
    ("_ORIGIN", "Origin indicator"),
    ("_ORIGIN2", "Origin indicator 2"),
    ("_OPEN90", "Right angle"),
    ("_OPEN30", "Open 30"),
    ("_DOTSMALL", "Dot small"),
    ("_DOTBLANK", "Dot blank"),
    ("_SMALL", "Dot small blank"),
    ("_BOXBLANK", "Box blank"),
    ("_BOXFILLED", "Box filled"),
    ("_DATUMBLANK", "Datum triangle"),
    ("_DATUMFILLED", "Datum triangle filled"),
    ("_INTEGRAL", "Integral"),
    ("_NONE", "None"),
];

/// The settings rows of one tab: each row reads the style and records the DIM variables the user
/// changed.
struct Form<'a> {
    name: &'a str,
    change: Map<String, Value>,
}

impl Form<'_> {
    fn set(&mut self, var: &str, v: Value) {
        self.change.insert(var.into(), v);
    }

    fn heading(ui: &mut egui::Ui, text: &str) {
        ui.label(RichText::new(text).strong());
        ui.end_row();
    }

    /// A size; negative values are refused (`positive`: zero too).
    fn num(&mut self, ui: &mut egui::Ui, label: &str, var: &'static str, v: f64, positive: bool) {
        ui.label(label);
        if let Some(x) = num_field(ui, ("dimstyle", self.name, var), v).filter(|x| if positive { *x > 0.0 } else { *x >= 0.0 }) {
            self.set(var, json!(x));
        }
        ui.end_row();
    }

    fn check(&mut self, ui: &mut egui::Ui, label: &str, var: &'static str, v: bool) {
        ui.label("");
        let mut b = v;
        if ui.checkbox(&mut b, label).changed() {
            self.set(var, json!(b));
        }
        ui.end_row();
    }

    fn color(&mut self, ui: &mut egui::Ui, label: &str, var: &'static str, c: Color) {
        ui.label(label);
        if let Some(c) = crate::layers::color_button(ui, egui::Id::new(("dimstyle", self.name, var)), c, true, 160.0) {
            self.set(var, json!(c.name()));
        }
        ui.end_row();
    }

    /// One of `options` (value, name); the picked value, if it changed.
    fn pick(ui: &mut egui::Ui, salt: (&str, &str), v: &Value, options: &[(Value, &str)]) -> Option<Value> {
        let shown = options.iter().find(|o| o.0 == *v).map_or_else(|| v.to_string(), |o| o.1.to_string());
        let mut picked = None;
        egui::ComboBox::from_id_salt(("dimstyle", salt)).selected_text(shown).width(170.0).show_ui(ui, |ui| {
            for (k, l) in options {
                if ui.selectable_label(k == v, *l).clicked() && k != v {
                    picked = Some(k.clone());
                }
            }
        });
        picked
    }

    fn choice(&mut self, ui: &mut egui::Ui, label: &str, var: &'static str, v: Value, options: &[(Value, &str)]) {
        ui.label(label);
        if let Some(k) = Self::pick(ui, (self.name, var), &v, options) {
            self.set(var, k);
        }
        ui.end_row();
    }

    /// An arrowhead list (DIMBLK, DIMLDRBLK).
    fn arrow(&mut self, ui: &mut egui::Ui, label: &str, var: &'static str, v: &str) -> bool {
        let kind = cadcraft_render::Arrowhead::parse(v);
        let shown = ARROWS.iter().find(|a| cadcraft_render::Arrowhead::parse(a.0) == kind).map_or(v, |a| a.0);
        let options: Vec<(Value, &str)> = ARROWS.iter().map(|(k, l)| (json!(k), crate::i18n::t(l))).collect();
        ui.label(label);
        let picked = Self::pick(ui, (self.name, var), &json!(shown), &options);
        ui.end_row();
        let changed = picked.is_some();
        if let Some(k) = picked {
            self.set(var, k);
        }
        changed
    }
}

/// Precision choices: 0 to 8 decimal places.
fn precisions() -> Vec<(Value, String)> {
    (0..=8u8).map(|n| (json!(n), if n == 0 { "0".to_string() } else { format!("0.{}", "0".repeat(usize::from(n))) })).collect()
}

fn tab_lines(ui: &mut egui::Ui, f: &mut Form, s: &DimStyle) {
    Form::heading(ui, crate::tl!("Dimension lines"));
    f.color(ui, crate::tl!("Color"), "DIMCLRD", s.dim_line_color);
    f.num(ui, crate::tl!("Extend beyond ticks"), "DIMDLE", s.dim_line_extend, false);
    f.num(ui, crate::tl!("Baseline spacing"), "DIMDLI", s.baseline_spacing, false);
    Form::heading(ui, crate::tl!("Extension lines"));
    f.color(ui, crate::tl!("Color"), "DIMCLRE", s.ext_line_color);
    f.check(ui, crate::tl!("Suppress ext line 1"), "DIMSE1", s.suppress_ext1);
    f.check(ui, crate::tl!("Suppress ext line 2"), "DIMSE2", s.suppress_ext2);
    f.num(ui, crate::tl!("Extend beyond dim lines"), "DIMEXE", s.ext_extend, false);
    f.num(ui, crate::tl!("Offset from origin"), "DIMEXO", s.ext_offset, false);
}

fn tab_arrows(ui: &mut egui::Ui, f: &mut Form, s: &DimStyle) {
    Form::heading(ui, crate::tl!("Arrowheads"));
    // One arrowhead for both ends: the per-end ones (DIMBLK1, DIMBLK2) follow it.
    let both = if s.arrow_block1.is_empty() && s.arrow_block2.is_empty() { s.arrow_block.as_str() } else { s.arrow_block1.as_str() };
    if f.arrow(ui, crate::tl!("First and second"), "DIMBLK", both) {
        f.set("DIMBLK1", json!(""));
        f.set("DIMBLK2", json!(""));
    }
    f.arrow(ui, crate::tl!("Leader"), "DIMLDRBLK", &s.leader_arrow_block);
    f.num(ui, crate::tl!("Arrow size"), "DIMASZ", s.arrow_size, false);
    Form::heading(ui, crate::tl!("Center marks"));
    // DIMCEN: 0 none, positive a mark, negative a mark with lines.
    let kind = if s.center_mark > 0.0 {
        1
    } else if s.center_mark < 0.0 {
        2
    } else {
        0
    };
    let size = if s.center_mark == 0.0 { DimStyle::default().center_mark } else { s.center_mark.abs() };
    let kinds = [(json!(0), crate::tl!("None")), (json!(1), crate::tl!("Mark")), (json!(2), crate::tl!("Line"))];
    ui.label(crate::tl!("Type"));
    if let Some(k) = Form::pick(ui, (f.name, "DIMCEN_TYPE"), &json!(kind), &kinds) {
        let sign = match k.as_i64() {
            Some(1) => 1.0,
            Some(2) => -1.0,
            _ => 0.0,
        };
        f.set("DIMCEN", json!(size * sign));
    }
    ui.end_row();
    ui.label(crate::tl!("Size"));
    if let Some(x) = num_field(ui, ("dimstyle", f.name, "DIMCEN"), size).filter(|x| *x > 0.0) {
        f.set("DIMCEN", json!(if kind == 2 { -x } else { x }));
    }
    ui.end_row();
}

fn tab_text(ui: &mut egui::Ui, f: &mut Form, s: &DimStyle, text_styles: &[String]) {
    Form::heading(ui, crate::tl!("Text appearance"));
    let styles: Vec<(Value, &str)> = text_styles.iter().map(|n| (json!(n), n.as_str())).collect();
    f.choice(ui, crate::tl!("Text style"), "DIMTXSTY", json!(s.text_style), &styles);
    f.color(ui, crate::tl!("Text color"), "DIMCLRT", s.text_color);
    f.num(ui, crate::tl!("Text height"), "DIMTXT", s.text_height, true);
    Form::heading(ui, crate::tl!("Text placement"));
    let vertical = [
        (json!(0), crate::tl!("Centered")),
        (json!(1), crate::tl!("Above")),
        (json!(2), crate::tl!("Outside")),
        (json!(3), crate::tl!("JIS")),
        (json!(4), crate::tl!("Below")),
    ];
    f.choice(ui, crate::tl!("Vertical position"), "DIMTAD", json!(s.text_above), &vertical);
    let horizontal = [
        (json!(0), crate::tl!("Centered")),
        (json!(1), crate::tl!("At Ext Line 1")),
        (json!(2), crate::tl!("At Ext Line 2")),
        (json!(3), crate::tl!("Over Ext Line 1")),
        (json!(4), crate::tl!("Over Ext Line 2")),
    ];
    f.choice(ui, crate::tl!("Horizontal position"), "DIMJUST", json!(s.text_just), &horizontal);
    f.num(ui, crate::tl!("Offset from dim line"), "DIMGAP", s.text_gap, false);
    Form::heading(ui, crate::tl!("Text alignment"));
    // DIMTIH and DIMTOH together.
    let align = match (s.text_inside_horizontal, s.text_outside_horizontal) {
        (true, _) => 0,
        (false, false) => 1,
        (false, true) => 2,
    };
    let aligns =
        [(json!(0), crate::tl!("Horizontal")), (json!(1), crate::tl!("Aligned with dimension line")), (json!(2), crate::tl!("ISO standard"))];
    ui.label(crate::tl!("Alignment"));
    if let Some(k) = Form::pick(ui, (f.name, "DIMTIH_TOH"), &json!(align), &aligns) {
        let (inside, outside) = match k.as_i64() {
            Some(1) => (false, false),
            Some(2) => (false, true),
            _ => (true, true),
        };
        f.set("DIMTIH", json!(inside));
        f.set("DIMTOH", json!(outside));
    }
    ui.end_row();
}

fn tab_fit(ui: &mut egui::Ui, f: &mut Form, s: &DimStyle) {
    Form::heading(ui, crate::tl!("Fit options"));
    let fits = [
        (json!(3), crate::tl!("Either text or arrows (best fit)")),
        (json!(1), crate::tl!("Arrows")),
        (json!(2), crate::tl!("Text")),
        (json!(0), crate::tl!("Both text and arrows")),
    ];
    f.choice(ui, crate::tl!("Move outside first"), "DIMATFIT", json!(s.fit), &fits);
    f.check(ui, crate::tl!("Always keep text between ext lines"), "DIMTIX", s.text_inside);
    f.check(ui, crate::tl!("Suppress arrows if they don't fit inside extension lines"), "DIMSOXD", s.suppress_outside_arrows);
    Form::heading(ui, crate::tl!("Scale for dimension features"));
    f.num(ui, crate::tl!("Use overall scale of"), "DIMSCALE", s.scale, false);
    Form::heading(ui, crate::tl!("Fine tuning"));
    f.check(ui, crate::tl!("Draw dim line between ext lines"), "DIMTOFL", s.force_line_inside);
}

fn tab_units(ui: &mut egui::Ui, f: &mut Form, s: &DimStyle) {
    Form::heading(ui, crate::tl!("Linear dimensions"));
    let units = [
        (json!(1), crate::tl!("Scientific")),
        (json!(2), crate::tl!("Decimal")),
        (json!(3), crate::tl!("Engineering")),
        (json!(4), crate::tl!("Architectural")),
        (json!(5), crate::tl!("Fractional")),
        (json!(6), crate::tl!("Windows Desktop")),
    ];
    f.choice(ui, crate::tl!("Unit format"), "DIMLUNIT", json!(s.linear_unit), &units);
    let labels = precisions();
    let precisions: Vec<(Value, &str)> = labels.iter().map(|(v, l)| (v.clone(), l.as_str())).collect();
    f.choice(ui, crate::tl!("Precision"), "DIMDEC", json!(s.decimals), &precisions);
    let separators = [(json!("."), crate::tl!("'.' (Period)")), (json!(","), crate::tl!("',' (Comma)")), (json!(" "), crate::tl!("' ' (Space)"))];
    f.choice(ui, crate::tl!("Decimal separator"), "DIMDSEP", json!(s.decimal_separator), &separators);
    f.num(ui, crate::tl!("Round off"), "DIMRND", s.round, false);
    f.num(ui, crate::tl!("Scale factor"), "DIMLFAC", s.linear_factor, true);
    // DIMZIN bits 4 (leading) and 8 (trailing).
    for (bit, label) in [(4u8, crate::tl!("Suppress leading zeros")), (8, crate::tl!("Suppress trailing zeros"))] {
        ui.label("");
        let mut on = s.zero_suppression & bit != 0;
        if ui.checkbox(&mut on, label).changed() {
            f.set("DIMZIN", json!(if on { s.zero_suppression | bit } else { s.zero_suppression & !bit }));
        }
        ui.end_row();
    }
    Form::heading(ui, crate::tl!("Angular dimensions"));
    let angular = [
        (json!(0), crate::tl!("Decimal Degrees")),
        (json!(1), crate::tl!("Degrees Minutes Seconds")),
        (json!(2), crate::tl!("Gradians")),
        (json!(3), crate::tl!("Radians")),
    ];
    f.choice(ui, crate::tl!("Units format"), "DIMAUNIT", json!(s.angular_unit), &angular);
    ui.label(crate::tl!("Precision"));
    if let Some(k) = Form::pick(ui, (f.name, "DIMADEC"), &json!(s.angular_decimals), &precisions) {
        f.set("DIMADEC", k);
    }
    ui.end_row();
}

/// The `dimstyle` parameters that make `name` a copy of `base`.
fn new_style_params(name: &str, base: &DimStyle) -> Value {
    let mut p = match serde_json::to_value(base) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    p.insert("name".into(), json!(name));
    p.insert("current".into(), json!(false));
    Value::Object(p)
}

/// A colour of the preview: index colour 7 and By* colours take the text colour of the theme.
pub(super) fn shown(c: Color, t: &Tokens) -> Color32 {
    match c {
        Color::Index(i) if i != 7 => {
            let r = aci_rgb(i);
            Color32::from_rgb(r.0, r.1, r.2)
        }
        Color::True(r) => Color32::from_rgb(r.0, r.1, r.2),
        _ => t.text,
    }
}

/// A horizontal linear dimension drawn with the style, fitted into `size`.
fn preview(ui: &mut egui::Ui, style: &DimStyle, size: egui::Vec2) {
    let t = Tokens::get();
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, t.canvas);
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    // Twelve text heights long, its line four text heights above the measured points.
    let unit = style.text_height.max(style.arrow_size) * style.effective_scale(1.0);
    if !(unit.is_finite() && unit > 0.0) {
        return;
    }
    let dim = Dimension {
        kind: DimKind::Linear { rotation: 0.0 },
        defpt: Vec3::new(unit * 12.0, unit * 4.0, 0.0),
        text_mid: Vec3::ZERO,
        p13: Vec3::ZERO,
        p14: Vec3::new(unit * 12.0, 0.0, 0.0),
        p15: Vec3::ZERO,
        p16: Vec3::ZERO,
        text: String::new(),
        style: style.name.clone(),
        measurement: 0.0,
        text_rotation: 0.0,
        user_text_pos: false,
        block: None,
        overrides: Default::default(),
        assoc: Vec::new(),
        marks: Default::default(),
    };
    let g = cadcraft_render::dimension_geometry(&dim, style, 1.0);
    let all = || g.lines.iter().chain(&g.fills).chain(&g.text).flatten().copied().filter(|q| q.is_finite());
    let (lo, hi) = all().fold((Vec2::new(0.0, 0.0), Vec2::new(unit * 12.0, unit * 4.0)), |(lo, hi), q| {
        (Vec2::new(lo.x.min(q.x), lo.y.min(q.y)), Vec2::new(hi.x.max(q.x), hi.y.max(q.y)))
    });
    let k = ((f64::from(rect.width()) - 24.0) / (hi.x - lo.x)).min((f64::from(rect.height()) - 24.0) / (hi.y - lo.y));
    if !(k.is_finite() && k > 0.0) {
        return;
    }
    let mid = (lo + hi) * 0.5;
    let c = rect.center();
    let to = |q: &Vec2| pos2(c.x + ((q.x - mid.x) * k) as f32, c.y - ((q.y - mid.y) * k) as f32);
    let dim_color = shown(style.dim_line_color, &t);
    for (l, role) in g.lines.iter().zip(&g.line_roles).take(200) {
        let color = if *role == cadcraft_render::LineRole::Ext { shown(style.ext_line_color, &t) } else { dim_color };
        p.add(egui::Shape::line(l.iter().map(to).collect(), Stroke::new(1.0, color)));
    }
    for tri in g.fills.iter().take(200) {
        p.add(egui::Shape::convex_polygon(tri.iter().map(to).collect(), dim_color, Stroke::NONE));
    }
    let text_color = shown(style.text_color, &t);
    for l in g.text.iter().take(500) {
        p.add(egui::Shape::line(l.iter().map(to).collect(), Stroke::new(1.0, text_color)));
    }
}

pub(super) fn dialog(app: &mut CadApp, ctx: &egui::Context, open: &mut bool) -> Action {
    let t = Tokens::get();
    let Ok(d) = app.session.doc() else {
        no_drawing(ctx, "Dimension Style Manager", open);
        return None;
    };
    let current = d.header.str("DIMSTYLE", "Standard");
    let mut styles = d.dim_styles.clone();
    if styles.is_empty() {
        styles.push(DimStyle::default());
    }
    let text_styles: Vec<String> = d.text_styles.iter().map(|s| s.name.clone()).collect();
    let mut st = state(ctx, "dimstyle_dlg", || DlgState { sel: current.clone(), ..Default::default() });
    if !styles.iter().any(|s| s.name == st.sel) {
        st.sel = styles.iter().find(|s| s.name.eq_ignore_ascii_case(&current)).map_or_else(|| current.clone(), |s| s.name.clone());
    }
    let mut action: Action = None;
    let mut close = false;
    egui::Window::new(crate::tl!("Dimension Style Manager"))
        .id(egui::Id::new("Dimension Style Manager"))
        .open(open)
        .default_size(vec2(760.0, 600.0))
        .resizable(true)
        .show(ctx, |ui| {
            ui.label(RichText::new(crate::tf!("Current dimension style: {name}", name = current)).color(t.text_dim));
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(210.0);
                    ui.label(RichText::new(crate::tl!("Styles")).strong());
                    egui::ScrollArea::vertical().id_salt("dimstyle_list").max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                        for s in &styles {
                            let mut label = RichText::new(&s.name);
                            if s.name.eq_ignore_ascii_case(&current) {
                                label = label.strong();
                            }
                            let r = ui.selectable_label(s.name == st.sel, label);
                            if r.clicked() && s.name != st.sel {
                                st.sel = s.name.clone();
                                st.renaming = None;
                            }
                            if r.double_clicked() {
                                action = Some(("dimstyle.current", json!({ "name": s.name })));
                            }
                        }
                    });
                    let is_current = st.sel.eq_ignore_ascii_case(&current);
                    let standard = st.sel.eq_ignore_ascii_case("Standard");
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!is_current, egui::Button::new(crate::tl!("Set Current"))).clicked() {
                            action = Some(("dimstyle.current", json!({ "name": st.sel })));
                        }
                        if ui.add_enabled(!standard, egui::Button::new(crate::tl!("Rename"))).clicked() {
                            st.renaming = Some(st.sel.clone());
                        }
                        if ui
                            .add_enabled(!standard && !is_current, egui::Button::new(crate::tl!("Delete")))
                            .on_disabled_hover_text(crate::tl!("Standard and the current style cannot be deleted"))
                            .clicked()
                        {
                            action = Some(("dimstyle.delete", json!({ "name": st.sel })));
                        }
                    });
                    if let Some(mut to) = st.renaming.take() {
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut to).desired_width(130.0));
                            let name = to.trim().to_string();
                            let ok = !name.is_empty() && !styles.iter().any(|s| s.name.eq_ignore_ascii_case(&name) && s.name != st.sel);
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.add_enabled(ok, egui::Button::new(crate::tl!("OK"))).clicked() || (enter && ok) {
                                action = Some(("dimstyle.rename", json!({ "from": st.sel, "to": name })));
                                st.sel = name;
                            } else if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                st.renaming = Some(to);
                            }
                        });
                    }
                    ui.separator();
                    ui.label(RichText::new(crate::tl!("Create New Dimension Style")).strong());
                    ui.add(egui::TextEdit::singleline(&mut st.new_name).hint_text(crate::tl!("New style name")).desired_width(190.0));
                    let base = styles.iter().find(|s| s.name == st.based_on).or_else(|| styles.iter().find(|s| s.name == st.sel));
                    ui.horizontal(|ui| {
                        ui.label(crate::tl!("Start With"));
                        let shown = base.map_or("", |b| b.name.as_str()).to_string();
                        egui::ComboBox::from_id_salt("dimstyle_base").selected_text(shown).width(120.0).show_ui(ui, |ui| {
                            for s in &styles {
                                if ui.selectable_label(base.is_some_and(|b| b.name == s.name), &s.name).clicked() {
                                    st.based_on = s.name.clone();
                                }
                            }
                        });
                    });
                    let name = st.new_name.trim().to_string();
                    let ok = !name.is_empty() && !styles.iter().any(|s| s.name.eq_ignore_ascii_case(&name));
                    if ui.add_enabled(ok, egui::Button::new(crate::tl!("New"))).clicked()
                        && let Some(b) = base
                    {
                        action = Some(("dimstyle", new_style_params(&name, b)));
                        st.sel = name;
                        st.new_name.clear();
                        st.based_on.clear();
                    }
                    ui.add_space(8.0);
                    if ui.button(crate::tl!("Close")).clicked() {
                        close = true;
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(style) = styles.iter().find(|s| s.name == st.sel) else { return };
                    ui.label(RichText::new(crate::tf!("Preview of: {name}", name = style.name)).strong());
                    preview(ui, style, vec2(ui.available_width().clamp(240.0, 480.0), 110.0));
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let tabs = [
                            crate::tl!("Lines"),
                            crate::tl!("Symbols and Arrows"),
                            crate::tl!("Text"),
                            crate::tl!("Fit"),
                            crate::tl!("Primary Units"),
                        ];
                        for (i, label) in tabs.iter().enumerate() {
                            if ui.selectable_label(st.tab == i, *label).clicked() {
                                st.tab = i;
                            }
                        }
                    });
                    ui.separator();
                    let mut form = Form { name: &style.name, change: Map::new() };
                    egui::ScrollArea::vertical().id_salt("dimstyle_tab").auto_shrink([false, true]).show(ui, |ui| {
                        egui::Grid::new(("dimstyle_grid", st.tab)).num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| match st.tab {
                            0 => tab_lines(ui, &mut form, style),
                            1 => tab_arrows(ui, &mut form, style),
                            2 => tab_text(ui, &mut form, style, &text_styles),
                            3 => tab_fit(ui, &mut form, style),
                            _ => tab_units(ui, &mut form, style),
                        });
                    });
                    if !form.change.is_empty() {
                        let mut p = form.change;
                        p.insert("name".into(), json!(style.name));
                        p.insert("current".into(), json!(false));
                        action = Some(("dimstyle", Value::Object(p)));
                    }
                });
            });
        });
    set_state(ctx, "dimstyle_dlg", st);
    if close {
        *open = false;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_engine::Session;

    #[test]
    fn a_new_style_copies_the_style_it_starts_with() {
        let mut s = Session::new();
        s.execute("dimstyle", &json!({ "name": "Arch", "DIMBLK": "_ARCHTICK", "DIMTXT": 3.5, "DIMLUNIT": 4, "current": false })).unwrap();
        let arch = s.doc().unwrap().dim_style("Arch").cloned().unwrap();
        // The current style (Standard) differs; New starting with Arch still copies Arch.
        s.execute("dimstyle", &new_style_params("Arch2", &arch)).unwrap();
        let d = s.doc().unwrap();
        let copy = d.dim_style("Arch2").cloned().unwrap();
        assert_eq!(DimStyle { name: "Arch".into(), ..copy }, arch);
        assert_eq!(d.header.str("DIMSTYLE", ""), "Standard", "New does not make it current");
    }

    #[test]
    fn every_arrowhead_name_is_one_the_renderer_draws() {
        for (k, _) in ARROWS {
            let a = cadcraft_render::Arrowhead::parse(k);
            assert!(k.is_empty() || a != cadcraft_render::Arrowhead::ClosedFilled, "{k}");
        }
    }

    #[test]
    fn the_preview_survives_hostile_styles() {
        let ctx = egui::Context::default();
        for st in [
            DimStyle::default(),
            DimStyle { text_height: f64::NAN, ..Default::default() },
            DimStyle { arrow_size: 1e300, scale: 1e300, ..Default::default() },
            DimStyle { text_height: 0.0, arrow_size: 0.0, ..Default::default() },
        ] {
            let out = ctx.run_ui(Default::default(), |ui| preview(ui, &st, vec2(300.0, 130.0)));
            out.drop_without_applying_deltas();
        }
    }
}
