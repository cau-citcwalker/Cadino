//! The command-line forms of the drawing settings: COLOR, LWEIGHT, LTSCALE, LIMITS, -UNITS and
//! -STYLE. Each asks what AutoCAD asks and ends by running the command's JSON form, so the
//! dialogs, scripts and the control channel share one path.

use cadcraft_color::{Color, Rgb};
use cadcraft_doc::{Drawing, Lineweight, TextStyle};
use cadcraft_geom::Vec2;
use serde_json::{Value, json};

use crate::units::{AngleSettings, format_angle, format_distance};
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

/// A prompt answered only by its keywords (Space is Enter).
const KEYWORDS: Accept = Accept { point: false, number: false, text: false, select: false, enter: true };
/// A value typed as a word or a number (a colour, a lineweight): Space ends it, as at a number
/// prompt.
const VALUE: Accept = Accept { number: true, ..Accept::TEXT };

fn yes(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

/// The colour new objects get (CECOLOR, or the true colour beside it).
fn current_color(d: &Drawing) -> Color {
    let h = &d.header;
    match h.get("CECOLOR_RGB").and_then(cadcraft_doc::HVal::as_i64).and_then(|v| u32::try_from(v).ok()) {
        Some(v) => Color::True(Rgb::from_u32(v)),
        None => Color::from_aci(i16::try_from(h.i64("CECOLOR", 256)).unwrap_or(256)),
    }
}

/// COLOR: "Enter default object color [Truecolor/COlorbook] <BYLAYER>:".
#[derive(Default)]
pub(super) struct ColorM {
    truecolor: bool,
}

impl Interactive for ColorM {
    fn name(&self) -> &'static str {
        "COLOR"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        if self.truecolor {
            return Prompt::new("Red, Green, Blue", VALUE);
        }
        let cur = s.doc().map(current_color).unwrap_or_default();
        let shown = if matches!(cur, Color::ByLayer | Color::ByBlock) { cur.name().to_ascii_uppercase() } else { cur.name() };
        Prompt::new("Enter default object color", VALUE).kw(&["Truecolor", "COlorbook"]).default(shown)
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Keyword(k) if k == "Truecolor" => self.truecolor = true,
            // ponytail: no colour books ship with CADCraft; add the lookup when one does.
            Input::Keyword(_) => s.echo("No color books are available."),
            Input::Text(t) => match Color::parse(&t) {
                Some(c) if !self.truecolor || matches!(c, Color::True(_)) => {
                    super::run_color(s, &json!({ "color": t.trim() }))?;
                    return Ok(Step::Done);
                }
                _ => s.echo(if self.truecolor { "Requires three values from 0 to 255, separated by commas." } else { "Invalid color." }),
            },
            Input::Enter => return Ok(Step::Done),
            Input::Cancel => return Ok(Step::Cancel),
            _ => {}
        }
        Ok(Step::Continue)
    }
}

/// LWEIGHT: "Current lineweight: ByLayer", then "Enter default lineweight for new objects or [?]".
pub(super) struct LweightM;

fn current_lw(s: &Session) -> Lineweight {
    s.doc().map(|d| Lineweight::from_dxf(i16::try_from(d.header.i64("CELWEIGHT", -1)).unwrap_or(-1))).unwrap_or_default()
}

impl Interactive for LweightM {
    fn name(&self) -> &'static str {
        "LWEIGHT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        s.echo(format!("Current lineweight: {}", current_lw(s).name()));
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        Prompt::new("Enter default lineweight for new objects", VALUE).kw(&["?"]).default(current_lw(s).name())
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Keyword(_) => {
                let all: Vec<String> = ["ByLayer", "ByBlock", "Default"]
                    .into_iter()
                    .map(String::from)
                    .chain(Lineweight::STANDARD.iter().map(|w| Lineweight::Mm100(*w).name()))
                    .collect();
                for row in all.chunks(6) {
                    s.echo(row.iter().map(|n| format!("{n:<10}")).collect::<String>().trim_end().to_string());
                }
            }
            Input::Text(t) => match super::super::layer::parse_lw(&Value::String(t)) {
                Some(lw) => {
                    // Any other width is taken to the nearest standard one, as AutoCAD does.
                    let v = match lw {
                        Lineweight::Mm100(x) => {
                            let w = Lineweight::STANDARD.iter().copied().min_by_key(|w| w.abs_diff(x)).unwrap_or(x);
                            json!(f64::from(w) / 100.0)
                        }
                        other => json!(other.name()),
                    };
                    super::run_lweight(s, &json!({ "lineweight": v }))?;
                    return Ok(Step::Done);
                }
                None => s.echo("Invalid lineweight."),
            },
            Input::Enter => return Ok(Step::Done),
            Input::Cancel => return Ok(Step::Cancel),
            _ => {}
        }
        Ok(Step::Continue)
    }
}

/// LTSCALE: "Enter new linetype scale factor <1.0000>:", then "Regenerating model.".
pub(super) struct LtscaleM;

impl Interactive for LtscaleM {
    fn name(&self) -> &'static str {
        "LTSCALE"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let cur = s.doc().map(|d| d.header.f64("LTSCALE", 1.0)).unwrap_or(1.0);
        Prompt::new("Enter new linetype scale factor", Accept::NUMBER).default(format!("{cur:.4}"))
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Text(t) => match super::super::machines::number(&t).filter(|v| *v > 0.0 && v.is_finite()) {
                Some(v) => {
                    let r = super::run_ltscale(s, &json!({ "scale": v }))?;
                    if let Some(m) = r.get("message").and_then(Value::as_str) {
                        s.echo(m.to_string());
                    }
                    Ok(Step::Done)
                }
                None => {
                    s.echo("Value must be positive and nonzero.");
                    Ok(Step::Continue)
                }
            },
            Input::Cancel => Ok(Step::Cancel),
            Input::Enter => Ok(Step::Done),
            _ => Ok(Step::Continue),
        }
    }
}

/// LIMITS: "Specify lower left corner or [ON/OFF] <0.0000,0.0000>:", then "Specify upper right
/// corner <420.0000,297.0000>:". ON and OFF set LIMCHECK. The corners are points of the current
/// UCS, as the JSON form takes them.
#[derive(Default)]
pub(super) struct LimitsM {
    lower: Option<Vec2>,
}

/// LIMMIN or LIMMAX as a point of the current UCS (the JSON form takes UCS points).
fn limit(s: &Session, var: &str, default: Vec2) -> Vec2 {
    let w = s.doc().ok().and_then(|d| d.header.point(var)).map_or(default, |p| p.xy());
    let c = s.cplane();
    if crate::cplane::tilted(&c) { w } else { crate::cplane::plan_to_plane(&c, w) }
}

fn xy(p: Vec2) -> String {
    format!("{:.4},{:.4}", p.x, p.y)
}

impl Interactive for LimitsM {
    fn name(&self) -> &'static str {
        "LIMITS"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        // ponytail: the JSON form sets model space limits only (LIMMIN/LIMMAX); paper space's
        // PLIMMIN/PLIMMAX when a layout needs them.
        s.echo("Reset Model space limits:");
        Ok(Step::Continue)
    }
    fn prompt(&self, s: &Session) -> Prompt {
        match self.lower {
            None => Prompt::new("Specify lower left corner", Accept::POINT).kw(&["ON", "OFF"]).default(xy(limit(s, "LIMMIN", Vec2::ZERO))),
            Some(a) => Prompt::new("Specify upper right corner", Accept::POINT).corner(a).default(xy(limit(s, "LIMMAX", Vec2::new(420.0, 297.0)))),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let p = match i {
            Input::Keyword(k) if self.lower.is_none() => {
                s.doc_mut()?.header.set_i64("LIMCHECK", i64::from(k == "ON"));
                return Ok(Step::Done);
            }
            Input::Point(p) => p,
            Input::Enter if self.lower.is_none() => limit(s, "LIMMIN", Vec2::ZERO),
            Input::Enter => limit(s, "LIMMAX", Vec2::new(420.0, 297.0)),
            Input::Cancel => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        match self.lower {
            None => {
                self.lower = Some(p);
                Ok(Step::Continue)
            }
            Some(a) => {
                super::run_limits(s, &json!({ "min": [a.x, a.y], "max": [p.x, p.y] }))?;
                Ok(Step::Done)
            }
        }
    }
}

/// -UNITS: the length format and precision, the angle format and precision, the direction of
/// angle 0 and whether angles turn clockwise, each listed with examples drawn by CADCraft's own
/// formatters.
pub(super) struct UnitsM {
    step: u8,
    lunits: i64,
    luprec: i64,
    aunits: i64,
    auprec: i64,
    /// Radians counterclockwise from +X.
    angbase: f64,
    clockwise: bool,
}

impl UnitsM {
    pub(super) fn new(s: &Session) -> Self {
        let h = s.doc().map(|d| d.header.clone()).unwrap_or_default();
        UnitsM {
            step: 0,
            lunits: h.i64("LUNITS", 2).clamp(1, 5),
            luprec: h.i64("LUPREC", 4).clamp(0, 8),
            aunits: h.i64("AUNITS", 0).clamp(0, 4),
            auprec: h.i64("AUPREC", 0).clamp(0, 8),
            angbase: h.f64("ANGBASE", 0.0),
            clockwise: h.i64("ANGDIR", 0) != 0,
        }
    }
    fn fractions(&self) -> bool {
        matches!(self.lunits, 4 | 5)
    }
    /// The listing shown before the step's question.
    fn listing(&self, s: &mut Session) {
        let lines: Vec<String> =
            match self.step {
                0 => std::iter::once("Report formats:     (Examples)".to_string())
                    .chain(
                        ["Scientific", "Decimal", "Engineering", "Architectural", "Fractional"]
                            .iter()
                            .zip(1..)
                            .map(|(n, k)| format!("  {k}. {n:<15}{}", format_distance(15.5, k, if k >= 4 { 1 } else { 2 }))),
                    )
                    .collect(),
                2 => {
                    std::iter::once("Systems of angle measure:        (Examples)".to_string())
                        .chain(["Decimal degrees", "Degrees/minutes/seconds", "Grads", "Radians", "Surveyor's units"].iter().zip(0..).map(
                            |(n, k)| format!("  {}. {n:<28}{}", k + 1, format_angle(45f64.to_radians(), k, if k == 1 || k == 4 { 0 } else { 4 })),
                        ))
                        .collect()
                }
                4 => {
                    std::iter::once("Direction for angle 0:".to_string())
                        .chain(["East   3 o'clock", "North 12 o'clock", "West   9 o'clock", "South  6 o'clock"].iter().zip(0..).map(|(n, k)| {
                            format!("  {n}  =  {}", format_angle(f64::from(k) * std::f64::consts::FRAC_PI_2, self.aunits, self.auprec))
                        }))
                        .collect()
                }
                _ => Vec::new(),
            };
        for l in lines {
            s.echo(l);
        }
    }
    /// Say what is wrong with the answer; the question stays.
    fn refuse(s: &mut Session, msg: String) -> Result<Step> {
        s.echo(msg);
        Ok(Step::Continue)
    }
}

impl Interactive for UnitsM {
    fn name(&self) -> &'static str {
        "UNITS"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        self.listing(s);
        Ok(Step::Continue)
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match self.step {
            0 => Prompt::new("Enter choice, 1 to 5", Accept::NUMBER).default(self.lunits.to_string()),
            1 if self.fractions() => {
                Prompt::new("Enter denominator of smallest fraction to display (1, 2, 4, 8, 16, 32, 64, 128, or 256)", Accept::NUMBER)
                    .default((1i64 << self.luprec).to_string())
            }
            1 => Prompt::new("Enter number of digits to right of decimal point (0 to 8)", Accept::NUMBER).default(self.luprec.to_string()),
            2 => Prompt::new("Enter choice, 1 to 5", Accept::NUMBER).default((self.aunits + 1).to_string()),
            3 => Prompt::new("Enter number of fractional places for display of angles (0 to 8)", Accept::NUMBER).default(self.auprec.to_string()),
            4 => Prompt::new("Enter direction for angle 0", Accept::NUMBER).default(format_angle(self.angbase, self.aunits, self.auprec)),
            _ => Prompt::new("Do you want angles measured clockwise?", KEYWORDS).kw(&["Yes", "No"]).default(yes(self.clockwise)),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let t = match i {
            Input::Text(t) => Some(t),
            Input::Keyword(k) => Some(k),
            Input::Enter => None,
            Input::Cancel => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        };
        if let Some(t) = t {
            // The answer, or what is wrong with it.
            let bad = match self.step {
                0 | 2 | 3 => {
                    let (lo, hi) = if self.step == 3 { (0, 8) } else { (1, 5) };
                    match t.trim().parse::<i64>().ok().filter(|v| (lo..=hi).contains(v)) {
                        Some(v) if self.step == 0 => self.lunits = v,
                        Some(v) if self.step == 2 => self.aunits = v - 1,
                        Some(v) => self.auprec = v,
                        None => return Self::refuse(s, format!("Requires an integer between {lo} and {hi}.")),
                    }
                    None
                }
                1 if self.fractions() => match t.trim().parse::<u32>().ok().filter(|d| d.is_power_of_two() && *d <= 256) {
                    Some(d) => {
                        self.luprec = i64::from(d.trailing_zeros());
                        None
                    }
                    None => Some("Requires 1, 2, 4, 8, 16, 32, 64, 128 or 256."),
                },
                1 => match t.trim().parse::<i64>().ok().filter(|v| (0..=8).contains(v)) {
                    Some(v) => {
                        self.luprec = v;
                        None
                    }
                    None => Some("Requires an integer between 0 and 8."),
                },
                4 => match (AngleSettings { aunits: self.aunits, ..Default::default() }).direction(&t) {
                    Some(a) => {
                        self.angbase = a;
                        None
                    }
                    None => Some("Requires a valid angle."),
                },
                _ if t == "Yes" || t == "No" => {
                    self.clockwise = t == "Yes";
                    None
                }
                _ => Some("Invalid option keyword."),
            };
            if let Some(m) = bad {
                return Self::refuse(s, m.to_string());
            }
        }
        if self.step < 5 {
            self.step += 1;
            self.listing(s);
            return Ok(Step::Continue);
        }
        let p = json!({
            "lunits": self.lunits, "luprec": self.luprec, "aunits": self.aunits, "auprec": self.auprec,
            "angbase": self.angbase.to_degrees(), "angdir": self.clockwise,
        });
        super::run_units(s, &p)?;
        Ok(Step::Done)
    }
}

/// -STYLE: the style's name, then its font, height, width factor, obliquing angle and the
/// Backwards, Upside-down and Vertical settings; the style becomes current.
#[derive(Default)]
pub(super) struct StyleM {
    /// The style being edited, once its name is known.
    style: Option<TextStyle>,
    step: u8,
    /// Listing names: "Enter text style(s) to list <*>".
    listing: bool,
}

const FONT: u8 = 1;
const HEIGHT: u8 = 2;
const WIDTH: u8 = 3;
const OBLIQUE: u8 = 4;
const BACKWARDS: u8 = 5;
const UPSIDE_DOWN: u8 = 6;
const VERTICAL: u8 = 7;

impl StyleM {
    /// Store the style, make it current and say so.
    fn finish(&self, s: &mut Session) -> Result<Step> {
        let Some(st) = &self.style else { return Ok(Step::Cancel) };
        super::run_style(
            s,
            &json!({
                "name": st.name, "font": st.font, "height": st.height, "widthFactor": st.width_factor,
                "oblique": st.oblique.to_degrees(), "backwards": st.backwards, "upsideDown": st.upside_down,
                "vertical": st.vertical, "annotative": st.annotative, "current": true,
            }),
        )?;
        s.echo(format!("\"{}\" is now the current text style.", st.name));
        Ok(Step::Done)
    }
    /// The step after `step`: Vertical is asked only for stroke (SHX-like) fonts.
    fn next(&mut self, s: &mut Session) -> Result<Step> {
        self.step += 1;
        let outline = self.style.as_ref().is_some_and(|st| cadcraft_fonts::TextFont::resolve(&st.font).is_outline());
        if self.step > VERTICAL || (self.step == VERTICAL && outline) {
            return self.finish(s);
        }
        Ok(Step::Continue)
    }
}

impl Interactive for StyleM {
    fn name(&self) -> &'static str {
        "STYLE"
    }
    fn prompt(&self, s: &Session) -> Prompt {
        if self.listing {
            return Prompt::new("Enter text style(s) to list", Accept::TEXT).default("*");
        }
        let Some(st) = &self.style else {
            let cur = s.doc().map(|d| d.header.str("TEXTSTYLE", "Standard")).unwrap_or_else(|_| "Standard".into());
            return Prompt::new("Enter name of text style", Accept::TEXT).kw(&["?"]).default(cur);
        };
        let yn = |m: &str, b: bool| Prompt::new(m, KEYWORDS).kw(&["Yes", "No"]).default(yes(b));
        match self.step {
            FONT => Prompt::new("Specify full font name or font filename (TTF or SHX)", Accept::WORD).default(st.font.clone()),
            HEIGHT if st.annotative => Prompt::new("Specify paper text height", Accept::NUMBER).default(format!("{:.4}", st.height)),
            HEIGHT => Prompt::new("Specify height of text", Accept::NUMBER).kw(&["Annotative"]).default(format!("{:.4}", st.height)),
            WIDTH => Prompt::new("Specify width factor", Accept::NUMBER).default(format!("{:.4}", st.width_factor)),
            OBLIQUE => Prompt::new("Specify obliquing angle", Accept::NUMBER).default(format!("{}", st.oblique.to_degrees())),
            BACKWARDS => yn("Display text backwards?", st.backwards),
            UPSIDE_DOWN => yn("Display text upside-down?", st.upside_down),
            _ => yn("Vertical?", st.vertical),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if matches!(i, Input::Cancel) {
            return Ok(Step::Cancel);
        }
        if self.listing {
            let pat = match &i {
                Input::Text(t) => t.trim().to_string(),
                _ => "*".to_string(),
            };
            let d = s.doc()?;
            let names: Vec<String> =
                d.text_styles.iter().filter(|t| super::super::qselect::wildcard(&pat, &t.name)).map(|t| t.name.clone()).collect();
            s.echo("Text styles:");
            for n in names {
                s.echo(format!("  {n}"));
            }
            self.listing = false;
            return Ok(Step::Continue);
        }
        let Some(st) = &mut self.style else {
            let name = match i {
                Input::Keyword(_) => {
                    self.listing = true;
                    return Ok(Step::Continue);
                }
                Input::Text(t) => t.trim().to_string(),
                Input::Enter => s.doc()?.header.str("TEXTSTYLE", "Standard"),
                _ => return Ok(Step::Continue),
            };
            if name.is_empty() || name.len() > 255 {
                s.echo("Invalid style name.");
                return Ok(Step::Continue);
            }
            let existing = s.doc()?.text_style(&name).cloned();
            s.echo(if existing.is_some() { "Existing style." } else { "New style." });
            self.style = Some(existing.unwrap_or(TextStyle { name, ..Default::default() }));
            self.step = FONT;
            return Ok(Step::Continue);
        };
        let t = match i {
            Input::Text(t) => t,
            Input::Keyword(k) if k == "Annotative" => {
                st.annotative = true;
                return Ok(Step::Continue);
            }
            Input::Keyword(k) => k,
            Input::Enter => return self.next(s),
            _ => return Ok(Step::Continue),
        };
        let num = super::super::machines::number(&t).filter(|v| v.is_finite());
        let bad = |s: &mut Session, m: &str| {
            s.echo(m.to_string());
            Ok(Step::Continue)
        };
        if self.step >= BACKWARDS && t != "Yes" && t != "No" {
            return bad(s, "Invalid option keyword.");
        }
        match self.step {
            FONT => st.font = t.trim().trim_matches('"').trim().to_string(),
            HEIGHT => match num.filter(|h| *h >= 0.0) {
                Some(h) => st.height = h,
                None => return bad(s, "Requires a non-negative number."),
            },
            WIDTH => match num.filter(|w| *w > 0.0 && *w <= 100.0) {
                Some(w) => st.width_factor = w,
                None => return bad(s, "Requires a number greater than 0 and at most 100."),
            },
            OBLIQUE => match (AngleSettings::default()).amount(&t).map(f64::to_degrees).filter(|o| o.abs() <= 85.0) {
                Some(o) => st.oblique = o.to_radians(),
                None => return bad(s, "Requires an angle between -85 and 85."),
            },
            BACKWARDS => st.backwards = t == "Yes",
            UPSIDE_DOWN => st.upside_down = t == "Yes",
            _ => st.vertical = t == "Yes",
        }
        self.next(s)
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_geom::Vec2;
    use serde_json::json;

    use crate::{Input, Session};

    fn ends_with(s: &Session, tail: &str) {
        assert!(s.prompt_text().ends_with(tail), "{}", s.prompt_text());
    }

    /// Typed bare, the settings commands prompt (they used to refuse with "`x` is required");
    /// the JSON forms are unchanged.
    #[test]
    fn color_lweight_ltscale_prompt() {
        let mut s = Session::new();
        s.cmdline("color").unwrap();
        assert_eq!(s.prompt_text(), "COLOR Enter default object color or [Truecolor/COlorbook] <BYLAYER>:");
        s.cmdline("bogus").unwrap();
        assert!(s.running.is_some() && s.log.last().is_some_and(|l| l == "Invalid color."));
        s.cmdline("red").unwrap();
        assert!(s.running.is_none());
        assert_eq!(s.doc().unwrap().header.i64("CECOLOR", 256), 1);
        s.cmdline("-color t").unwrap();
        ends_with(&s, "Red, Green, Blue:");
        s.cmdline("10,20,30").unwrap();
        assert_eq!(s.doc().unwrap().header.i64("CECOLOR_RGB", 0), 0x0a141e);
        s.cmdline("color").unwrap();
        ends_with(&s, "<10,20,30>:");
        s.cmdline("").unwrap();
        assert!(s.running.is_none());

        s.cmdline("lweight").unwrap();
        assert!(s.log.iter().any(|l| l == "Current lineweight: ByLayer"), "{:?}", s.log);
        assert_eq!(s.prompt_text(), "LWEIGHT Enter default lineweight for new objects or [?] <ByLayer>:");
        s.cmdline("?").unwrap();
        assert!(s.log.iter().any(|l| l.contains("0.25 mm")) && s.running.is_some());
        s.cmdline("0.26").unwrap();
        assert_eq!(s.doc().unwrap().header.i64("CELWEIGHT", -1), 25, "the nearest standard lineweight");

        s.cmdline("lts").unwrap();
        assert_eq!(s.prompt_text(), "LTSCALE Enter new linetype scale factor <1.0000>:");
        s.cmdline("-2").unwrap();
        assert!(s.running.is_some());
        s.cmdline("2.5").unwrap();
        assert_eq!(s.doc().unwrap().header.f64("LTSCALE", 1.0), 2.5);
        assert_eq!(s.log.last().map(String::as_str), Some("Regenerating model."));

        // JSON forms as before.
        s.execute("color", &json!({ "color": 3 })).unwrap();
        assert_eq!(s.doc().unwrap().header.i64("CECOLOR", 256), 3);
        assert!(s.execute("ltscale", &json!({})).is_err());
        assert!(s.running.is_none());
    }

    #[test]
    fn limits_prompts_for_corners_and_on_off() {
        let mut s = Session::new();
        s.cmdline("limits").unwrap();
        assert!(s.log.iter().any(|l| l == "Reset Model space limits:"));
        assert!(s.prompt_text().starts_with("LIMITS Specify lower left corner or [ON/OFF] <"), "{}", s.prompt_text());
        s.cmdline("10,20").unwrap();
        ends_with(&s, ">:");
        assert!(s.prompt_text().contains("Specify upper right corner <"), "{}", s.prompt_text());
        s.input(Input::Point(Vec2::new(110.0, 90.0))).unwrap();
        assert!(s.running.is_none());
        let h = &s.doc().unwrap().header;
        assert_eq!((h.point("LIMMIN").unwrap().xy(), h.point("LIMMAX").unwrap().xy()), (Vec2::new(10.0, 20.0), Vec2::new(110.0, 90.0)));
        s.cmdline("limits").unwrap();
        ends_with(&s, "<10.0000,20.0000>:");
        s.cmdline("on").unwrap();
        assert!(s.running.is_none());
        assert_eq!(s.doc().unwrap().header.i64("LIMCHECK", 0), 1);
        // Enter twice keeps the limits.
        s.cmdline("limits").unwrap();
        s.cmdline("").unwrap();
        ends_with(&s, "<110.0000,90.0000>:");
        s.cmdline("").unwrap();
        assert_eq!(s.doc().unwrap().header.point("LIMMAX").unwrap().xy(), Vec2::new(110.0, 90.0));
    }

    #[test]
    fn dash_units_asks_every_setting() {
        let mut s = Session::new();
        s.cmdline("-units").unwrap();
        assert!(s.log.iter().any(|l| l.starts_with("  4. Architectural")), "{:?}", s.log);
        assert_eq!(s.prompt_text(), "-UNITS Enter choice, 1 to 5 <2>:");
        s.cmdline("9").unwrap();
        assert!(s.log.last().is_some_and(|l| l.contains("between 1 and 5")));
        s.cmdline("4").unwrap();
        assert!(s.prompt_text().contains("Enter denominator of smallest fraction"), "{}", s.prompt_text());
        s.cmdline("3").unwrap();
        assert!(s.running.is_some(), "3 is not a power of two");
        s.cmdline("8").unwrap();
        assert_eq!(s.prompt_text(), "-UNITS Enter choice, 1 to 5 <1>:");
        s.cmdline("2").unwrap();
        s.cmdline("").unwrap();
        assert!(s.log.iter().any(|l| l.starts_with("  North 12 o'clock")));
        s.cmdline("90").unwrap();
        assert_eq!(s.prompt_text(), "-UNITS Do you want angles measured clockwise? [Yes/No] <No>:");
        s.cmdline("y").unwrap();
        assert!(s.running.is_none());
        let h = &s.doc().unwrap().header;
        assert_eq!((h.i64("LUNITS", 0), h.i64("LUPREC", 0), h.i64("AUNITS", 0), h.i64("ANGDIR", 0)), (4, 3, 1, 1));
        assert!((h.f64("ANGBASE", 0.0) - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        // The JSON form is unchanged and quiet.
        let r = s.execute("units", &json!({ "lunits": 2 })).unwrap();
        assert_eq!(r["lunits"], 2);
        assert!(s.running.is_none());
    }

    #[test]
    fn dash_style_defines_a_style() {
        let mut s = Session::new();
        s.cmdline("-style").unwrap();
        assert_eq!(s.prompt_text(), "-STYLE Enter name of text style or [?] <Standard>:");
        s.cmdline("?").unwrap();
        s.cmdline("").unwrap();
        assert!(s.log.iter().any(|l| l == "  Standard"), "{:?}", s.log);
        s.cmdline("Notes").unwrap();
        assert!(s.log.iter().any(|l| l == "New style."));
        assert!(s.prompt_text().contains("Specify full font name or font filename (TTF or SHX)"), "{}", s.prompt_text());
        s.cmdline("").unwrap();
        assert_eq!(s.prompt_text(), "-STYLE Specify height of text or [Annotative] <0.0000>:");
        s.cmdline("2.5").unwrap();
        s.cmdline("0.8").unwrap();
        ends_with(&s, "Specify obliquing angle <0>:");
        s.cmdline("15").unwrap();
        assert_eq!(s.prompt_text(), "-STYLE Display text backwards? [Yes/No] <No>:");
        s.cmdline("y").unwrap();
        s.cmdline("").unwrap();
        assert!(s.prompt_text().ends_with("Vertical? [Yes/No] <No>:"), "{}", s.prompt_text());
        s.cmdline("").unwrap();
        assert!(s.running.is_none());
        assert_eq!(s.log.last().map(String::as_str), Some("\"Notes\" is now the current text style."));
        let d = s.doc().unwrap();
        let st = d.text_style("Notes").unwrap();
        assert_eq!((st.height, st.width_factor, st.backwards, st.upside_down), (2.5, 0.8, true, false));
        assert!((st.oblique.to_degrees() - 15.0).abs() < 1e-9);
        assert_eq!(d.header.str("TEXTSTYLE", ""), "Notes");
        // An existing style offers its own values.
        s.cmdline("-style notes").unwrap();
        assert!(s.log.iter().any(|l| l == "Existing style."));
        s.cmdline("").unwrap();
        ends_with(&s, "<2.5000>:");
        s.cancel();
        assert!(s.execute("style", &json!({})).is_err(), "the JSON form still needs a name");
    }
}
