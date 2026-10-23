//! Text tools: SCALETEXT (new heights about a base point), JUSTIFYTEXT (a new justification, the
//! text staying in place), TEXTALIGN (texts lined up along a line) and SPELL (not available).
//! They work on single-line text, attribute definitions and multiline text.

use cadcraft_doc::{AnnoDraw, Drawing, Entity, EntityKind, HAlign, Handle, MText, Text, VAlign};
use cadcraft_geom::{Mat3, Vec2};
use serde_json::{Value, json};

use super::curves::KW;
use super::machines::{SelOutcome, SelectPhase, number};
use super::*;
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// The justifications JUSTIFYTEXT offers, as keywords.
const JUSTIFY: &[&str] = &["Left", "Align", "Fit", "Center", "Middle", "Right", "TL", "TC", "TR", "ML", "MC", "MR", "BL", "BC", "BR"];
/// SCALETEXT's base point options.
const BASES: &[&str] = &["Existing", "Left", "Center", "Middle", "Right", "TL", "TC", "TR", "ML", "MC", "MR", "BL", "BC", "BR"];

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("scaletext", "Scale Text", run_scaletext)
            .params("{handles?, height? (model height) | paperHeight? (annotative objects only) | match?: hex (another text's height) | factor?, base?: existing|L|C|M|R|TL|TC|TR|ML|MC|MR|BL|BC|BR (default existing: each object's own insertion point)} → {changed}")
            .interactive(|_| Ok(Box::new(ScaleTextM::default()))),
        CommandSpec::new("justifytext", "Justify Text", run_justifytext)
            .params("{handles?, justify: L|A|F|C|M|R|TL|TC|TR|ML|MC|MR|BL|BC|BR} → {changed}: the text stays where it is")
            .interactive(|_| Ok(Box::new(JustifyM::default()))),
        CommandSpec::new("textalign", "Text Align", run_textalign)
            .alias(&["ta"])
            .params("{handles (the texts to align), to?: hex (the text to align to; default the first of `handles`) | from?: [x,y], through: [x,y] (the line's second point), mode?: vertical (each keeps its height on the page) | horizontal | distribute (evenly between the points) | spacing (`spacing` apart), spacing?, alignment?: left|center|right (default left)} → {moved}")
            .interactive(|_| Ok(Box::new(TextAlignM::default()))),
        CommandSpec::new("spell", "Spelling", |_, _| {
            Err(EngineError::Other("SPELL is not available yet: Cadino has no spelling dictionary; FIND finds and replaces text".into()))
        })
        .alias(&["sp"])
        .params("not available yet")
        .noundo(),
    ]
}

fn other(m: impl Into<String>) -> EngineError {
    EngineError::Other(m.into())
}

// ---------------- geometry of single-line text ----------------

/// The factor an annotative object is drawn at (1 for others).
fn drawn(d: &Drawing, e: &Entity) -> f64 {
    match d.anno_draw(e) {
        AnnoDraw::Scaled(f) if f.is_finite() && f > 0.0 => f,
        _ => 1.0,
    }
}

/// Where the glyphs start from a text's alignment point, for a text `w` wide and `ht` high (as
/// the renderer places it).
fn offset(h: HAlign, v: VAlign, w: f64, ht: f64) -> Vec2 {
    let dx = match h {
        HAlign::Left | HAlign::Aligned | HAlign::Fit => 0.0,
        HAlign::Center | HAlign::Middle => -w / 2.0,
        HAlign::Right => -w,
    };
    let dy = match (h, v) {
        (HAlign::Middle, _) | (_, VAlign::Middle) => -ht / 2.0,
        (_, VAlign::Baseline) => 0.0,
        (_, VAlign::Bottom) => ht / 3.0,
        (_, VAlign::Top) => -ht,
    };
    Vec2::new(dx, dy)
}

/// A text's baseline: its start, direction, length, and the height and width factor it shows at
/// (an aligned or fit text's come from its two points).
struct Baseline {
    start: Vec2,
    rot: f64,
    width: f64,
    height: f64,
    wf: f64,
    /// The style's backwards and upside-down flags, as -1 factors.
    mirror: Vec2,
}

impl Baseline {
    fn of(d: &Drawing, t: &Text) -> Baseline {
        let st = d.text_style(&t.style).cloned().unwrap_or_default();
        let font = cadcraft_fonts::TextFont::resolve(&st.font);
        let mirror = Vec2::new(if st.backwards { -1.0 } else { 1.0 }, if st.upside_down { -1.0 } else { 1.0 });
        let (mut height, mut wf) = (t.height, t.width_factor);
        if matches!(t.halign, HAlign::Aligned | HAlign::Fit)
            && let Some(p2) = t.align_pt
        {
            let (a, b) = (t.insert.xy(), p2.xy());
            let len = a.dist(b);
            let w = cadcraft_fonts::text_width(&font, &t.value, height, wf);
            if w > 1e-12 && len > 1e-12 {
                if t.halign == HAlign::Aligned {
                    height *= len / w;
                } else {
                    wf *= len / w;
                }
            }
            return Baseline { start: a, rot: if len > 1e-12 { a.angle_to(b) } else { t.rotation }, width: len, height, wf, mirror };
        }
        let width = cadcraft_fonts::text_width(&font, &t.value, height, wf);
        let b = Baseline { start: Vec2::ZERO, rot: t.rotation, width, height, wf, mirror };
        Baseline { start: t.justify_point() + b.local(t.halign, t.valign), ..b }
    }
    /// From the alignment point of justification `h`/`v` to the start of the baseline.
    fn local(&self, h: HAlign, v: VAlign) -> Vec2 {
        let o = offset(h, v, self.width, self.height);
        Vec2::new(o.x * self.mirror.x, o.y * self.mirror.y).rotate(self.rot)
    }
}

/// `t` with justification `h`/`v`, its glyphs where they were: the new alignment point is where
/// that justification falls on the text (an aligned or fit text gets its two baseline points).
pub(crate) fn rejustify(d: &Drawing, t: &Text, h: HAlign, v: VAlign) -> Text {
    let b = Baseline::of(d, t);
    let z = t.insert.z;
    let mut n = Text { halign: h, valign: v, height: b.height, width_factor: b.wf, rotation: b.rot, insert: b.start.to3(z), ..t.clone() };
    n.align_pt = match (h, v) {
        (HAlign::Left, VAlign::Baseline) => None,
        (HAlign::Aligned | HAlign::Fit, _) => Some((b.start + Vec2::from_angle(b.rot) * b.width).to3(z)),
        _ => Some((b.start - b.local(h, v)).to3(z)),
    };
    n
}

/// [`rejustify`] as `e` is drawn (an annotative text at the current scale).
fn rejustify_drawn(d: &Drawing, e: &Entity, t: &Text, h: HAlign, v: VAlign) -> Text {
    let f = drawn(d, e);
    let mut n = rejustify(d, &Text { height: t.height * f, ..t.clone() }, h, v);
    n.height /= f;
    n
}

// ---------------- multiline text ----------------

/// The box of a multiline text: its width (the defined width, else the text's) and height.
fn mtext_box(d: &Drawing, t: &MText) -> (f64, f64) {
    let flat = MText { rotation: 0.0, ..t.clone() };
    let b = cadcraft_render::layout_mtext_entity(d, &flat).bounds;
    let empty = b.is_empty() || !b.width().is_finite();
    let w = if t.width > 0.0 {
        t.width
    } else if empty {
        0.0
    } else {
        b.width()
    };
    (w, if empty { t.height } else { b.height() })
}

/// Where attachment `a` (1..=9: TL TC TR ML MC MR BL BC BR) is in a box `w` × `h`, from its top left.
fn attach_at(a: u8, w: f64, h: f64) -> Vec2 {
    let i = a.clamp(1, 9) - 1;
    Vec2::new(w * f64::from(i % 3) / 2.0, -h * f64::from(i / 3) / 2.0)
}

/// `t` attached at `a`, the text staying in place.
fn reattach(d: &Drawing, t: &MText, a: u8) -> MText {
    let (w, h) = mtext_box(d, t);
    let shift = (attach_at(a, w, h) - attach_at(t.attach, w, h)).rotate(t.rotation);
    let mut n = t.clone();
    n.attach = a;
    n.insert.x += shift.x;
    n.insert.y += shift.y;
    n
}

/// A justification code for multiline text: the single-line ones go to the top row.
// ponytail: Left/Align/Fit → TL, Center → TC, Right → TR, Middle → MC (MTEXT has no baseline).
fn mtext_attach(code: &str) -> Option<u8> {
    match code.trim().to_ascii_uppercase().as_str() {
        "L" | "LEFT" | "A" | "ALIGN" | "F" | "FIT" => Some(1),
        "C" | "CENTER" => Some(2),
        "R" | "RIGHT" => Some(3),
        "M" | "MIDDLE" => Some(5),
        c => super::mtext_opts::attach_code(c),
    }
}

fn is_text(k: &EntityKind) -> bool {
    matches!(k, EntityKind::Text(_) | EntityKind::AttDef(_) | EntityKind::MText(_))
}

/// The texts among `hs` not on a locked layer, as they are now.
fn texts(s: &Session, hs: &[Handle]) -> Result<Vec<Entity>> {
    let d = s.doc()?;
    Ok(hs.iter().filter_map(|h| d.entity(*h)).filter(|e| is_text(&e.kind) && !super::curves::is_locked(s, e.handle)).map(|e| (**e).clone()).collect())
}

fn replace_kinds(s: &mut Session, new: Vec<(Handle, EntityKind)>) -> Result<usize> {
    let d = s.doc_mut()?;
    let n = new.len();
    for (h, k) in new {
        d.modify_entity(h, |e| e.kind = k)?;
    }
    Ok(n)
}

fn changed(n: usize) -> Value {
    json!({ "changed": n, "message": format!("{n} object{} changed.", if n == 1 { "" } else { "s" }) })
}

// ---------------- JUSTIFYTEXT ----------------

fn justified(d: &Drawing, e: &Entity, code: &str) -> Option<EntityKind> {
    let j = super::draw::justify(code);
    Some(match &e.kind {
        EntityKind::Text(t) => EntityKind::Text(rejustify_drawn(d, e, t, j?.0, j?.1)),
        EntityKind::AttDef(a) => {
            let mut a = a.clone();
            a.text = rejustify_drawn(d, e, &a.text, j?.0, j?.1);
            EntityKind::AttDef(a)
        }
        EntityKind::MText(t) => EntityKind::MText(reattach(d, t, mtext_attach(code)?)),
        _ => return None,
    })
}

fn run_justifytext(s: &mut Session, p: &Value) -> Result<Value> {
    let code = str_param(p, "justify").ok_or_else(|| bad("justifytext", "`justify` (L, A, F, C, M, R, TL … BR) is required"))?;
    if super::draw::justify(code).is_none() {
        return Err(bad("justifytext", format!("unknown justification `{code}`")));
    }
    let ents = texts(s, &targets(s, p)?)?;
    let d = s.doc()?;
    let new: Vec<(Handle, EntityKind)> = ents.iter().filter_map(|e| Some((e.handle, justified(d, e, code)?))).collect();
    Ok(changed(replace_kinds(s, new)?))
}

/// JUSTIFYTEXT typed: objects, then the justification.
#[derive(Default)]
struct JustifyM {
    sel: SelectPhase,
    picked: Option<Vec<Handle>>,
}

impl Interactive for JustifyM {
    fn name(&self) -> &'static str {
        "JUSTIFYTEXT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.picked = Some(self.sel.picked.clone());
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.picked {
            None => self.sel.prompt(),
            Some(_) => Prompt::new("Enter a justification option", KW).kw(JUSTIFY).default("Left"),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        let Some(hs) = self.picked.clone() else {
            return Ok(match self.sel.feed(s, &i)? {
                SelOutcome::More => Step::Continue,
                SelOutcome::Empty => Step::Done,
                SelOutcome::Done(hs) => {
                    self.picked = Some(hs);
                    Step::Continue
                }
            });
        };
        let code = match i {
            Input::Keyword(k) => k,
            Input::Enter => "Left".into(),
            Input::Text(_) => return Err(other("Invalid option keyword.")),
            _ => return Ok(Step::Continue),
        };
        let r = run_justifytext(s, &json!({ "handles": hs.iter().map(|h| h.hex()).collect::<Vec<_>>(), "justify": code }))?;
        s.echo(r["message"].as_str().unwrap_or_default().to_string());
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}

// ---------------- SCALETEXT ----------------

/// How SCALETEXT sets the heights.
#[derive(Clone, Copy, Debug, PartialEq)]
enum How {
    /// The height objects show at (an annotative one at the current annotation scale).
    Model(f64),
    /// The paper height of annotative objects; others are left as they are.
    Paper(f64),
    Factor(f64),
}

fn stored_height(k: &EntityKind) -> Option<f64> {
    match k {
        EntityKind::Text(t) => Some(t.height),
        EntityKind::AttDef(a) => Some(a.text.height),
        EntityKind::MText(t) => Some(t.height),
        _ => None,
    }
}

/// The height `e` shows at.
fn model_height(d: &Drawing, e: &Entity) -> Option<f64> {
    Some(stored_height(&e.kind)? * drawn(d, e))
}

/// The single-line text `t` (of `e`) at `height`, scaled about base point `base` (its own
/// alignment point when `None`).
fn scaled_text(d: &Drawing, e: &Entity, t: &Text, height: f64, base: Option<(HAlign, VAlign)>) -> Text {
    let f = drawn(d, e);
    let own = (t.halign, t.valign);
    let (bh, bv) = base.unwrap_or(if matches!(t.halign, HAlign::Aligned | HAlign::Fit) { (HAlign::Left, VAlign::Baseline) } else { own });
    let shown = Text { height: t.height * f, ..t.clone() };
    let mut at_base = rejustify(d, &shown, bh, bv);
    let k = if t.height > 0.0 { height / t.height } else { 1.0 };
    at_base.height *= k;
    let mut n = rejustify(d, &at_base, own.0, own.1);
    n.height /= f;
    n
}

/// The multiline text `t` at `height`, scaled about attachment point `base` (its own when `None`).
fn scaled_mtext(d: &Drawing, t: &MText, height: f64, base: Option<u8>) -> MText {
    let k = if t.height > 0.0 { height / t.height } else { 1.0 };
    let mut n = MText { height, ..t.clone() };
    if let Some(b) = base.filter(|b| *b != t.attach) {
        let (w, h) = mtext_box(d, t);
        let (w2, h2) = (if t.width > 0.0 { w } else { w * k }, h * k);
        let fixed = attach_at(b, w, h) - attach_at(t.attach, w, h);
        let now = attach_at(b, w2, h2) - attach_at(t.attach, w2, h2);
        let shift = (fixed - now).rotate(t.rotation);
        n.insert.x += shift.x;
        n.insert.y += shift.y;
    }
    n
}

fn scaled(d: &Drawing, e: &Entity, how: How, base: Option<&str>) -> Option<EntityKind> {
    let stored = stored_height(&e.kind)?;
    let annotative = cadcraft_doc::is_annotative(e);
    let height = match how {
        How::Model(h) => h / drawn(d, e),
        How::Paper(h) if annotative => h,
        How::Paper(_) => return None,
        How::Factor(k) => stored * k,
    };
    if !(height.is_finite() && height > 0.0) {
        return None;
    }
    let j = base.and_then(super::draw::justify);
    Some(match &e.kind {
        EntityKind::Text(t) => EntityKind::Text(scaled_text(d, e, t, height, j)),
        EntityKind::AttDef(a) => {
            let mut a = a.clone();
            a.text = scaled_text(d, e, &a.text, height, j);
            EntityKind::AttDef(a)
        }
        EntityKind::MText(t) => EntityKind::MText(scaled_mtext(d, t, height, base.and_then(mtext_attach))),
        _ => return None,
    })
}

fn run_scaletext(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "scaletext";
    let base = match str_param(p, "base") {
        None => None,
        Some(b) if b.eq_ignore_ascii_case("existing") || b.eq_ignore_ascii_case("e") => None,
        Some(b) if matches!(b.to_ascii_uppercase().as_str(), "A" | "ALIGN" | "F" | "FIT") || super::draw::justify(b).is_none() => {
            return Err(bad(CMD, format!("`base` must be existing, L, C, M, R, TL, TC, TR, ML, MC, MR, BL, BC or BR, not `{b}`")));
        }
        Some(b) => Some(b.to_string()),
    };
    let how = if let Some(h) = size_param(CMD, p, "height", false)? {
        How::Model(h)
    } else if let Some(h) = size_param(CMD, p, "paperHeight", false)? {
        How::Paper(h)
    } else if let Some(k) = size_param(CMD, p, "factor", false)? {
        How::Factor(k)
    } else if let Some(v) = p.get("match") {
        let h = super::curves::handle_value(v).ok_or_else(|| bad(CMD, "`match` must be an object handle"))?;
        let d = s.doc()?;
        let e = d.entity(h).ok_or_else(|| bad(CMD, "`match`: no such object"))?;
        How::Model(model_height(d, e).ok_or_else(|| bad(CMD, "`match` must be a text object"))?)
    } else {
        return Err(bad(CMD, "give `height`, `paperHeight`, `match` or `factor`"));
    };
    let ents = texts(s, &targets(s, p)?)?;
    let d = s.doc()?;
    let new: Vec<(Handle, EntityKind)> = ents.iter().filter_map(|e| Some((e.handle, scaled(d, e, how, base.as_deref())?))).collect();
    Ok(changed(replace_kinds(s, new)?))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum ScaleAsk {
    #[default]
    Select,
    Base,
    Height,
    Paper,
    Match,
    Factor,
    RefLength,
    NewLength(f64),
}

/// SCALETEXT typed: objects, the base point option, then a model height, Paper height, Match
/// object or Scale factor (with Reference).
#[derive(Default)]
struct ScaleTextM {
    ask: ScaleAsk,
    sel: SelectPhase,
    picked: Vec<Handle>,
    base: Option<String>,
}

impl ScaleTextM {
    fn apply(&self, s: &mut Session, how: Value) -> Result<Step> {
        let mut p = json!({ "handles": self.picked.iter().map(|h| h.hex()).collect::<Vec<_>>() });
        if let (Some(o), Some(h)) = (p.as_object_mut(), how.as_object()) {
            o.extend(h.clone());
            if let Some(b) = &self.base {
                o.insert("base".into(), json!(b));
            }
        }
        let r = run_scaletext(s, &p)?;
        s.echo(r["message"].as_str().unwrap_or_default().to_string());
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
    /// The first picked text's model height, offered as the default.
    fn current(&self, s: &Session) -> f64 {
        let d = s.doc().ok();
        d.and_then(|d| self.picked.iter().filter_map(|h| d.entity(*h)).find_map(|e| model_height(d, e)))
            .unwrap_or_else(|| d.map_or(0.2, |d| d.header.f64("TEXTSIZE", 0.2)))
    }
}

fn value(t: &str, positive: bool) -> Result<f64> {
    number(t).filter(|v| v.is_finite() && (*v > 0.0 || !positive && *v >= 0.0)).ok_or_else(|| other("Requires a positive value."))
}

impl Interactive for ScaleTextM {
    fn name(&self) -> &'static str {
        "SCALETEXT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.picked = texts(s, &self.sel.picked)?.iter().map(|e| e.handle).collect();
            self.ask = ScaleAsk::Base;
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let f = |v: f64| format!("{v:.4}");
        match self.ask {
            ScaleAsk::Select => self.sel.prompt(),
            ScaleAsk::Base => Prompt::new("Enter a base point option for scaling", KW).kw(BASES).default("Existing"),
            ScaleAsk::Height => Prompt::new("Specify new model height", Accept::NUMBER)
                .kw(&["Paper height", "Match object", "Scale factor"])
                .default(f(self.current(s))),
            ScaleAsk::Paper => Prompt::new("Specify new paper height", Accept::NUMBER).default(f(self.current(s))),
            ScaleAsk::Match => Prompt::new("Select a text object with the desired height", Accept::POINT),
            ScaleAsk::Factor => Prompt::new("Specify scale factor", Accept::NUMBER).kw(&["Reference"]).default(f(2.0)),
            ScaleAsk::RefLength => Prompt::new("Specify reference length", Accept::NUMBER).default(f(1.0)),
            ScaleAsk::NewLength(_) => Prompt::new("Specify new length", Accept::NUMBER),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match (self.ask, i) {
            (ScaleAsk::Select, i) => match self.sel.feed(s, &i)? {
                SelOutcome::More => {}
                SelOutcome::Empty => return Ok(Step::Done),
                SelOutcome::Done(hs) => {
                    self.picked = texts(s, &hs)?.iter().map(|e| e.handle).collect();
                    if self.picked.is_empty() {
                        s.echo("No text objects selected.");
                        return Ok(Step::Done);
                    }
                    self.ask = ScaleAsk::Base;
                }
            },
            (ScaleAsk::Base, Input::Keyword(k)) => {
                self.base = (k != "Existing").then_some(k);
                self.ask = ScaleAsk::Height;
            }
            (ScaleAsk::Base, Input::Enter) => self.ask = ScaleAsk::Height,
            (ScaleAsk::Height, Input::Keyword(k)) => {
                self.ask = match k.as_str() {
                    "Paper height" => ScaleAsk::Paper,
                    "Match object" => ScaleAsk::Match,
                    _ => ScaleAsk::Factor,
                }
            }
            (ScaleAsk::Height, Input::Text(t)) => return self.apply(s, json!({ "height": value(&t, true)? })),
            (ScaleAsk::Height, Input::Enter) => return self.apply(s, json!({ "height": self.current(s) })),
            (ScaleAsk::Paper, Input::Text(t)) => return self.apply(s, json!({ "paperHeight": value(&t, true)? })),
            (ScaleAsk::Paper, Input::Enter) => return self.apply(s, json!({ "paperHeight": self.current(s) })),
            (ScaleAsk::Match, Input::Point(p)) => {
                let h = super::curves::pick_at(s, p).filter(|h| s.doc().is_ok_and(|d| d.entity(*h).is_some_and(|e| is_text(&e.kind))));
                let h = h.ok_or_else(|| other("Select a text object."))?;
                return self.apply(s, json!({ "match": h.hex() }));
            }
            (ScaleAsk::Factor, Input::Keyword(_)) => self.ask = ScaleAsk::RefLength,
            (ScaleAsk::Factor, Input::Text(t)) => return self.apply(s, json!({ "factor": value(&t, true)? })),
            (ScaleAsk::Factor, Input::Enter) => return self.apply(s, json!({ "factor": 2.0 })),
            (ScaleAsk::RefLength, Input::Text(t)) => self.ask = ScaleAsk::NewLength(value(&t, true)?),
            (ScaleAsk::RefLength, Input::Enter) => self.ask = ScaleAsk::NewLength(1.0),
            (ScaleAsk::NewLength(r), Input::Text(t)) => return self.apply(s, json!({ "factor": value(&t, true)? / r })),
            (_, Input::Text(_)) => return Err(other("Invalid option keyword.")),
            _ => {}
        }
        Ok(Step::Continue)
    }
}

// ---------------- TEXTALIGN ----------------

/// A text's alignment point (a multiline text's insertion point).
fn anchor(k: &EntityKind) -> Option<Vec2> {
    match k {
        EntityKind::Text(t) => Some(t.justify_point()),
        EntityKind::AttDef(a) => Some(a.text.justify_point()),
        EntityKind::MText(t) => Some(t.insert.xy()),
        _ => None,
    }
}

/// `e` justified to column `col` (0 left, 1 centre, 2 right), its row kept, in place.
fn in_column(d: &Drawing, e: &Entity, col: usize) -> Option<EntityKind> {
    let h = [HAlign::Left, HAlign::Center, HAlign::Right].get(col).copied()?;
    let v = |t: &Text| match t.halign {
        HAlign::Middle => VAlign::Middle,
        HAlign::Aligned | HAlign::Fit => VAlign::Baseline,
        _ => t.valign,
    };
    Some(match &e.kind {
        EntityKind::Text(t) => EntityKind::Text(rejustify_drawn(d, e, t, h, v(t))),
        EntityKind::AttDef(a) => {
            let mut a = a.clone();
            a.text = rejustify_drawn(d, e, &a.text, h, v(&a.text));
            EntityKind::AttDef(a)
        }
        EntityKind::MText(t) => EntityKind::MText(reattach(d, t, (t.attach.clamp(1, 9) - 1) / 3 * 3 + col as u8 + 1)),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum AlignMode {
    Vertical,
    Horizontal,
    Distribute,
    Spacing(f64),
}

fn run_textalign(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "textalign";
    let col = match str_param(p, "alignment").map(str::to_ascii_lowercase).as_deref() {
        None | Some("left" | "l") => 0,
        Some("center" | "c") => 1,
        Some("right" | "r") => 2,
        Some(a) => return Err(bad(CMD, format!("`alignment` must be left, center or right, not `{a}`"))),
    };
    let mode = match str_param(p, "mode").map(str::to_ascii_lowercase).as_deref() {
        None | Some("vertical") => AlignMode::Vertical,
        Some("horizontal") => AlignMode::Horizontal,
        Some("distribute") => AlignMode::Distribute,
        Some("spacing") => AlignMode::Spacing(size_param(CMD, p, "spacing", false)?.ok_or_else(|| bad(CMD, "`spacing` (positive) is required"))?),
        Some(m) => return Err(bad(CMD, format!("`mode` must be vertical, horizontal, distribute or spacing, not `{m}`"))),
    };
    let through = point_req(CMD, p, "through")?;
    let ents = texts(s, &targets(s, p)?)?;
    let to = match p.get("to") {
        Some(v) => Some(super::curves::handle_value(v).ok_or_else(|| bad(CMD, "`to` must be an object handle"))?),
        None if point_param(p, "from").is_some() => None,
        None => Some(ents.first().map(|e| e.handle).ok_or_else(|| bad(CMD, "select the texts to align (`handles`)"))?),
    };
    let d = s.doc()?;
    // The texts in the alignment chosen, in place; the one aligned to stays where it is.
    let mut moved: Vec<(Handle, EntityKind)> =
        ents.iter().filter(|e| Some(e.handle) != to).filter_map(|e| Some((e.handle, in_column(d, e, col)?))).collect();
    let from = match to {
        Some(h) => {
            let e = d.entity(h).filter(|e| is_text(&e.kind)).ok_or_else(|| bad(CMD, "`to` must be a text object"))?;
            anchor(&in_column(d, e, col).ok_or_else(|| bad(CMD, "`to` must be a text object"))?).unwrap_or_default()
        }
        None => point_req(CMD, p, "from")?,
    };
    let dir = (through - from).normalized();
    if dir == Vec2::ZERO {
        return Err(bad(CMD, "the second point must differ from the first"));
    }
    // Along the line from the first point, in order.
    let along = |k: &EntityKind| anchor(k).map_or(0.0, |a| (a - from).dot(dir));
    moved.sort_by(|a, b| along(&a.1).total_cmp(&along(&b.1)));
    let n = moved.len();
    for (i, (_, k)) in moved.iter_mut().enumerate() {
        let Some(a) = anchor(k) else { continue };
        let target = match mode {
            AlignMode::Vertical if dir.y.abs() > 1e-9 => Vec2::new(from.x + dir.x * (a.y - from.y) / dir.y, a.y),
            AlignMode::Horizontal if dir.x.abs() > 1e-9 => Vec2::new(a.x, from.y + dir.y * (a.x - from.x) / dir.x),
            AlignMode::Vertical | AlignMode::Horizontal => {
                return Err(bad(CMD, "the line runs the way the texts keep: pick a second point off it, or Distribute"));
            }
            AlignMode::Distribute => from + (through - from) * ((i + 1) as f64 / n as f64),
            AlignMode::Spacing(sp) => from + dir * (sp * (i + 1) as f64),
        };
        k.transform(&Mat3::translate(target - a));
    }
    let n = replace_kinds(s, moved)?;
    Ok(json!({ "moved": n, "message": format!("{n} text object{} aligned.", if n == 1 { "" } else { "s" }) }))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum AlignAsk {
    #[default]
    Select,
    Alignment,
    Options,
    Spacing,
    Reference,
    FirstPoint,
    Second,
}

/// TEXTALIGN typed: the texts (Alignment and Options can be set there), the text to align to (or
/// two Points), then the second point.
// ponytail: Set spacing measures between alignment points, not text extents.
#[derive(Default)]
struct TextAlignM {
    ask: AlignAsk,
    back: AlignAsk,
    sel: SelectPhase,
    picked: Vec<Handle>,
    col: usize,
    mode: Option<AlignMode>,
    to: Option<Handle>,
    from: Option<Vec2>,
}

impl TextAlignM {
    fn mode(&self) -> AlignMode {
        self.mode.unwrap_or(AlignMode::Vertical)
    }
    fn apply(&self, s: &mut Session, through: Vec2) -> Result<Step> {
        let (mode, spacing) = match self.mode() {
            AlignMode::Vertical => ("vertical", None),
            AlignMode::Horizontal => ("horizontal", None),
            AlignMode::Distribute => ("distribute", None),
            AlignMode::Spacing(sp) => ("spacing", Some(sp)),
        };
        let alignment = ["left", "center", "right"].get(self.col).copied().unwrap_or("left");
        let mut p = json!({
            "handles": self.picked.iter().map(|h| h.hex()).collect::<Vec<_>>(),
            "through": [through.x, through.y],
            "mode": mode,
            "alignment": alignment,
        });
        if let Some(o) = p.as_object_mut() {
            match (self.to, self.from) {
                (Some(h), _) => o.insert("to".into(), json!(h.hex())),
                (None, Some(f)) => o.insert("from".into(), json!([f.x, f.y])),
                _ => None,
            };
            if let Some(sp) = spacing {
                o.insert("spacing".into(), json!(sp));
            }
        }
        let r = run_textalign(s, &p)?;
        s.echo(r["message"].as_str().unwrap_or_default().to_string());
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
    fn base(&self, s: &Session) -> Option<Vec2> {
        self.from.or_else(|| s.doc().ok()?.entity(self.to?).and_then(|e| anchor(&e.kind)))
    }
}

impl Interactive for TextAlignM {
    fn name(&self) -> &'static str {
        "TEXTALIGN"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            self.picked = texts(s, &self.sel.picked)?.iter().map(|e| e.handle).collect();
            self.ask = AlignAsk::Reference;
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.ask {
            AlignAsk::Select => Prompt::new(if self.sel.removing { "Remove objects" } else { "Select text objects to align" }, Accept::SELECT)
                .kw(&["Alignment", "Options"]),
            AlignAsk::Alignment => Prompt::new("Enter an option", KW)
                .kw(&["Left", "Center", "Right"])
                .default(["Left", "Center", "Right"].get(self.col).copied().unwrap_or("Left")),
            AlignAsk::Options => Prompt::new("Enter an option", KW)
                .kw(&["Distribute", "Set spacing", "Current vertical", "Current horizontal"])
                .default(match self.mode() {
                    AlignMode::Vertical => "Current vertical",
                    AlignMode::Horizontal => "Current horizontal",
                    AlignMode::Distribute => "Distribute",
                    AlignMode::Spacing(_) => "Set spacing",
                }),
            AlignAsk::Spacing => Prompt::new("Specify spacing", Accept::NUMBER),
            AlignAsk::Reference => Prompt::new("Select text object to align to", Accept::POINT).kw(&["Points"]),
            AlignAsk::FirstPoint => Prompt::new("Specify first point", Accept::POINT),
            AlignAsk::Second => Prompt::new("Pick second point", Accept::POINT).kw(&["Options"]).base_opt(self.base(s)),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        match (self.ask, i) {
            (AlignAsk::Select, Input::Keyword(k)) if k == "Alignment" || k == "Options" => {
                self.back = AlignAsk::Select;
                self.ask = if k == "Alignment" { AlignAsk::Alignment } else { AlignAsk::Options };
            }
            (AlignAsk::Select, i) => match self.sel.feed(s, &i)? {
                SelOutcome::More => {}
                SelOutcome::Empty => return Ok(Step::Done),
                SelOutcome::Done(hs) => {
                    self.picked = texts(s, &hs)?.iter().map(|e| e.handle).collect();
                    if self.picked.is_empty() {
                        s.echo("No text objects selected.");
                        return Ok(Step::Done);
                    }
                    self.ask = AlignAsk::Reference;
                }
            },
            (AlignAsk::Alignment, i) => {
                if let Input::Keyword(k) = &i {
                    self.col = match k.as_str() {
                        "Center" => 1,
                        "Right" => 2,
                        _ => 0,
                    };
                }
                self.ask = self.back;
            }
            (AlignAsk::Options, i) => {
                self.ask = self.back;
                if let Input::Keyword(k) = &i {
                    match k.as_str() {
                        "Distribute" => self.mode = Some(AlignMode::Distribute),
                        "Current horizontal" => self.mode = Some(AlignMode::Horizontal),
                        "Current vertical" => self.mode = Some(AlignMode::Vertical),
                        _ => self.ask = AlignAsk::Spacing,
                    }
                }
            }
            (AlignAsk::Spacing, Input::Text(t)) => {
                self.mode = Some(AlignMode::Spacing(value(&t, true)?));
                self.ask = self.back;
            }
            (AlignAsk::Spacing, Input::Enter) => self.ask = self.back,
            (AlignAsk::Reference, Input::Keyword(_)) => self.ask = AlignAsk::FirstPoint,
            (AlignAsk::Reference, Input::Point(p)) => {
                let h = super::curves::pick_at(s, p).filter(|h| s.doc().is_ok_and(|d| d.entity(*h).is_some_and(|e| is_text(&e.kind))));
                self.to = Some(h.ok_or_else(|| other("Select a text object."))?);
                self.ask = AlignAsk::Second;
            }
            (AlignAsk::FirstPoint, Input::Point(p)) => {
                self.from = Some(p);
                self.ask = AlignAsk::Second;
            }
            (AlignAsk::Second, Input::Keyword(_)) => {
                self.back = AlignAsk::Second;
                self.ask = AlignAsk::Options;
            }
            (AlignAsk::Second, Input::Point(p)) => return self.apply(s, p),
            (_, Input::Text(_)) => return Err(other("Invalid input.")),
            _ => {}
        }
        Ok(Step::Continue)
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_doc::{EntityKind, HAlign, Handle, MText, Text, VAlign};
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::Session;

    fn add(s: &mut Session, p: serde_json::Value) -> Handle {
        Handle::parse_hex(s.execute("text", &p).unwrap()["handle"].as_str().unwrap()).unwrap()
    }
    fn text(s: &Session, h: Handle) -> Text {
        match &s.doc().unwrap().entity(h).unwrap().kind {
            EntityKind::Text(t) => t.clone(),
            other => panic!("{other:?}"),
        }
    }
    /// The text's drawn box, as the renderer places it.
    fn bounds(s: &Session, h: Handle) -> cadcraft_geom::Bounds2 {
        let t = text(s, h);
        cadcraft_render::place_text_entity(s.doc().unwrap(), &t, &t.value).1
    }
    fn near(a: Vec2, b: Vec2) -> bool {
        a.near(b, 1e-6)
    }

    #[test]
    fn text_tools_leave_texts_on_locked_layers_alone() {
        let mut s = Session::new();
        let h = add(&mut s, json!({"at": [10, 5], "text": "Hello", "height": 2}));
        s.execute("layer.set", &json!({"name": "0", "locked": true})).unwrap();
        let before = text(&s, h);
        for (cmd, p) in [("scaletext", json!({"height": 5})), ("justifytext", json!({"justify": "MC"}))] {
            let mut p = p;
            p["handles"] = json!([h.hex()]);
            let _ = s.execute(cmd, &p);
            assert_eq!(text(&s, h), before, "{cmd}");
        }
    }

    #[test]
    fn justifytext_keeps_the_text_in_place() {
        let mut s = Session::new();
        let h = add(&mut s, json!({"at": [10, 5], "text": "Hello", "height": 2, "rotation": 30}));
        let before = bounds(&s, h);
        for j in ["MC", "R", "TL", "BR", "M", "F", "A", "L", "C"] {
            s.execute("justifytext", &json!({"handles": [h.hex()], "justify": j})).unwrap();
            let after = bounds(&s, h);
            assert!(near(after.min, before.min) && near(after.max, before.max), "{j}: {before:?} → {after:?}");
        }
        let t = text(&s, h);
        assert_eq!((t.halign, t.valign), (HAlign::Center, VAlign::Baseline));
        assert!((t.height - 2.0).abs() < 1e-9);
        assert!(s.execute("justifytext", &json!({"handles": [h.hex()], "justify": "XX"})).is_err());

        // Multiline text: the attachment changes, the box stays.
        let m = Handle::parse_hex(
            s.execute("mtext", &json!({"at": [0, 0], "text": "One\nTwo", "height": 1, "width": 10})).unwrap()["handle"].as_str().unwrap(),
        )
        .unwrap();
        s.execute("justifytext", &json!({"handles": [m.hex()], "justify": "BR"})).unwrap();
        let EntityKind::MText(t) = &s.doc().unwrap().entity(m).unwrap().kind else { panic!() };
        let MText { attach, insert, .. } = t.clone();
        assert_eq!(attach, 9);
        assert!((insert.x - 10.0).abs() < 1e-9 && insert.y < -1.0, "{insert:?}");

        // Typed: objects, then the option.
        s.set_selection(vec![h]);
        s.cmdline("justifytext").unwrap();
        assert!(
            s.prompt_text().ends_with("Enter a justification option [Left/Align/Fit/Center/Middle/Right/TL/TC/TR/ML/MC/MR/BL/BC/BR] <Left>:"),
            "{}",
            s.prompt_text()
        );
        s.cmdline("tr").unwrap();
        assert!(s.running.is_none());
        assert_eq!(text(&s, h).halign, HAlign::Right);
        assert_eq!(text(&s, h).valign, VAlign::Top);
    }

    #[test]
    fn scaletext_heights_about_base_points() {
        let mut s = Session::new();
        let a = add(&mut s, json!({"at": [0, 0], "text": "AB", "height": 1}));
        let b = add(&mut s, json!({"at": [0, 5], "text": "CD", "height": 2, "justify": "TR"}));
        // Existing: about each one's own insertion point.
        s.execute("scaletext", &json!({"handles": [a.hex(), b.hex()], "height": 3})).unwrap();
        assert_eq!(text(&s, a).height, 3.0);
        assert_eq!(text(&s, a).insert.xy(), Vec2::ZERO);
        assert_eq!(text(&s, b).justify_point(), Vec2::new(0.0, 5.0));
        // A base point option keeps that point of each text and the text's own justification.
        let before = bounds(&s, a);
        s.execute("scaletext", &json!({"handles": [a.hex()], "factor": 0.5, "base": "TR"})).unwrap();
        let after = bounds(&s, a);
        assert!(near(after.max, before.max), "{before:?} → {after:?}");
        assert_eq!(text(&s, a).halign, HAlign::Left);
        assert!((text(&s, a).height - 1.5).abs() < 1e-9);
        // Match object; paper height leaves non-annotative text alone.
        s.execute("scaletext", &json!({"handles": [a.hex()], "match": b.hex()})).unwrap();
        assert!((text(&s, a).height - 3.0).abs() < 1e-9);
        assert_eq!(s.execute("scaletext", &json!({"handles": [a.hex()], "paperHeight": 7})).unwrap()["changed"], 0);
        assert!(s.execute("scaletext", &json!({"handles": [a.hex()], "height": -1})).is_err());
        assert!(s.execute("scaletext", &json!({"handles": [a.hex()], "factor": 2, "base": "Fit"})).is_err());

        // Typed: objects, base option, then Scale factor with Reference.
        s.set_selection(vec![a]);
        s.cmdline("scaletext").unwrap();
        assert!(
            s.prompt_text()
                .ends_with("Enter a base point option for scaling or [Existing/Left/Center/Middle/Right/TL/TC/TR/ML/MC/MR/BL/BC/BR] <Existing>:"),
            "{}",
            s.prompt_text()
        );
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Specify new model height or [Paper height/Match object/Scale factor] <3.0000>:"), "{}", s.prompt_text());
        s.cmdline("s r 2 3").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        assert!((text(&s, a).height - 4.5).abs() < 1e-9);
    }

    #[test]
    fn textalign_lines_texts_up() {
        let mut s = Session::new();
        let r = add(&mut s, json!({"at": [0, 10], "text": "Ref", "height": 1}));
        let a = add(&mut s, json!({"at": [3, 7], "text": "A", "height": 1}));
        let b = add(&mut s, json!({"at": [-2, 4], "text": "B", "height": 1, "justify": "R"}));
        // Current vertical along a vertical line: each keeps its y, x goes to the line; left aligned.
        s.execute("textalign", &json!({"handles": [r.hex(), a.hex(), b.hex()], "through": [0, 0]})).unwrap();
        assert_eq!(text(&s, a).justify_point(), Vec2::new(0.0, 7.0));
        assert!(near(text(&s, b).justify_point(), Vec2::new(0.0, 4.0)));
        assert_eq!(text(&s, b).halign, HAlign::Left);
        assert_eq!(text(&s, r).justify_point(), Vec2::new(0.0, 10.0), "the text aligned to stays");
        // Distribute evenly between the two points.
        s.execute("textalign", &json!({"handles": [a.hex(), b.hex()], "to": r.hex(), "through": [0, 0], "mode": "distribute"})).unwrap();
        assert!(near(text(&s, a).justify_point(), Vec2::new(0.0, 5.0)));
        assert!(near(text(&s, b).justify_point(), Vec2::new(0.0, 0.0)));
        // Spacing from two points.
        s.execute("textalign", &json!({"handles": [a.hex(), b.hex()], "from": [20, 0], "through": [30, 0], "mode": "spacing", "spacing": 4}))
            .unwrap();
        assert!(near(text(&s, a).justify_point(), Vec2::new(24.0, 0.0)) || near(text(&s, b).justify_point(), Vec2::new(24.0, 0.0)));
        assert!(
            s.execute("textalign", &json!({"handles": [a.hex()], "from": [0, 0], "through": [5, 0]})).is_err(),
            "vertical mode on a horizontal line"
        );

        // Typed as TA: texts, the text to align to, the second point.
        s.cmdline("ta").unwrap();
        assert!(s.prompt_text().ends_with("Select text objects to align or [Alignment/Options]:"), "{}", s.prompt_text());
        s.input(crate::Input::Pick(vec![a, b])).unwrap();
        s.input(crate::Input::Enter).unwrap();
        assert!(s.prompt_text().ends_with("Select text object to align to or [Points]:"), "{}", s.prompt_text());
        s.cmdline("p 0,0").unwrap();
        assert!(s.prompt_text().ends_with("Pick second point or [Options]:"), "{}", s.prompt_text());
        s.cmdline("o d 0,10").unwrap();
        assert!(s.running.is_none(), "{}", s.prompt_text());
        assert!(near(text(&s, a).justify_point(), Vec2::new(0.0, 5.0)) && near(text(&s, b).justify_point(), Vec2::new(0.0, 10.0)));
    }

    #[test]
    fn spell_is_refused_clearly() {
        let mut s = Session::new();
        let e = s.cmdline("sp").unwrap_err().to_string();
        assert!(e.contains("SPELL is not available"), "{e}");
    }
}
