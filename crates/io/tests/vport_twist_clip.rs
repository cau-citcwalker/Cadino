//! A tile's DVIEW twist and clipping planes survive a DXF save and open: the view twist (VPORT
//! 51, degrees), the front and back planes (43 and 44, from the target 17) and the VIEWMODE (71)
//! bits 2, 4 and 16, in parallel and in perspective; another program's front plane at the eye
//! (bit 16 clear) clips nothing more than the near plane, and hostile values read as none.

use cadcraft_doc::geom::{Camera, Clip, Vec2, ViewDir};
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
fn twist_and_clipping_planes_round_trip_through_dxf() {
    let iso = ViewDir::named("iso").expect("iso").dir;
    let twisted = VportTile { view_dir: [iso.x, iso.y, iso.z], twist: 0.5, clip: Clip::new(Some(40.0), Some(-25.0)), ..tile((0.0, 0.0), (0.5, 1.0)) };
    let persp = VportTile {
        view_dir: [iso.x, iso.y, iso.z],
        camera: Some(Camera::new(35.0, 120.0)),
        twist: 1.25,
        clip: Clip::new(None, Some(60.0)),
        ..tile((0.5, 0.0), (1.0, 1.0))
    };
    let mut d = Drawing::new_metric();
    d.vports = vec![VportConfig { name: VportConfig::ACTIVE.into(), tiles: vec![twisted, persp] }];
    let text = write_dxf(&d);
    let r = read_dxf(text.as_bytes()).expect("reopen");
    let tiles = &r.vports.iter().find(|c| c.is_active()).expect("tiles").tiles;
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    let (a, b) = (&tiles[0], &tiles[1]);
    assert!(near(a.twist, 0.5) && near(a.center.x, 10.0) && near(a.center.y, 20.0), "{a:?}");
    assert!(a.clip.front.is_some_and(|f| near(f, 40.0)) && a.clip.back.is_some_and(|b| near(b, -25.0)), "{:?}", a.clip);
    assert!(near(b.twist, 1.25) && b.camera.is_some(), "{b:?}");
    assert!(b.clip.front.is_none() && b.clip.back.is_some_and(|x| near(x, 60.0)), "{:?}", b.clip);
    // The perspective tile's centre and camera come back where they were, twisted.
    assert!(near(b.center.x, 10.0) && near(b.center.y, 20.0), "{b:?}");
    assert!(b.camera.is_some_and(|c| near(c.lens, 35.0) && near(c.target, 120.0)));
    // An upright, unclipped tile writes neither (VIEWMODE stays 0 for it).
    let mut plain = Drawing::new_metric();
    plain.vports = vec![VportConfig { name: VportConfig::ACTIVE.into(), tiles: vec![tile((0.0, 0.0), (1.0, 1.0))] }];
    let r = read_dxf(write_dxf(&plain).as_bytes()).expect("reopen");
    assert!(r.vports.iter().flat_map(|c| &c.tiles).all(|t| t.twist == 0.0 && t.clip.is_off()));
}

/// A VPORT record from another program: twist 30°, a front plane at the eye (VIEWMODE 2 without
/// 16: none here), a back plane 5 behind the target; then hostile twist and planes.
#[test]
fn foreign_and_hostile_vport_twist_and_planes() {
    let record = |extra: &str| {
        format!(
            "0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nVPORT\n70\n1\n0\nVPORT\n2\n*ACTIVE\n70\n0\n10\n0.0\n20\n0.0\n11\n1.0\n21\n1.0\n12\n5.0\n22\n5.0\n16\n0.0\n26\n0.0\n36\n1.0\n17\n0.0\n27\n0.0\n37\n2.0\n40\n10.0\n41\n1.0\n{extra}0\nENDTAB\n0\nENDSEC\n0\nEOF\n"
        )
    };
    let tile = |extra: &str| {
        let d = read_dxf(record(extra).as_bytes()).expect("reads");
        d.vports.iter().find(|c| c.is_active()).and_then(|c| c.tiles.first().copied()).expect("a tile")
    };
    let t = tile("43\n3.0\n44\n-5.0\n51\n30.0\n71\n6\n");
    assert!((t.twist - 30f64.to_radians()).abs() < 1e-12);
    // Plan: depth is z, the target at z = 2.
    assert_eq!((t.clip.front, t.clip.back), (None, Some(-3.0)));
    let t = tile("43\n3.0\n71\n18\n");
    assert_eq!(t.clip.front, Some(5.0));
    // Hostile numbers: no twist, and planes at finite depths or none.
    let d = read_dxf(record("43\nnan\n44\n1e999\n51\ninf\n71\n22\n").as_bytes()).expect("reads");
    let finite = |p: Option<f64>| p.is_none_or(f64::is_finite);
    assert!(d.vports.iter().flat_map(|c| &c.tiles).all(|t| t.twist == 0.0 && finite(t.clip.front) && finite(t.clip.back)));
}
