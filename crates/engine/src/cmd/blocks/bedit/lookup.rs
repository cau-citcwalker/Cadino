//! BLOOKUPTABLE: a lookup action's table, as AutoCAD's Property Lookup Table dialog edits it (the
//! UI opens the dialog for BLOOKUPTABLE typed bare): input properties (the parameter values a row
//! matches), the lookup property (the value a matching row gives the lookup parameter), the rows,
//! what a reference shows when no row matches, and whether choosing a lookup value on a reference
//! sets its row's inputs (Allow reverse lookup) or the value is read-only.

use cadcraft_doc::dynblock::{ActionKind, DynamicBlock, ParameterKind, author};
use serde_json::{Value, json};

use super::action::{lookup_property, lookup_table};
use super::{authored, authoring_enabled, parameter_name};
use crate::cmd::{CommandSpec, bad, bool_or};
use crate::{Result, Session};

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("blookuptable", "Lookup Table", run)
            .params("{action?: name or node (a lookup action; default the block's only one), columns?: [{parameter, property?, lookup?: bool, unmatched?, reverse?: bool (a lookup column: Allow reverse lookup)}], rows?: [[cells]], check?: bool (audit the table given: nothing changes)} → {node, name, parameter, columns: [{parameter, label, property, lookup, unmatched, reverse}], rows, available: [{parameter, label, property, lookup}]}; without `columns` it reports the table")
            .enabled(authoring_enabled),
    ]
}

/// The lookup action `v` names (its node or name), else the block's only one.
fn lookup_action(m: &DynamicBlock, v: Option<&Value>) -> std::result::Result<(i32, String, i32), String> {
    let lookups: Vec<_> = m.actions.iter().filter(|a| matches!(a.kind, ActionKind::Lookup { .. })).collect();
    let a = match v {
        Some(v) => lookups
            .iter()
            .find(|a| v.as_i64() == Some(i64::from(a.node)) || v.as_str().is_some_and(|n| n.trim().eq_ignore_ascii_case(&a.name)))
            .ok_or("`action` must name one of the block's lookup actions")?,
        None => match lookups.as_slice() {
            [one] => one,
            [] => return Err("the block has no lookup action: add one first (BACTION Lookup)".into()),
            _ => return Err("`action` is required: the block has more than one lookup action".into()),
        },
    };
    // The lookup parameter it gives a value: its table's (first) lookup column's.
    let parameter = match &a.kind {
        ActionKind::Lookup { columns, .. } => columns.iter().find(|c| c.lookup).map_or(0, |c| c.node),
        _ => 0,
    };
    Ok((a.node, a.name.clone(), parameter))
}

/// Lookup action `node`'s table, and the parameter properties a column can be.
fn report(m: &DynamicBlock, node: i32, parameter: i32) -> Value {
    let a = m.actions.iter().find(|a| a.node == node);
    let (columns, rows) = match a.map(|a| &a.kind) {
        Some(ActionKind::Lookup { columns, rows }) => (columns.clone(), rows.clone()),
        _ => (Vec::new(), Vec::new()),
    };
    let columns: Vec<Value> = columns
        .iter()
        .map(|c| {
            json!({ "parameter": c.node, "label": parameter_name(m, c.node), "property": c.property, "lookup": c.lookup,
                    "unmatched": c.unmatched, "reverse": c.reverse })
        })
        .collect();
    // Input properties: the values CADCraft can match (distances, visibility states); lookup
    // properties: lookup parameters.
    let available: Vec<Value> = m
        .parameters
        .iter()
        .filter_map(|p| {
            let lookup = matches!(p.kind, ParameterKind::Lookup { .. });
            let property = lookup_property(&p.kind)?;
            Some(json!({ "parameter": p.node, "label": parameter_name(m, p.node), "property": property, "lookup": lookup }))
        })
        .collect();
    json!({ "node": node, "name": a.map(|a| a.name.clone()).unwrap_or_default(), "parameter": parameter, "columns": columns, "rows": rows,
            "available": available })
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "blookuptable";
    let m = authored(s)?.ok_or_else(|| bad(cmd, "the block has no lookup action: add one first (BACTION Lookup)"))?;
    let (node, name, parameter) = lookup_action(&m, p.get("action")).map_err(|e| bad(cmd, e))?;
    if p.get("columns").is_some() {
        let ActionKind::Lookup { columns, rows } = lookup_table(cmd, &m, parameter, p)? else {
            return Err(bad(cmd, "not a lookup table"));
        };
        if !bool_or(p, "check", false) {
            super::author(s, |d, block| author::set_lookup(d, block, node, columns, rows))?;
            s.echo(format!("Lookup table of {name} changed."));
        }
    }
    let m = authored(s)?.unwrap_or(m);
    Ok(report(&m, node, parameter))
}
