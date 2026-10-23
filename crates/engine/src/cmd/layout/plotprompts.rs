//! -PLOT and -PAGESETUP: the Plot and Page Setup dialogs' questions asked on the command line in
//! AutoCAD's order, so scripts written for its command-line forms can answer them. Each answer
//! goes through PAGESETUP's own validation ([`super::apply_page`]); -PLOT ends in the PLOT JSON
//! form ([`super::run_plot`]), -PAGESETUP stores the layout's page setup (one undo step).
//!
//! Every device writes a file, so -PLOT asks for the file name where AutoCAD asks whether to
//! write the plot to a file.

use std::collections::VecDeque;

use cadcraft_doc::{Drawing, PageSetup, PlotOutput, Space};
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use super::super::plotters::{device_names, find_device};
use crate::{Accept, EngineError, Input, Interactive, Picker, Prompt, Result, Session, Step};

/// Keyword-only: spaces separate inputs.
const KW: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };
const MODEL: &str = "Model";
const SHADE: &[&str] = &["As displayed", "legacy Wireframe", "legacy Hidden", "Visual styles", "Rendered"];
/// Said when a question is answered with something we don't plot yet.
const IGNORED: &str = "This option is not available yet; it is ignored.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Q {
    Detailed,
    Layout,
    PageSetupName,
    Device,
    Paper,
    Units,
    Orientation,
    UpsideDown,
    Area,
    WindowLo,
    WindowHi,
    View,
    Scale,
    Offset,
    UseStyles,
    StyleTable,
    Lineweights,
    ScaleLineweights,
    PaperFirst,
    HidePaper,
    Shade,
    File,
    Save,
    Proceed,
}

pub(super) struct PlotM {
    /// -PLOT (else -PAGESETUP).
    plot: bool,
    detailed: bool,
    queue: VecDeque<Q>,
    /// "Model" or a layout name.
    target: String,
    page: PageSetup,
    /// Paper units are inches (scale and offset are typed in them).
    inches: bool,
    lo: Option<Vec2>,
    window: Option<[Vec2; 2]>,
    /// Model-space plot offset in mm, when `page.center` is off.
    offset: [f64; 2],
    path: Option<String>,
    save: bool,
}

impl PlotM {
    pub(super) fn plot() -> Self {
        Self::new(true)
    }
    pub(super) fn pagesetup() -> Self {
        Self::new(false)
    }
    fn new(plot: bool) -> Self {
        let queue = if plot { VecDeque::from([Q::Detailed]) } else { VecDeque::new() };
        PlotM {
            plot,
            detailed: !plot,
            queue,
            target: MODEL.into(),
            page: PageSetup::default(),
            inches: false,
            lo: None,
            window: None,
            offset: [0.0; 2],
            path: None,
            save: false,
        }
    }

    fn model(&self) -> bool {
        self.target == MODEL
    }

    /// Start on `target` with its page setup (model space: fitted extents on A4 in metric
    /// drawings, ANSI A otherwise, centred, as the Plot dialog starts).
    fn set_target(&mut self, d: &Drawing, target: String) {
        self.page = match d.layout(&target) {
            Some(l) => l.page.clone(),
            None => {
                let mut page = PageSetup { plot_area: "extents".into(), scale_to_fit: true, center: true, ..Default::default() };
                if cadcraft_render::paper::paper_unit_mm(d) == 1.0
                    && let Some(a4) = cadcraft_render::paper_size("A4")
                {
                    page.paper = a4.name.into();
                    page.width_mm = a4.width_mm;
                    page.height_mm = a4.height_mm;
                }
                page
            }
        };
        self.target = target;
        self.paper_changed();
        let model = self.model();
        let mut q = if self.detailed {
            let mut q =
                vec![Q::Device, Q::Paper, Q::Units, Q::Orientation, Q::UpsideDown, Q::Area, Q::Scale, Q::Offset, Q::UseStyles, Q::Lineweights];
            q.extend_from_slice(if model { &[Q::Shade] } else { &[Q::ScaleLineweights, Q::PaperFirst, Q::HidePaper] });
            q
        } else {
            vec![Q::PageSetupName, Q::Device]
        };
        if self.plot {
            q.extend([Q::File, Q::Save, Q::Proceed]);
        }
        self.queue.extend(q);
    }

    /// Paper units follow the paper (an inch size or not).
    fn paper_changed(&mut self) {
        self.inches = self.page.paper.to_ascii_lowercase().contains("inch");
    }

    fn unit_mm(&self) -> f64 {
        if self.inches { 25.4 } else { 1.0 }
    }

    fn output(&self) -> PlotOutput {
        find_device(&self.page.device).map_or(PlotOutput::Pdf, |d| d.output)
    }

    fn default_yes(&self, q: Q) -> bool {
        match q {
            Q::UseStyles => !self.page.plot_style_table.is_empty(),
            Q::Lineweights => self.page.lineweights,
            Q::Proceed => true,
            _ => false,
        }
    }

    /// `<drawing folder>/<drawing name>-<layout>.<ext>`, as AutoCAD names plot files.
    fn default_file(&self, s: &Session) -> String {
        let st = s.state().ok();
        let title = st.map(|st| st.title.clone()).unwrap_or_default();
        let stem = std::path::Path::new(&title).file_stem().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty());
        let name = format!("{}-{}.{}", stem.unwrap_or_else(|| "Drawing".into()), self.target, self.output().as_str());
        let dir = st.and_then(|st| st.path.as_deref()).and_then(|p| std::path::Path::new(p).parent()).filter(|d| !d.as_os_str().is_empty());
        match dir {
            Some(d) => d.join(name).to_string_lossy().into_owned(),
            None => name,
        }
    }

    /// The PLOT parameters these answers make (as the Plot dialog sends them).
    fn plot_json(&self) -> Value {
        let page = &self.page;
        let mut p = json!({
            "layout": self.target,
            "landscape": page.landscape,
            "fit": page.scale_to_fit,
            "lineweights": page.lineweights,
            "plotTransparency": page.plot_transparency,
        });
        let Some(o) = p.as_object_mut() else { return p };
        let device = find_device(&page.device);
        if device.is_some() || cadcraft_render::paper_size(&page.paper).is_some() {
            o.insert("device".into(), json!(device.as_ref().map_or("None", |d| d.name.as_str())));
            o.insert("paper".into(), json!(page.paper));
        } else {
            o.insert("device".into(), json!("None"));
            o.insert("width".into(), json!(page.width_mm));
            o.insert("height".into(), json!(page.height_mm));
        }
        if !page.plot_style_table.is_empty() {
            o.insert("plotStyleTable".into(), json!(page.plot_style_table));
        }
        if self.model() {
            o.insert("plotArea".into(), json!(page.plot_area));
            if self.output() != PlotOutput::Svg {
                o.insert("center".into(), json!(page.center));
                if !page.center {
                    o.insert("offset".into(), json!(self.offset));
                }
            }
            if !page.scale_to_fit {
                o.insert("scale".into(), json!(page.scale));
            }
            if let Some([a, b]) = self.window {
                o.insert("window".into(), json!([[a.x, a.y], [b.x, b.y]]));
            }
        }
        if let Some(path) = &self.path {
            o.insert("path".into(), json!(path));
        }
        p
    }

    /// Apply PAGESETUP settings `p` to the page, with its validation.
    fn apply(&mut self, p: Value) -> Result<()> {
        super::apply_page(self.name_lc(), &p, &mut self.page).map_err(retry)
    }

    fn name_lc(&self) -> &'static str {
        if self.plot { "plot" } else { "pagesetup" }
    }

    /// Take the answer to `q`: `Ok(true)` moves on, `Ok(false)` asks again (after a `?` list).
    fn answer(&mut self, s: &mut Session, q: Q, i: &Input) -> Result<bool> {
        let text = match i {
            Input::Text(t) | Input::Keyword(t) => Some(t.trim().trim_matches('"').trim().to_string()).filter(|t| !t.is_empty()),
            _ => None,
        };
        let listing = text.as_deref() == Some("?");
        match q {
            Q::Detailed => {
                self.detailed = yes_no(i, false)?;
                self.queue.insert(1, Q::Layout);
            }
            Q::Layout => {
                let d = s.doc()?;
                if listing {
                    let mut ls: Vec<_> = d.layouts.iter().collect();
                    ls.sort_by_key(|l| l.tab_order);
                    let names: Vec<String> = std::iter::once(MODEL.to_string()).chain(ls.iter().map(|l| l.name.clone())).collect();
                    list(s, &names);
                    return Ok(false);
                }
                let name = match text {
                    None => s.layout_space().tab_name().to_string(),
                    Some(n) if n.eq_ignore_ascii_case(MODEL) => MODEL.into(),
                    Some(n) => d.layout(&n).map(|l| l.name.clone()).ok_or_else(|| EngineError::Other(format!("Layout \"{n}\" not found.")))?,
                };
                self.set_target(d, name);
            }
            Q::PageSetupName => {
                if text.is_some() {
                    s.echo("Named page setups are not available yet; the layout's own page setup is used.");
                }
            }
            Q::Device => match text {
                _ if listing => {
                    let names: Vec<String> = std::iter::once("None".to_string()).chain(device_names()).collect();
                    list(s, &names);
                    return Ok(false);
                }
                Some(n) => {
                    self.apply(json!({ "device": n }))?;
                    self.paper_changed();
                }
                None => {}
            },
            Q::Paper => match text {
                _ if listing => {
                    let names: Vec<String> = match find_device(&self.page.device) {
                        Some(d) => d.papers.iter().map(|p| p.name.clone()).collect(),
                        None => cadcraft_render::PAPER_SIZES.iter().map(|p| p.name.to_string()).collect(),
                    };
                    list(s, &names);
                    return Ok(false);
                }
                Some(n) => {
                    // With the device named, a paper it doesn't have is refused.
                    self.apply(json!({ "device": self.page.device, "paper": n }))?;
                    self.paper_changed();
                }
                None => {}
            },
            Q::Units => {
                if let Some(k) = keyword(i)? {
                    self.inches = k == "Inches";
                }
            }
            Q::Orientation => {
                if let Some(k) = keyword(i)? {
                    self.page.landscape = k == "Landscape";
                }
            }
            // ponytail: upside down, lineweight scaling, paper space order, hidden paper space
            // and shade plots aren't plotted yet; answering for them only says so.
            Q::UpsideDown | Q::ScaleLineweights | Q::PaperFirst | Q::HidePaper => {
                if yes_no(i, false)? {
                    s.echo(IGNORED);
                }
            }
            Q::Shade => {
                if keyword(i)?.is_some_and(|k| k != "As displayed") {
                    s.echo(IGNORED);
                }
            }
            Q::Area => {
                let Some(area) = keyword(i)?.map(str::to_ascii_lowercase) else { return Ok(true) };
                if !self.model() {
                    self.apply(json!({ "plotArea": area }))?;
                    return Ok(true);
                }
                match area.as_str() {
                    "window" => {
                        self.queue.insert(1, Q::WindowLo);
                        self.queue.insert(2, Q::WindowHi);
                    }
                    "view" => self.queue.insert(1, Q::View),
                    _ => self.window = None,
                }
                self.page.plot_area = if area == "view" { "window".into() } else { area };
            }
            Q::WindowLo => match i {
                Input::Point(p) => self.lo = Some(*p),
                _ => return Err(EngineError::Other("Requires a point.".into())),
            },
            Q::WindowHi => match (i, self.lo) {
                (Input::Point(b), Some(a)) => {
                    let (lo, hi) = (Vec2::new(a.x.min(b.x), a.y.min(b.y)), Vec2::new(a.x.max(b.x), a.y.max(b.y)));
                    if !(hi.x - lo.x > 1e-9 && hi.y - lo.y > 1e-9) {
                        return Err(EngineError::Other("The window needs a width and a height.".into()));
                    }
                    self.window = Some([lo, hi]);
                }
                _ => return Err(EngineError::Other("Requires a point.".into())),
            },
            Q::View => {
                let Some(n) = text else { return Err(EngineError::Other("Requires a view name.".into())) };
                let v = s.doc()?.views.iter().find(|v| v.layout.is_none() && v.name.eq_ignore_ascii_case(&n)).cloned();
                let v = v.ok_or_else(|| EngineError::Other(format!("View \"{n}\" not found.")))?;
                let half = Vec2::new(v.width / 2.0, v.height / 2.0);
                self.window = Some([v.center - half, v.center + half]);
            }
            Q::Scale => match i {
                Input::Keyword(_) => self.page.scale_to_fit = true,
                Input::Text(t) => {
                    let k = super::parse_scale(&json!(t.trim().replace('=', ":")))
                        .map(|k| k * self.unit_mm())
                        .filter(|k| k.is_finite())
                        .ok_or_else(|| EngineError::Other("Requires a positive scale such as 1:50, or Fit.".into()))?;
                    self.page.scale = k;
                    self.page.scale_to_fit = false;
                }
                _ => {}
            },
            Q::Offset => match i {
                Input::Keyword(_) => self.page.center = true,
                Input::Text(t) => {
                    let xy: Vec<f64> = t
                        .split(',')
                        .map(|v| v.trim().parse::<f64>().map(|v| v * self.unit_mm()))
                        .collect::<std::result::Result<_, _>>()
                        .unwrap_or_default();
                    let max = cadcraft_render::paper::MAX_PLOT_OFFSET_MM;
                    let [x, y] = xy[..] else { return Err(EngineError::Other("Requires an offset x,y, or Center.".into())) };
                    if !(x.is_finite() && y.is_finite() && x.abs() <= max && y.abs() <= max) {
                        return Err(EngineError::Other("Requires an offset x,y, or Center.".into()));
                    }
                    self.page.center = false;
                    self.offset = [x, y];
                    // ponytail: layouts plot at their sheet origin; their offset isn't kept yet.
                    if !self.model() && (x != 0.0 || y != 0.0) {
                        s.echo(IGNORED);
                    }
                }
                _ => {}
            },
            Q::UseStyles => {
                if yes_no(i, self.default_yes(q))? {
                    self.queue.insert(1, Q::StyleTable);
                } else {
                    self.page.plot_style_table.clear();
                }
            }
            Q::StyleTable => match text.as_deref() {
                Some("?") => {
                    let names = cadcraft_doc::plot_style_table_names(s.doc()?);
                    list(s, &names);
                    return Ok(false);
                }
                Some(".") => self.page.plot_style_table.clear(),
                Some(n) => {
                    let t = cadcraft_doc::plot_style_table(s.doc()?, n)
                        .ok_or_else(|| EngineError::Other(format!("Plot style table \"{n}\" not found.")))?;
                    self.page.plot_style_table = t.name;
                }
                None => {}
            },
            Q::Lineweights => self.page.lineweights = yes_no(i, self.default_yes(q))?,
            Q::File => {
                let ext = self.output().as_str();
                self.path = Some(match text {
                    None => self.default_file(s),
                    Some(t) if std::path::Path::new(&t).extension().is_some() => t,
                    Some(t) => format!("{t}.{ext}"),
                });
            }
            Q::Save => self.save = yes_no(i, false)?,
            Q::Proceed => {
                let go = yes_no(i, true)?;
                self.queue.clear();
                self.finish(s, go)?;
            }
        }
        Ok(true)
    }

    /// Store the page setup (-PAGESETUP, or -PLOT's "save changes") and plot (`go`).
    fn finish(&mut self, s: &mut Session, go: bool) -> Result<()> {
        if self.save || !self.plot {
            if self.model() {
                s.echo("Model space page setups are not saved yet.");
            } else {
                let before = s.state()?.doc.clone();
                if let Some(l) = s.doc_mut()?.layouts.iter_mut().find(|l| l.name == self.target) {
                    l.page = self.page.clone();
                }
                // PLOT records no undo step of its own (it only writes a file): the page setup it
                // saves gets one, so U takes back just that.
                if self.plot
                    && let Ok(st) = s.state_mut()
                {
                    let selection = st.selection.clone();
                    st.push_undo(crate::Snapshot { label: "Page Setup".into(), doc: before, selection });
                    st.revision += 1;
                }
            }
        }
        if self.plot && go {
            let p = self.plot_json();
            let r = super::super::plotters::plot_options(s, &p, "plot").and_then(|o| super::run_plot(s, &o, "plot"));
            match r {
                Ok(r) => s.echo(format!("Plotted to {}.", r.get("path").and_then(Value::as_str).unwrap_or_default())),
                Err(e) => s.echo(retry(e).to_string()),
            }
        }
        Ok(())
    }
}

/// An engine refusal as a re-prompt message (no `invalid parameters for …` prefix).
fn retry(e: EngineError) -> EngineError {
    match e {
        EngineError::BadParams { msg, .. } => EngineError::Other(msg),
        other => other,
    }
}

fn yes_no(i: &Input, default: bool) -> Result<bool> {
    match i {
        Input::Keyword(k) => Ok(k == "Yes"),
        Input::Enter => Ok(default),
        _ => Err(EngineError::Other("Invalid option keyword.".into())),
    }
}

/// A keyword answer, `None` for Enter (the default).
fn keyword(i: &Input) -> Result<Option<&str>> {
    match i {
        Input::Keyword(k) => Ok(Some(k)),
        Input::Enter => Ok(None),
        _ => Err(EngineError::Other("Invalid option keyword.".into())),
    }
}

fn yn(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

fn list(s: &mut Session, names: &[String]) {
    for n in names.iter().take(1000) {
        s.echo(n.clone());
    }
}

/// A number for a prompt default: up to four decimals, no trailing zeros.
fn num(v: f64) -> String {
    let t = format!("{v:.4}");
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

impl Interactive for PlotM {
    fn name(&self) -> &'static str {
        if self.plot { "-PLOT" } else { "-PAGESETUP" }
    }

    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if self.plot {
            return Ok(Step::Continue);
        }
        let Space::Paper(name) = s.layout_space() else {
            s.echo("Model space page setups are not stored yet; run -PAGESETUP on a layout tab.");
            return Ok(Step::Done);
        };
        self.set_target(s.doc()?, name);
        Ok(Step::Continue)
    }

    fn prompt(&self, s: &Session) -> Prompt {
        let page = &self.page;
        let units = if self.inches { "Inches" } else { "Millimeters" };
        let yes_no = |m: &str, q: Q| Prompt::new(m, KW).kw(&["Yes", "No"]).default(yn(self.default_yes(q)));
        let Some(q) = self.queue.front().copied() else { return Prompt::new("Proceed with plot?", KW).kw(&["Yes", "No"]) };
        match q {
            Q::Detailed => yes_no("Detailed plot configuration?", q),
            Q::Layout => Prompt::new("Enter a layout name or [?]", Accept::TEXT).default(s.layout_space().tab_name().to_string()),
            Q::PageSetupName => Prompt::new("Enter a page setup name", Accept::TEXT).default(""),
            Q::Device => Prompt::new("Enter an output device name or [?]", Accept::TEXT).default(page.device.clone()),
            Q::Paper => Prompt::new("Enter paper size or [?]", Accept::TEXT).default(page.paper.clone()),
            Q::Units => Prompt::new("Enter paper units", KW).kw(&["Inches", "Millimeters"]).default(units),
            Q::Orientation => Prompt::new("Enter drawing orientation", KW).kw(&["Portrait", "Landscape"]).default(if page.landscape {
                "Landscape"
            } else {
                "Portrait"
            }),
            Q::UpsideDown => yes_no("Plot upside down?", q),
            Q::Area if self.model() => {
                Prompt::new("Enter plot area", KW).kw(&["Display", "Extents", "Limits", "View", "Window"]).default(cap(&page.plot_area))
            }
            Q::Area => Prompt::new("Enter plot area", KW).kw(&["Display", "Extents", "Layout", "View", "Window"]).default(cap(&page.plot_area)),
            Q::WindowLo => Prompt::new("Enter lower left corner of window", Accept::POINT),
            Q::WindowHi => {
                let p = Prompt::new("Enter upper right corner of window", Accept::POINT);
                match self.lo {
                    Some(a) => p.corner(a),
                    None => p,
                }
            }
            Q::View => Prompt::new("Enter view name", Accept::TEXT),
            Q::Scale => {
                let cur = if page.scale_to_fit { "Fit".to_string() } else { format!("1:{}", num(self.unit_mm() / page.scale)) };
                Prompt::new(format!("Enter plot scale (Plotted {units}=Drawing Units)"), Accept::WORD).kw(&["Fit"]).default(cur)
            }
            Q::Offset => {
                let k = self.unit_mm();
                let cur = if page.center { "Center".to_string() } else { format!("{},{}", num(self.offset[0] / k), num(self.offset[1] / k)) };
                Prompt::new("Enter plot offset (x,y)", Accept::WORD).kw(&["Center"]).default(cur)
            }
            Q::UseStyles => yes_no("Plot with plot styles?", q),
            Q::StyleTable => {
                let p = Prompt::new("Enter plot style table name or [?] (enter . for none)", Accept::TEXT);
                if page.plot_style_table.is_empty() { p } else { p.default(page.plot_style_table.clone()) }
            }
            Q::Lineweights => yes_no("Plot with lineweights?", q),
            Q::ScaleLineweights => yes_no("Scale lineweights with plot scale?", q),
            Q::PaperFirst => yes_no("Plot paper space first?", q),
            Q::HidePaper => yes_no("Hide paperspace objects?", q),
            Q::Shade => Prompt::new("Enter shade plot setting", KW).kw(SHADE).default("As displayed"),
            Q::File => {
                let (label, exts): (&'static str, &'static [&'static str]) = match self.output() {
                    PlotOutput::Png => ("PNG", &["png"]),
                    PlotOutput::Svg => ("SVG", &["svg"]),
                    _ => ("PDF", &["pdf"]),
                };
                Prompt::new("Enter file name", Accept::WORD).default(self.default_file(s)).picker(Picker::File { save: true, label, exts })
            }
            Q::Save => yes_no("Save changes to page setup?", q),
            Q::Proceed => yes_no("Proceed with plot?", q),
        }
    }

    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if i == Input::Cancel {
            return Ok(Step::Cancel);
        }
        let Some(q) = self.queue.front().copied() else { return Ok(Step::Done) };
        if !self.answer(s, q, &i)? {
            return Ok(Step::Continue);
        }
        self.queue.pop_front();
        if !self.queue.is_empty() {
            return Ok(Step::Continue);
        }
        // -PAGESETUP's last answer (-PLOT ends at "Proceed with plot?").
        if !self.plot {
            self.finish(s, false)?;
        }
        Ok(Step::Done)
    }
}
