//! A perspective tile (DVIEW Distance, Rhino's Perspective view) survives a DXF save and open:
//! VIEWMODE (71) bit 1, the lens (42), the target (17) at the view's centre and the camera along
//! the direction (16) as far as its length; parallel tiles keep writing what they did.

use cadcraft_doc::geom::{Camera, Vec2, Vec3, ViewDir};
use cadcraft_doc::{Drawing, VportConfig, VportTile};
use cadcraft_io::{read_dxf, write_dxf};

fn tile(min: (f64, f64), max: (f64, f64)) -> VportTile {
    VportTile {
        min: Vec2::new(min.0, min.1),
        max: Vec2::new(max.0, max.1),
        center: Vec2::new(10.0, 20.0),
        height: 300.0,
        aspect: 1.5,
        ..VportTile::default()
    }
}

#[test]
fn a_perspective_tile_round_trips_through_dxf() {
    let iso = ViewDir::named("iso").expect("iso").dir;
    let persp = VportTile { view_dir: [iso.x, iso.y, iso.z], camera: Some(Camera::new(35.0, 120.0)), ..tile((0.5, 0.5), (1.0, 1.0)) };
    let plan = tile((0.0, 0.5), (0.5, 1.0));
    let mut d = Drawing::new_metric();
    d.vports =
        vec![VportConfig { name: VportConfig::ACTIVE.into(), tiles: vec![persp, plan, tile((0.0, 0.0), (0.5, 0.5)), tile((0.5, 0.0), (1.0, 0.5))] }];
    let r = read_dxf(write_dxf(&d).as_bytes()).expect("reopen");
    let cfg = r.vports.iter().find(|c| c.is_active()).expect("tiles");
    let back = cfg.tiles.first().expect("the perspective tile");
    let cam = back.camera.expect("still perspective");
    assert!((cam.lens - 35.0).abs() < 1e-9 && (cam.target - 120.0).abs() < 1e-6, "{cam:?}");
    assert!(back.center.near(persp.center, 1e-6) && (back.height - persp.height).abs() < 1e-6, "{back:?}");
    let d = ViewDir::new(Vec3::new(back.view_dir[0], back.view_dir[1], back.view_dir[2]));
    assert!((d.dir - iso).len() < 1e-9);
    // The others stay parallel.
    assert!(cfg.tiles.iter().skip(1).all(|t| t.camera.is_none()));
}

/// Another program's perspective record: the target off the origin, the camera 2000 away along
/// 16 and a 24 mm lens; the view's height comes from the lens and that distance.
#[test]
fn another_programs_perspective_vport_reads_its_camera() {
    let rec = [
        (0, "SECTION"),
        (2, "TABLES"),
        (0, "TABLE"),
        (2, "VPORT"),
        (70, "1"),
        (0, "VPORT"),
        (2, "*ACTIVE"),
        (70, "0"),
        (10, "0"),
        (20, "0"),
        (11, "1"),
        (21, "1"),
        (12, "0"),
        (22, "0"),
        (16, "0"),
        (26, "-2000"),
        (36, "0"),
        (17, "100"),
        (27, "50"),
        (37, "700"),
        (40, "123"),
        (41, "1.5"),
        (42, "24"),
        (71, "1"),
        (0, "ENDTAB"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ];
    let text: String = rec.iter().map(|(c, v)| format!("{c}\n{v}\n")).collect();
    let d = read_dxf(text.as_bytes()).expect("read");
    let t = d.vports.iter().find(|c| c.is_active()).and_then(|c| c.tiles.first()).expect("a tile");
    let cam = t.camera.expect("perspective");
    assert_eq!(cam.lens, 24.0);
    // Looking from the front at the target: its depth along (0, -1, 0) is -50.
    assert!((cam.target + 50.0).abs() < 1e-9, "{cam:?}");
    assert!((t.height - cam.height_at(2000.0)).abs() < 1e-9 && (t.center - Vec2::new(100.0, 700.0)).len() < 1e-9, "{t:?}");
}
