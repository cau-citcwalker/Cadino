//! Changing attached images: IMAGECLIP (a rectangular or polygonal clip boundary, ON, OFF,
//! Delete), IMAGEADJUST (brightness, contrast, fade), TRANSPARENCY (a picture's transparent
//! pixels show what is under it), and the drawing's IMAGEFRAME (0 no frames, 1 shown and plotted,
//! 2 shown but not plotted) and IMAGEQUALITY (High: blended pixels, Draft: the nearest).

use cadcraft_doc::{EntityKind, Handle, IMAGE_TRANSPARENT, Image};
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use crate::cmd::machines::{SelOutcome, SelectPhase, number};
use crate::cmd::{CommandSpec, bad, curves, points_param, str_param, targets};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

/// Most clip boundary points.
const MAX_CLIP: usize = 10_000;
/// [`Image::display`] bit: the clip boundary is used.
const USE_CLIP: u16 = 4;

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("imageclip", "Clip Image", run_imageclip)
            .menu(&["Modify", "Clip", "Image"])
            .alias(&["icl"])
            .params("{handles? (images; default the selection), option?: new|on|off|delete (default new), points?: [[x,y]…] (new: two = a rectangle's opposite corners, three or more = a polygon)} → {handles}")
            .interactive(|_| Ok(Box::new(ClipM::default()))),
        CommandSpec::new("imageadjust", "Adjust...", run_imageadjust)
            .menu(&["Modify", "Object", "Image", "Adjust..."])
            .alias(&["iad"])
            .params("{handles? (images; default the selection), brightness?, contrast?, fade? (0..100; brightness and contrast 50 = as the file)} → {handles}")
            .interactive(|_| Ok(Box::new(AdjustM::default()))),
        CommandSpec::new("-imageadjust", "Adjust (command line)", run_imageadjust)
            .params("as IMAGEADJUST")
            .interactive(|_| Ok(Box::new(AdjustM::default()))),
        CommandSpec::new("imageframe", "Frame", run_imageframe)
            .menu(&["Modify", "Object", "Image", "Frame"])
            .params("{value?: 0 (frames hidden) | 1 (shown and plotted) | 2 (shown, not plotted)} → {IMAGEFRAME}")
            .interactive(|_| Ok(Box::new(SettingM::Frame))),
        CommandSpec::new("imagequality", "Quality", run_imagequality)
            .menu(&["Modify", "Object", "Image", "Quality"])
            .params("{quality?: high|draft} → {IMAGEQUALITY: 1 high | 0 draft}")
            .interactive(|_| Ok(Box::new(SettingM::Quality))),
        CommandSpec::new("transparency", "Transparency", run_transparency)
            .menu(&["Modify", "Object", "Image", "Transparency"])
            .params("{handles? (images; default the selection), on: bool (a picture's transparent pixels show what is under it)} → {handles}")
            .interactive(|_| Ok(Box::new(TransparencyM::default()))),
    ]
}

/// The images among `hs`, leaving out those on locked layers.
fn images(s: &Session, hs: &[Handle]) -> Vec<Handle> {
    let Ok(d) = s.doc() else { return Vec::new() };
    hs.iter().copied().filter(|h| d.entity(*h).is_some_and(|e| matches!(e.kind, EntityKind::Image(_))) && !curves::is_locked(s, *h)).collect()
}

fn hex(hs: &[Handle]) -> Vec<String> {
    hs.iter().map(|h| h.hex()).collect()
}

/// The images a JSON call names; an error when there are none.
fn chosen(s: &Session, cmd: &str, p: &Value) -> Result<Vec<Handle>> {
    let hs = images(s, &targets(s, p)?);
    if hs.is_empty() {
        return Err(bad(cmd, "no image given (pass `handles` or select images)"));
    }
    Ok(hs)
}

fn change(s: &mut Session, hs: &[Handle], f: impl Fn(&mut Image)) -> Result<()> {
    let d = s.doc_mut()?;
    for h in hs {
        d.modify_entity(*h, |e| {
            if let EntityKind::Image(i) = &mut e.kind {
                f(i);
            }
        })
        .map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(())
}

fn image(s: &Session, h: Handle) -> Option<Image> {
    match &s.doc().ok()?.entity(h)?.kind {
        EntityKind::Image(i) => Some(i.clone()),
        _ => None,
    }
}

// ---------- IMAGECLIP ----------

/// A boundary given in the drawing as `i`'s clip boundary (pixel coordinates): a rectangle stays
/// two corners while the image is square to the axes, else becomes its corners.
fn to_pixels(i: &Image, world: &[Vec2]) -> Option<Vec<Vec2>> {
    let square = i.u.y.abs() < 1e-12 && i.v.x.abs() < 1e-12;
    let pts: Vec<Vec2> = match world {
        [a, b] if !square => vec![*a, Vec2::new(b.x, a.y), *b, Vec2::new(a.x, b.y)],
        vs => vs.to_vec(),
    };
    let px: Option<Vec<Vec2>> = pts.iter().map(|p| i.world_to_pixel(*p)).collect();
    match px?.as_slice() {
        [a, b] => Some(vec![Vec2::new(a.x.min(b.x), a.y.min(b.y)), Vec2::new(a.x.max(b.x), a.y.max(b.y))]),
        vs => Some(vs.to_vec()),
    }
}

fn run_imageclip(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "imageclip";
    let hs = chosen(s, cmd, p)?;
    let option = str_param(p, "option").unwrap_or("new").trim().to_ascii_lowercase();
    match option.as_str() {
        "new" | "n" | "new boundary" => {
            let pts = points_param(p, "points").ok_or_else(|| bad(cmd, "`points` (two corners or a polygon's points) is required"))?;
            let pts = crate::cmd::distinct_points(&pts);
            let ok = match pts.as_slice() {
                [a, b] => (a.x - b.x).abs() > 1e-9 && (a.y - b.y).abs() > 1e-9,
                vs => vs.len() >= 3 && vs.len() <= MAX_CLIP,
            };
            if !ok {
                return Err(bad(cmd, format!("`points`: two corners of a rectangle with an area, or 3 to {MAX_CLIP} polygon points")));
            }
            let mut out = Vec::new();
            for h in &hs {
                let Some(i) = image(s, *h) else { continue };
                if let Some(clip) = to_pixels(&i, &pts) {
                    out.push((*h, clip));
                }
            }
            if out.is_empty() {
                return Err(bad(cmd, "the images have no area to clip"));
            }
            for (h, clip) in out {
                change(s, &[h], |i| {
                    i.clip = clip.clone();
                    i.clipping = true;
                    i.display |= USE_CLIP;
                })?;
            }
        }
        "on" | "off" => {
            let on = option == "on";
            let clipped: Vec<Handle> = hs.iter().copied().filter(|h| image(s, *h).is_some_and(|i| i.clip.len() >= 2)).collect();
            if clipped.is_empty() {
                return Err(bad(cmd, "the images have no clip boundary"));
            }
            change(s, &clipped, |i| i.clipping = on)?;
            return Ok(json!({ "handles": hex(&clipped) }));
        }
        "delete" | "d" => change(s, &hs, |i| {
            i.clip.clear();
            i.clipping = false;
        })?,
        o => return Err(bad(cmd, format!("unknown option `{}` (new, on, off or delete)", o.chars().take(20).collect::<String>()))),
    }
    Ok(json!({ "handles": hex(&hs) }))
}

fn keyword(i: &Input) -> Option<String> {
    match i {
        Input::Keyword(k) | Input::Text(k) => Some(k.trim().to_ascii_lowercase()),
        _ => None,
    }
}

#[derive(Default)]
enum CStage {
    #[default]
    Select,
    Option,
    DeleteOld,
    Kind,
    Corner(Option<Vec2>),
    Polygon(Vec<Vec2>),
}

/// IMAGECLIP typed at the command line.
#[derive(Default)]
struct ClipM {
    image: Option<Handle>,
    stage: CStage,
}

impl ClipM {
    fn run(&self, s: &mut Session, mut p: Value) -> Result<Value> {
        if let (Some(o), Some(h)) = (p.as_object_mut(), self.image) {
            o.insert("handles".into(), json!([h.hex()]));
        }
        run_imageclip(s, &p)
    }
    fn clipped(&self, s: &Session) -> bool {
        self.image.and_then(|h| image(s, h)).is_some_and(|i| i.clip.len() >= 2)
    }
}

impl Interactive for ClipM {
    fn name(&self) -> &'static str {
        "IMAGECLIP"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if s.settings.pickfirst
            && let [h] = images(s, &s.selection()).as_slice()
        {
            self.image = Some(*h);
            self.stage = CStage::Option;
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.stage {
            CStage::Select => Prompt::new("Select image to clip", Accept::POINT),
            CStage::Option => {
                Prompt::new("Enter image clipping option", Accept::WORD).kw(&["ON", "OFF", "Delete", "New boundary"]).default("New boundary")
            }
            CStage::DeleteOld => Prompt::new("Delete old boundary?", Accept::WORD).kw(&["No", "Yes"]).default("Yes"),
            CStage::Kind => Prompt::new("Enter clipping type", Accept::WORD).kw(&["Polygonal", "Rectangular"]).default("Rectangular"),
            CStage::Corner(None) => Prompt::new("Specify first corner point", Accept::POINT),
            CStage::Corner(Some(a)) => Prompt::new("Specify opposite corner point", Accept::POINT).corner(*a),
            CStage::Polygon(vs) if vs.is_empty() => Prompt::new("Specify first point", Accept::POINT),
            CStage::Polygon(vs) => Prompt::new("Specify next point", Accept::POINT).kw(&["Undo"]).base_opt(vs.last().copied()),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match std::mem::take(&mut self.stage) {
            CStage::Select => match i {
                Input::Point(p) => {
                    match curves::pick_at(s, p).filter(|h| !images(s, &[*h]).is_empty()) {
                        Some(h) => {
                            self.image = Some(h);
                            self.stage = CStage::Option;
                        }
                        None => {
                            s.echo("Select an image.");
                            self.stage = CStage::Select;
                        }
                    }
                    Ok(Step::Continue)
                }
                Input::Enter | Input::Cancel => Ok(Step::Done),
                _ => {
                    self.stage = CStage::Select;
                    Ok(Step::Continue)
                }
            },
            CStage::Option => {
                let k = if matches!(i, Input::Enter) { Some("new boundary".to_string()) } else { keyword(&i) };
                match k.as_deref() {
                    Some(o @ ("on" | "off")) => {
                        if self.clipped(s) {
                            self.run(s, json!({ "option": o }))?;
                        } else {
                            s.echo("The image has no clip boundary.");
                        }
                        Ok(Step::Done)
                    }
                    Some("delete" | "d") => {
                        self.run(s, json!({ "option": "delete" }))?;
                        Ok(Step::Done)
                    }
                    Some("new boundary" | "new" | "n") => {
                        self.stage = if self.clipped(s) { CStage::DeleteOld } else { CStage::Kind };
                        Ok(Step::Continue)
                    }
                    _ => {
                        self.stage = CStage::Option;
                        Err(EngineError::Other("Requires an option keyword.".into()))
                    }
                }
            }
            CStage::DeleteOld => match (matches!(i, Input::Enter), keyword(&i).as_deref()) {
                (true, _) | (_, Some("yes" | "y")) => {
                    self.stage = CStage::Kind;
                    Ok(Step::Continue)
                }
                (_, Some("no" | "n")) => Ok(Step::Done),
                _ => {
                    self.stage = CStage::DeleteOld;
                    Err(EngineError::Other("Requires Yes or No.".into()))
                }
            },
            CStage::Kind => {
                let k = if matches!(i, Input::Enter) { Some("rectangular".to_string()) } else { keyword(&i) };
                self.stage = match k.as_deref() {
                    Some("rectangular" | "r") => CStage::Corner(None),
                    Some("polygonal" | "p") => CStage::Polygon(Vec::new()),
                    _ => {
                        self.stage = CStage::Kind;
                        return Err(EngineError::Other("Requires an option keyword.".into()));
                    }
                };
                Ok(Step::Continue)
            }
            CStage::Corner(first) => match (first, i) {
                (None, Input::Point(a)) => {
                    self.stage = CStage::Corner(Some(a));
                    Ok(Step::Continue)
                }
                (Some(a), Input::Point(b)) => match self.run(s, json!({ "option": "new", "points": [[a.x, a.y], [b.x, b.y]] })) {
                    Ok(_) => Ok(Step::Done),
                    Err(e) => {
                        self.stage = CStage::Corner(Some(a));
                        Err(e)
                    }
                },
                (first, _) => {
                    self.stage = CStage::Corner(first);
                    Ok(Step::Continue)
                }
            },
            CStage::Polygon(mut vs) => match i {
                Input::Point(q) => {
                    if !vs.last().is_some_and(|l| l.near(q, 1e-12)) && vs.len() < MAX_CLIP {
                        vs.push(q);
                    }
                    self.stage = CStage::Polygon(vs);
                    Ok(Step::Continue)
                }
                Input::Keyword(_) | Input::Text(_) if matches!(keyword(&i).as_deref(), Some("undo" | "u")) => {
                    vs.pop();
                    self.stage = CStage::Polygon(vs);
                    Ok(Step::Continue)
                }
                Input::Enter if vs.len() >= 3 => {
                    let pts: Vec<[f64; 2]> = vs.iter().map(|v| [v.x, v.y]).collect();
                    self.run(s, json!({ "option": "new", "points": pts }))?;
                    Ok(Step::Done)
                }
                Input::Enter => {
                    self.stage = CStage::Polygon(vs);
                    Err(EngineError::Other("A polygonal boundary needs at least three points.".into()))
                }
                _ => {
                    self.stage = CStage::Polygon(vs);
                    Ok(Step::Continue)
                }
            },
        }
    }
    fn preview(&self, _s: &Session, cursor: Vec2) -> Vec<EntityKind> {
        let pts = match &self.stage {
            CStage::Corner(Some(a)) => vec![*a, Vec2::new(cursor.x, a.y), cursor, Vec2::new(a.x, cursor.y), *a],
            CStage::Polygon(vs) if !vs.is_empty() => {
                let mut v = vs.clone();
                v.push(cursor);
                v.extend(vs.first().copied());
                v
            }
            _ => return Vec::new(),
        };
        vec![crate::cmd::helpers::lwpoly(pts.into_iter().map(cadcraft_geom::PolyVertex::new).collect(), false)]
    }
}

// ---------- IMAGEADJUST ----------

/// A 0..=100 setting of a JSON call, when given.
fn percent(cmd: &str, p: &Value, key: &str) -> Result<Option<u8>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite() && (0.0..=100.0).contains(x))
            .map(|x| Some(x.round() as u8))
            .ok_or_else(|| bad(cmd, format!("`{key}` must be a number from 0 to 100"))),
    }
}

fn run_imageadjust(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "imageadjust";
    let (b, c, f) = (percent(cmd, p, "brightness")?, percent(cmd, p, "contrast")?, percent(cmd, p, "fade")?);
    if b.is_none() && c.is_none() && f.is_none() {
        return Err(bad(cmd, "`brightness`, `contrast` or `fade` is required"));
    }
    let hs = chosen(s, cmd, p)?;
    change(s, &hs, |i| {
        i.brightness = b.unwrap_or(i.brightness);
        i.contrast = c.unwrap_or(i.contrast);
        i.fade = f.unwrap_or(i.fade);
    })?;
    Ok(json!({ "handles": hex(&hs) }))
}

#[derive(Default)]
enum JStage {
    #[default]
    Select,
    Option,
    Value(&'static str),
}

/// IMAGEADJUST typed at the command line (-IMAGEADJUST's prompts).
/// ponytail: no Image Adjust dialog with its preview; typed IMAGEADJUST asks on the command line.
#[derive(Default)]
struct AdjustM {
    sel: SelectPhase,
    images: Vec<Handle>,
    stage: JStage,
}

impl AdjustM {
    fn picked(&mut self, s: &mut Session, hs: Vec<Handle>) -> Step {
        self.images = images(s, &hs);
        if self.images.is_empty() {
            s.echo("No images selected.");
            return Step::Done;
        }
        self.stage = JStage::Option;
        Step::Continue
    }
    fn current(&self, s: &Session, what: &str) -> u8 {
        let i = self.images.first().and_then(|h| image(s, *h)).unwrap_or_default();
        match what {
            "contrast" => i.contrast,
            "fade" => i.fade,
            _ => i.brightness,
        }
    }
}

impl Interactive for AdjustM {
    fn name(&self) -> &'static str {
        "IMAGEADJUST"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            return Ok(self.picked(s, hs));
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.stage {
            JStage::Select => Prompt { message: "Select image(s)".into(), ..self.sel.prompt() },
            JStage::Option => {
                Prompt::new("Enter image adjustment option", Accept::WORD).kw(&["Contrast", "Fade", "Brightness"]).default("Brightness")
            }
            JStage::Value(what) => Prompt::new(format!("Enter {what} value (0-100)"), Accept::NUMBER).default(self.current(s, what).to_string()),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match self.stage {
            JStage::Select => match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => Ok(self.picked(s, hs)),
            },
            JStage::Option => {
                let k = if matches!(i, Input::Enter) { Some("brightness".to_string()) } else { keyword(&i) };
                self.stage = JStage::Value(match k.as_deref() {
                    Some("brightness" | "b") => "brightness",
                    Some("contrast" | "c") => "contrast",
                    Some("fade" | "f") => "fade",
                    _ => return Err(EngineError::Other("Requires an option keyword.".into())),
                });
                Ok(Step::Continue)
            }
            JStage::Value(what) => {
                let v = match &i {
                    Input::Enter => return Ok(Step::Done),
                    Input::Text(t) => number(t).filter(|v| (0.0..=100.0).contains(v)),
                    _ => return Ok(Step::Continue),
                };
                let v = v.ok_or_else(|| EngineError::Other("Requires an integer between 0 and 100.".into()))?;
                let mut p = json!({ "handles": hex(&self.images) });
                p[what] = json!(v);
                run_imageadjust(s, &p)?;
                s.set_selection(Vec::new());
                Ok(Step::Done)
            }
        }
    }
}

// ---------- IMAGEFRAME, IMAGEQUALITY ----------

fn run_imageframe(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(v) = p.get("value").filter(|v| !v.is_null()) {
        let v = v.as_i64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())).filter(|v| (0..=2).contains(v));
        let v = v.ok_or_else(|| bad("imageframe", "`value` must be 0, 1 or 2"))?;
        s.doc_mut()?.header.set_i64("IMAGEFRAME", v);
    }
    Ok(json!({ "IMAGEFRAME": s.doc()?.header.i64("IMAGEFRAME", 1) }))
}

fn run_imagequality(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(q) = str_param(p, "quality") {
        let v = match q.trim().to_ascii_lowercase().as_str() {
            "high" | "h" | "1" => 1,
            "draft" | "d" | "0" => 0,
            _ => return Err(bad("imagequality", "`quality` must be high or draft")),
        };
        s.doc_mut()?.header.set_i64("IMAGEQUALITY", v);
    }
    Ok(json!({ "IMAGEQUALITY": s.doc()?.header.i64("IMAGEQUALITY", 1) }))
}

/// IMAGEFRAME and IMAGEQUALITY typed: one setting.
enum SettingM {
    Frame,
    Quality,
}

impl Interactive for SettingM {
    fn name(&self) -> &'static str {
        match self {
            SettingM::Frame => "IMAGEFRAME",
            SettingM::Quality => "IMAGEQUALITY",
        }
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let h = |k: &str, d: i64| s.doc().map(|doc| doc.header.i64(k, d)).unwrap_or(d);
        match self {
            SettingM::Frame => Prompt::new("Enter image frame setting", Accept::WORD).kw(&["0", "1", "2"]).default(h("IMAGEFRAME", 1).to_string()),
            SettingM::Quality => Prompt::new("Enter image quality setting", Accept::WORD)
                .kw(&["High", "Draft"])
                .default(if h("IMAGEQUALITY", 1) == 0 { "Draft" } else { "High" }),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Some(k) = keyword(&i) else { return Ok(if matches!(i, Input::Enter | Input::Cancel) { Step::Done } else { Step::Continue }) };
        let r = match self {
            SettingM::Frame => run_imageframe(s, &json!({ "value": k })),
            SettingM::Quality => run_imagequality(s, &json!({ "quality": k })),
        };
        r.map_err(|_| EngineError::Other("Invalid option keyword.".into()))?;
        Ok(Step::Done)
    }
}

// ---------- TRANSPARENCY ----------

fn run_transparency(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "transparency";
    let on = match (p.get("on").and_then(Value::as_bool), str_param(p, "mode").map(|m| m.trim().to_ascii_lowercase())) {
        (Some(b), _) => b,
        (None, Some(m)) if m == "on" => true,
        (None, Some(m)) if m == "off" => false,
        _ => return Err(bad(cmd, "`on` (true or false) is required")),
    };
    let hs = chosen(s, cmd, p)?;
    change(s, &hs, |i| {
        if on {
            i.display |= IMAGE_TRANSPARENT;
        } else {
            i.display &= !IMAGE_TRANSPARENT;
        }
    })?;
    Ok(json!({ "handles": hex(&hs) }))
}

/// TRANSPARENCY typed at the command line.
#[derive(Default)]
struct TransparencyM {
    sel: SelectPhase,
    images: Option<Vec<Handle>>,
}

impl TransparencyM {
    fn picked(&mut self, s: &mut Session, hs: Vec<Handle>) -> Step {
        let hs = images(s, &hs);
        if hs.is_empty() {
            s.echo("No images selected.");
            return Step::Done;
        }
        self.images = Some(hs);
        Step::Continue
    }
}

impl Interactive for TransparencyM {
    fn name(&self) -> &'static str {
        "TRANSPARENCY"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.sel = SelectPhase::begin(s);
        if self.sel.done {
            let hs = self.sel.picked.clone();
            return Ok(self.picked(s, hs));
        }
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match &self.images {
            None => Prompt { message: "Select image(s)".into(), ..self.sel.prompt() },
            Some(hs) => {
                let on = hs.first().and_then(|h| image(s, *h)).is_some_and(|i| i.transparent());
                Prompt::new("Enter transparency mode", Accept::WORD).kw(&["ON", "OFF"]).default(if on { "ON" } else { "OFF" })
            }
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let Some(hs) = self.images.clone() else {
            return match self.sel.feed(s, &i)? {
                SelOutcome::More => Ok(Step::Continue),
                SelOutcome::Empty => Ok(Step::Done),
                SelOutcome::Done(hs) => Ok(self.picked(s, hs)),
            };
        };
        let on = match (&i, keyword(&i).as_deref()) {
            (Input::Enter, _) => return Ok(Step::Done),
            (_, Some("on")) => true,
            (_, Some("off")) => false,
            _ => return Err(EngineError::Other("Requires ON or OFF.".into())),
        };
        run_transparency(s, &json!({ "handles": hex(&hs), "on": on }))?;
        s.set_selection(Vec::new());
        Ok(Step::Done)
    }
}
