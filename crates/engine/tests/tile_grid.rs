//! Rhino's "Apply grid changes to: active viewport only / all viewports" (OPTIONS `gridViewport`):
//! a model tile with a grid of its own, the grid variables following the current tile, the
//! drawing's grid kept in the profile, and the tile's grid saved with the drawing.

use cadcraft_engine::Session;
use cadcraft_engine::cmd::options;
use serde_json::json;

fn four_views() -> Session {
    let mut s = Session::new();
    s.viewport_px = (1200.0, 800.0);
    s.execute("viewports.rhino", &json!({})).unwrap();
    s
}

fn getvar(s: &Session, name: &str) -> serde_json::Value {
    cadcraft_engine::sysvars::get(s, name).unwrap()
}

#[test]
fn the_active_viewport_gets_a_grid_of_its_own() {
    let mut s = four_views();
    let r = s.execute("options", &json!({"gridViewport": "active", "GRIDUNIT": [1, 1], "GRIDLINECOUNT": 200, "GRIDAXES": 0})).unwrap();
    assert_eq!(r["gridViewport"], "active");
    // The profile (and the report) keep the drawing's grid; GETVAR reads the current tile's.
    assert_eq!((r["GRIDUNIT"].clone(), r["GRIDLINECOUNT"].clone(), r["GRIDAXES"].clone()), (json!([0.5, 0.5]), json!(0), json!(1)));
    assert_eq!((getvar(&s, "GRIDUNIT"), getvar(&s, "GRIDLINECOUNT"), getvar(&s, "GRIDAXES")), (json!([1.0, 1.0]), json!(200), json!(0)));
    let tiles = s.model_tiles();
    let (mine, others): (Vec<&cadcraft_engine::ModelTile>, Vec<_>) = tiles.iter().partition(|t| t.active);
    assert_eq!((mine[0].grid.unit.x, mine[0].grid.count, mine[0].grid.axes), (1.0, 200, false));
    assert!(others.iter().all(|t| t.grid == s.drawing_grid()), "the other tiles show the drawing's grid");
    assert_eq!(s.drafting_settings().gridunit.x, 1.0, "snapping and drawing follow the current tile");

    // In another tile the grid variables are the drawing's again; F7 there leaves the first.
    let other = tiles.iter().position(|t| !t.active).unwrap();
    s.activate_tile(other).unwrap();
    assert_eq!(getvar(&s, "GRIDUNIT"), json!([0.5, 0.5]));
    assert_eq!(s.execute("options", &json!({})).unwrap()["gridViewport"], "all");
    s.execute("grid", &json!({"on": false})).unwrap();
    assert!(!s.drawing_grid().on);
    assert!(s.model_tiles().iter().any(|t| t.grid.on && t.grid.count == 200), "the first tile's own grid still shows");
}

#[test]
fn all_viewports_shows_the_current_grid_everywhere() {
    let mut s = four_views();
    s.execute("options", &json!({"gridViewport": "active", "GRIDUNIT": [2, 2], "GRIDMAJOR": 4})).unwrap();
    // "All viewports" from that tile: its grid becomes the drawing's and every tile shows it.
    s.execute("options", &json!({"gridViewport": "all", "GRIDLINECOUNT": 50})).unwrap();
    assert!(s.tile_grid().is_none());
    let g = s.drawing_grid();
    assert_eq!((g.unit.x, g.major, g.count), (2.0, 4, 50));
    assert!(s.model_tiles().iter().all(|t| t.grid == g));
    assert!(s.execute("options", &json!({"gridViewport": "some"})).is_err());
}

#[test]
fn a_tile_grid_is_saved_with_the_drawing() {
    let mut s = four_views();
    s.execute("options", &json!({"gridViewport": "active", "GRIDUNIT": [1, 1], "SNAPUNIT": [0.25, 0.25], "GRIDLINECOUNT": 200, "GRIDAXES": 0}))
        .unwrap();
    let own = s.tile_grid().unwrap();
    let back = cadcraft_io::read_dxf(cadcraft_io::write_dxf(s.doc().unwrap()).as_bytes()).unwrap();
    let tiles = &back.vports.iter().find(|c| c.is_active()).unwrap().tiles;
    assert_eq!(tiles.iter().filter_map(|t| t.grid).collect::<Vec<_>>(), vec![own], "one tile has its own grid");
    // The drawing's own spacings are the header's, not the current tile's.
    assert_eq!((back.header.grid_unit().x, back.header.snap_unit().x), (0.5, 0.5));
}

/// GRIDLINECOUNT and GRIDAXES are profile settings like GRIDMAJOR: a new drawing keeps them,
/// OPTIONS reset puts them back, and a call that fails leaves them (and the drawing) as they were.
#[test]
fn grid_line_count_and_axes_are_profile_settings() {
    let mut s = Session::new();
    s.execute("options", &json!({"GRIDLINECOUNT": 200, "GRIDAXES": 0})).unwrap();
    s.execute("new", &json!({})).unwrap();
    assert_eq!((getvar(&s, "GRIDLINECOUNT"), getvar(&s, "GRIDAXES")), (json!(200), json!(0)), "a new drawing keeps them");
    let saved = options::profile(&s);
    let mut t = Session::new();
    options::restore(&mut t, &saved);
    assert_eq!((getvar(&t, "GRIDLINECOUNT"), getvar(&t, "GRIDAXES")), (json!(200), json!(0)), "the profile keeps them");

    let doc = s.state().unwrap().doc.clone();
    for bad in [json!({"GRIDLINECOUNT": 300, "GRIDUNIT": [3, 3], "OSMODE": "lots"}), json!({"GRIDLINECOUNT": 1001})] {
        assert!(s.execute("options", &bad).is_err(), "{bad}");
        assert_eq!(getvar(&s, "GRIDLINECOUNT"), json!(200), "{bad}");
        assert!(std::sync::Arc::ptr_eq(&s.state().unwrap().doc, &doc), "{bad}: the drawing's GRIDUNIT is as it was");
    }
    s.execute("options", &json!({"reset": true})).unwrap();
    assert_eq!((getvar(&s, "GRIDLINECOUNT"), getvar(&s, "GRIDAXES")), (json!(0), json!(1)));
}
