//! BEDIT, BSAVE, BSAVEAS and BCLOSE: the Block Editor. BEDIT opens a block definition in the
//! drawing's own tab, its base point at the origin; the block's objects are edited with the
//! ordinary commands; BSAVE saves them into the definition, so every reference to the block in the
//! drawing shows the change; BCLOSE goes back to the drawing, saving or discarding what isn't saved.
//! The state is [`crate::blockedit`].

use cadcraft_doc::dynblock::{DynamicBlock, ParameterKind, author, dynamic_block};
use cadcraft_doc::{Drawing, EntityKind, Handle};
use cadcraft_geom::{Vec2, Vec3};
use serde_json::{Value, json};

use crate::cmd::curves::{KW, handle_value};
use crate::cmd::{CommandSpec, bad, bool_or, str_param};
use crate::{Accept, EngineError, Input, Interactive, Prompt, Result, Session, Step};

mod action;
mod param;
mod vstate;

pub(super) fn specs() -> Vec<CommandSpec> {
    let mut v = authoring_specs();
    v.extend(param::specs());
    v.extend(action::specs());
    v.extend(vstate::specs());
    v
}

fn authoring_specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("bedit", "Block Editor", run_bedit)
            .menu(&["Tools", "Block Editor"])
            .alias(&["be", "-bedit"])
            .params("{name? (\"<Current Drawing>\": the drawing itself, a dynamic block's own drawing) | handle? (a block reference: edits its block); default the one selected block reference, create?: bool (a name no block has yet starts a new block; BSAVE creates it)} → {block, new}")
            .enabled(not_editing)
            .noundo()
            .interactive(|_| Ok(Box::new(BeditM))),
        CommandSpec::new("bsave", "Save Block", run_bsave).params("{} → {block}").enabled(not_testing).noundo(),
        CommandSpec::new("bsaveas", "Save Block As", run_bsaveas)
            .params("{name, replace?: bool (redefine an existing block of that name)} → {block}")
            .enabled(|s| editing_a_block(s).and_then(|_| not_testing(s)))
            .noundo()
            .interactive(|_| Ok(Box::new(BsaveasM::default()))),
        CommandSpec::new("btestblock", "Test Block", run_btestblock)
            .menu(&["Tools", "Block Editor", "Test Block"])
            .params("{} → {block, handle}: a test block window with one reference to the block as edited, at the origin; BCLOSE closes it, keeping nothing")
            .enabled(not_testing)
            .noundo(),
        CommandSpec::new("bclose", "Close Block Editor", run_bclose)
            .params("{save?: bool (default true; false discards the changes not saved into the block)} → {block, saved}; in the test block window it closes the window")
            .enabled(editing)
            .noundo()
            .interactive(|_| Ok(Box::new(BcloseM))),
    ]
}

fn editing(s: &Session) -> std::result::Result<(), String> {
    match s.state() {
        Ok(st) if st.block_edit.is_some() => Ok(()),
        Ok(_) => Err("The Block Editor is not open.".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// The Block Editor is open and the test block window isn't.
fn not_testing(s: &Session) -> std::result::Result<(), String> {
    editing(s)?;
    match s.state() {
        Ok(st) if st.block_testing() => Err("Not available in the test block window; close it first (BCLOSE).".into()),
        _ => Ok(()),
    }
}

/// The Block Editor is open on a block (not on the drawing itself, which BSAVEAS can't save as one).
fn editing_a_block(s: &Session) -> std::result::Result<(), String> {
    editing(s)?;
    match s.state() {
        Ok(st) if st.block_edit.as_ref().is_some_and(|b| b.current) => {
            Err("The Block Editor is open on the current drawing; there is no block to save under another name.".into())
        }
        _ => Ok(()),
    }
}

fn not_editing(s: &Session) -> std::result::Result<(), String> {
    match s.state() {
        Ok(st) => match st.block_editor() {
            Some(name) => Err(format!("The Block Editor is already open on \"{name}\"; close it first (BCLOSE).")),
            None => Ok(()),
        },
        Err(e) => Err(e.to_string()),
    }
}

/// Commands that leave model space, which the Block Editor is: not available while it is open.
const NOT_IN_EDITOR: &[&str] = &[
    "layout",
    "layout.new",
    "layout.delete",
    "layout.rename",
    "layout.copy",
    "layout.set",
    "mview",
    "mview.polygonal",
    "mspace",
    "pspace",
    "chspace",
];

/// Why command `id` can't run now, when the Block Editor is open and it is one of [`NOT_IN_EDITOR`].
pub(crate) fn refused_in_editor(s: &Session, id: &str) -> Option<String> {
    let name = s.state().ok()?.block_editor()?;
    NOT_IN_EDITOR.contains(&id).then(|| format!("Not available in the Block Editor (editing \"{name}\"); close it first (BCLOSE)."))
}

/// The block a block reference refers to.
fn block_of(d: &Drawing, h: Handle) -> Option<String> {
    match d.entity(h).map(|e| &e.kind) {
        Some(EntityKind::Insert(i)) => Some(dynamic_definition(d, &i.block).unwrap_or_else(|| i.block.clone())),
        _ => None,
    }
}

/// The dynamic block whose anonymous block `name` is (a dynamic block reference with values of
/// its own shows one, `*U12`): its BLOCK_RECORD's `AcDbBlockRepBTag` xdata names the dynamic
/// block's record. `None` for other blocks.
fn dynamic_definition(d: &Drawing, name: &str) -> Option<String> {
    if !name.starts_with('*') {
        return None;
    }
    let x = &d.foreign.records.get(&("BLOCK_RECORD".to_string(), name.to_ascii_uppercase()))?.xdata;
    let at = x.iter().position(|t| t.code == 1001 && t.value.eq_ignore_ascii_case("AcDbBlockRepBTag"))?;
    let record = x.iter().skip(at + 1).take_while(|t| t.code != 1001).find(|t| t.code == 1005).and_then(|t| Handle::parse_hex(&t.value))?;
    let (kind, block) = d.foreign.targets.get(&record)?;
    (kind == "BLOCK_RECORD").then(|| d.block(block).map(|b| b.name.clone())).flatten()
}

/// Open the Block Editor on block `name` and frame its objects.
fn open(s: &mut Session, name: &str, create: bool) -> Result<Value> {
    let new = create && !name.eq_ignore_ascii_case(crate::blockedit::CURRENT_DRAWING) && s.doc()?.block(name).is_none();
    s.state_mut()?.open_block_editor(name, create)?;
    let name = s.state()?.block_editor().unwrap_or(name).to_string();
    // The block's objects with room around them (and the editor's bar above them).
    s.zoom_extents()?;
    let c = s.state()?.view().center;
    s.zoom_about(0.75, c)?;
    s.echo(if new {
        format!("Block Editor: new block \"{name}\". BSAVE creates it in the drawing; BCLOSE closes the editor.")
    } else {
        format!("Block Editor: editing \"{name}\". BSAVE saves it into the drawing; BCLOSE closes the editor.")
    });
    let dynamic =
        |be: &crate::blockedit::BlockEdit| crate::blockedit::has_dynamic_data(&be.host.doc, if be.current { "*Model_Space" } else { &be.name });
    if s.state().is_ok_and(|st| st.block_edit.as_ref().is_some_and(dynamic)) {
        s.echo("This is a dynamic block: BPARAMETER, BACTION and BVSTATE add to its parameters, actions and visibility states.");
    }
    Ok(json!({ "block": name, "new": new }))
}

/// The one block reference selected, if exactly one is.
fn selected_reference(s: &Session) -> Option<String> {
    let d = s.doc().ok()?;
    let refs: Vec<String> = s.selection().iter().filter_map(|h| block_of(d, *h)).collect();
    match refs.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn run_bedit(s: &mut Session, p: &Value) -> Result<Value> {
    let name = match (str_param(p, "name"), p.get("handle")) {
        (Some(n), _) => n.to_string(),
        (None, Some(h)) => {
            let h = handle_value(h).ok_or_else(|| bad("bedit", "`handle` must be a handle"))?;
            block_of(s.doc()?, h).ok_or_else(|| bad("bedit", "`handle` is not a block reference"))?
        }
        (None, None) => selected_reference(s).ok_or_else(|| bad("bedit", "`name` or `handle` is required (or select one block reference)"))?,
    };
    let create = bool_or(p, "create", false);
    let name = name.trim();
    let current = name.eq_ignore_ascii_case(crate::blockedit::CURRENT_DRAWING);
    if create && !current && s.doc()?.block(name).is_none() && !super::valid_block_name(name) {
        return Err(bad("bedit", format!("\"{name}\" is not a valid block name")));
    }
    open(s, name, create)
}

fn run_bsave(s: &mut Session, _p: &Value) -> Result<Value> {
    s.state_mut()?.save_block_editor(None)?;
    let name = s.state()?.block_editor().unwrap_or_default().to_string();
    s.echo(format!("Block \"{name}\" saved."));
    Ok(json!({ "block": name }))
}

/// Check a BSAVEAS name: valid, and not another existing block unless `replace`.
fn check_save_as(s: &Session, name: &str, replace: bool) -> Result<()> {
    if !super::valid_block_name(name) {
        return Err(bad("bsaveas", format!("\"{name}\" is not a valid block name")));
    }
    let d = s.doc()?;
    let current = s.state()?.block_editor().is_some_and(|b| b.eq_ignore_ascii_case(name));
    if !current && !replace && d.block(name).is_some() {
        return Err(bad("bsaveas", format!("Block \"{name}\" already exists; pass `replace: true` to redefine it")));
    }
    if let Some(why) = d.block(name).filter(|_| !current).and_then(|_| crate::blockedit::refusal(d, name)) {
        return Err(bad("bsaveas", why));
    }
    Ok(())
}

fn run_bsaveas(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_param(p, "name").ok_or_else(|| bad("bsaveas", "`name` is required"))?.trim().to_string();
    check_save_as(s, &name, bool_or(p, "replace", false))?;
    s.state_mut()?.save_block_editor(Some(&name))?;
    let name = s.state()?.block_editor().unwrap_or_default().to_string();
    s.echo(format!("Block \"{name}\" saved; the Block Editor now edits it."));
    Ok(json!({ "block": name }))
}

fn close(s: &mut Session, save: bool) -> Result<Value> {
    let st = s.state()?;
    let name = st.block_editor().unwrap_or_default().to_string();
    let saved = save && st.block_unsaved();
    s.state_mut()?.close_block_editor(save)?;
    if saved {
        s.echo(format!("Block \"{name}\" saved."));
    }
    Ok(json!({ "block": name, "saved": saved }))
}

fn run_bclose(s: &mut Session, p: &Value) -> Result<Value> {
    if s.state_mut()?.end_block_test() {
        s.echo("Test block window closed.");
        let name = s.state()?.block_editor().unwrap_or_default().to_string();
        return Ok(json!({ "block": name, "saved": false }));
    }
    close(s, bool_or(p, "save", true))
}

fn run_btestblock(s: &mut Session, _p: &Value) -> Result<Value> {
    let name = s.state_mut()?.start_block_test()?;
    let r = crate::cmd::run_body(s, "insert", &json!({ "name": name, "at": [0, 0] }));
    let r = match r {
        Ok(r) => r,
        Err(e) => {
            s.state_mut()?.end_block_test();
            return Err(e);
        }
    };
    // The window starts with nothing to undo.
    let st = s.state_mut()?;
    st.undo.clear();
    st.redo.clear();
    s.echo(format!("Test block window: \"{name}\". Select the reference to try its grips; BCLOSE closes the window."));
    Ok(json!({ "block": name, "handle": r.get("handle").cloned().unwrap_or(Value::Null) }))
}

// ---------------- interactive ----------------

/// BEDIT: the selected block reference's block, else a block name typed or a reference picked.
struct BeditM;

impl Interactive for BeditM {
    fn name(&self) -> &'static str {
        "BEDIT"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        match selected_reference(s) {
            Some(name) => open(s, &name, false).map(|_| Step::Done),
            None => Ok(Step::Continue),
        }
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        Prompt::new("Enter block name or select a block reference [?]", Accept { point: true, number: false, text: true, select: true, enter: true })
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        match i {
            Input::Text(t) if t.trim() == "?" => {
                let names: Vec<String> = {
                    let d = s.doc()?;
                    d.blocks.values().filter(|b| crate::blockedit::refusal(d, &b.name).is_none()).map(|b| b.name.clone()).collect()
                };
                s.echo(if names.is_empty() { "No blocks to edit.".to_string() } else { names.join(", ") });
                Ok(Step::Continue)
            }
            Input::Text(t) | Input::Keyword(t) => open(s, t.trim(), false).map(|_| Step::Done),
            Input::Pick(hs) => {
                let name = {
                    let d = s.doc()?;
                    hs.iter().find_map(|h| block_of(d, *h))
                };
                match name {
                    Some(n) => open(s, &n, false).map(|_| Step::Done),
                    None => Err(EngineError::Other("Select a block reference.".into())),
                }
            }
            Input::Enter => Ok(Step::Done),
            _ => Ok(Step::Continue),
        }
    }
}

/// BCLOSE: asks whether to save changes not saved yet.
struct BcloseM;

impl Interactive for BcloseM {
    fn name(&self) -> &'static str {
        "BCLOSE"
    }
    fn begin(&mut self, s: &mut Session) -> Result<Step> {
        if s.state()?.block_unsaved() { Ok(Step::Continue) } else { close(s, false).map(|_| Step::Done) }
    }
    fn prompt(&self, s: &Session) -> Prompt {
        let name = s.state().ok().and_then(|st| st.block_editor().map(str::to_string)).unwrap_or_default();
        Prompt::new(format!("Save changes to block \"{name}\"?"), KW).kw(&["Yes", "No"]).default("Y")
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        let answer = match i {
            Input::Enter => "Y".to_string(),
            Input::Keyword(k) | Input::Text(k) => k,
            _ => return Ok(Step::Continue),
        };
        match answer.trim().chars().next().map(|c| c.to_ascii_uppercase()) {
            Some('Y') => close(s, true).map(|_| Step::Done),
            Some('N') => close(s, false).map(|_| Step::Done),
            _ => Err(EngineError::Other("Enter Yes or No.".into())),
        }
    }
}

/// BSAVEAS: a new block name; an existing block is redefined only when confirmed.
#[derive(Default)]
struct BsaveasM {
    /// The name given, waiting for "redefine?" because a block of that name exists.
    name: Option<String>,
}

impl BsaveasM {
    fn save(s: &mut Session, name: &str, replace: bool) -> Result<Step> {
        run_bsaveas(s, &json!({ "name": name, "replace": replace })).map(|_| Step::Done)
    }
}

impl Interactive for BsaveasM {
    fn name(&self) -> &'static str {
        "BSAVEAS"
    }
    fn prompt(&self, _s: &Session) -> Prompt {
        match &self.name {
            Some(n) => Prompt::new(format!("Block \"{n}\" already exists. Redefine it?"), KW).kw(&["Yes", "No"]).default("N"),
            None => Prompt::new("Enter block name", Accept::TEXT),
        }
    }
    fn input(&mut self, s: &mut Session, i: Input) -> Result<Step> {
        if let Some(n) = self.name.clone() {
            let yes = matches!(&i, Input::Keyword(k) | Input::Text(k) if k.trim().to_ascii_uppercase().starts_with('Y'));
            return if yes { Self::save(s, &n, true) } else { Ok(Step::Done) };
        }
        let name = match i {
            Input::Text(t) => t.trim().to_string(),
            Input::Enter => return Ok(Step::Done),
            _ => return Ok(Step::Continue),
        };
        let current = s.state()?.block_editor().is_some_and(|b| b.eq_ignore_ascii_case(&name));
        if !current && s.doc()?.block(&name).is_some() {
            check_save_as(s, &name, true)?;
            self.name = Some(name);
            return Ok(Step::Continue);
        }
        Self::save(s, &name, false)
    }
}

// ---------------- authoring (BPARAMETER, BACTION, BVSTATE…) ----------------

/// The block the Block Editor authors (the name its dynamic data goes by) and its base point.
fn authoring(s: &Session) -> Result<(String, Vec2)> {
    let st = s.state()?;
    let be = st.block_edit.as_ref().ok_or_else(|| {
        EngineError::Other("Parameters, actions and visibility states belong to a block: open the Block Editor first (BEDIT).".into())
    })?;
    Ok((be.dynamic_name().to_string(), be.base()))
}

/// The dynamic block the Block Editor authors, if it has parameters or actions yet.
fn authored(s: &Session) -> Result<Option<DynamicBlock>> {
    let (name, _) = authoring(s)?;
    Ok(dynamic_block(s.doc()?, &name))
}

/// A point of the editor in block coordinates (the base point is at the editor's origin).
fn to_block(base: Vec2, p: Vec2) -> Vec3 {
    (p + base).to3(0.0)
}

/// A block point in the editor.
fn to_editor(base: Vec2, p: Vec3) -> Vec2 {
    p.xy() - base
}

/// Change the editor's drawing (the block's dynamic data) with `f`, then show the current
/// visibility state again.
fn author<R>(s: &mut Session, f: impl FnOnce(&mut Drawing, &str) -> std::result::Result<R, String>) -> Result<R> {
    let (name, _) = authoring(s)?;
    let mut d = s.doc()?.clone();
    let r = f(&mut d, &name).map_err(EngineError::Other)?;
    *s.doc_mut()? = d;
    show_state(s)?;
    Ok(r)
}

/// Show the editor's visibility state: objects it hides are hidden in the editor.
// ponytail: hidden objects are not drawn (BVMODE 0); BVMODE 1 (shown dimmed) would need the
// renderer to draw them faded.
fn show_state(s: &mut Session) -> Result<()> {
    let (name, state) = {
        let st = s.state()?;
        let Some(be) = st.block_edit.as_ref() else { return Ok(()) };
        (be.dynamic_name().to_string(), be.vstate.clone())
    };
    let Some(m) = dynamic_block(s.doc()?, &name) else { return Ok(()) };
    let changes: Vec<(Handle, bool)> = {
        let d = s.doc()?;
        author::state_visibility(&m, state.as_deref()).into_iter().filter(|(h, v)| d.model.get(*h).is_some_and(|e| e.common.visible != *v)).collect()
    };
    if changes.is_empty() {
        return Ok(());
    }
    let d = s.doc_mut()?;
    for (h, v) in changes {
        d.modify_entity(h, |e| e.common.visible = v)?;
    }
    Ok(())
}

/// The parameter `v` names: its node id, or one of its property labels (any case).
fn parameter_named(m: &DynamicBlock, v: &Value) -> Option<i32> {
    if let Some(n) = v.as_i64() {
        return m.parameters.iter().find(|p| i64::from(p.node) == n).map(|p| p.node);
    }
    let t = v.as_str()?.trim();
    m.parameters
        .iter()
        .find(|p| p.kind.labels().iter().any(|l| l.eq_ignore_ascii_case(t)) || (p.kind.label().is_none() && p.element.eq_ignore_ascii_case(t)))
        .map(|p| p.node)
}

/// The parameter nearest editor point `at` within `tol` (its points and the line between them).
fn parameter_at(m: &DynamicBlock, base: Vec2, at: Vec2, tol: f64) -> Option<i32> {
    let mut best: Option<(f64, i32)> = None;
    for p in &m.parameters {
        let Some((a, b)) = p.kind.points() else { continue };
        let (a, b) = (to_editor(base, a), to_editor(base, b));
        let mut dist = cadcraft_geom::Line { a, b }.dist(at);
        if let ParameterKind::Flip { label_at, .. } = &p.kind {
            dist = dist.min((to_editor(base, *label_at) - at).len());
        }
        if dist <= tol && best.is_none_or(|(d, _)| dist < d) {
            best = Some((dist, p.node));
        }
    }
    best.map(|(_, n)| n)
}

/// How a parameter is called in messages: its label, else its element name.
fn parameter_name(m: &DynamicBlock, node: i32) -> String {
    m.parameters.iter().find(|p| p.node == node).map(|p| p.kind.label().map_or_else(|| p.element.clone(), str::to_string)).unwrap_or_default()
}

/// The Block Editor is open (and not its test block window): the authoring commands work in it.
fn authoring_enabled(s: &Session) -> std::result::Result<(), String> {
    editing(s).map_err(|_| "Parameters, actions and visibility states are added in the Block Editor (BEDIT).".to_string())?;
    not_testing(s)
}
