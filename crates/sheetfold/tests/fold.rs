//! Fold the flat patterns in `fixtures/` (made by unfolding parts of known outside dimensions with a
//! 90° bend deduction of 2.8 mm, SPCC 1.6 mm) and check the folded part has those dimensions again.

use solidcraft_sheetfold::{FoldOptions, fold_dxf};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn sorted(v: [f64; 3]) -> [f64; 3] {
    let mut v = v;
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

fn check(name: &str, expect: [f64; 3]) {
    let opts = FoldOptions { thickness: Some(1.6), d90: Some(2.8), ..FoldOptions::default() };
    let m = fold_dxf(&fixture(name), &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
    let (got, exp) = (sorted(m.size), sorted(expect));
    for k in 0..3 {
        assert!((got[k] - exp[k]).abs() < 0.02, "{name}: size {:?}, expected {:?} ({:?})", m.size, expect, m.warnings);
    }
    assert_eq!(m.mode, "한 장 전개도", "{name}");
}

#[test]
fn profiles_fold_back_to_their_outside_dimensions() {
    check("l-50x30.dxf", [50.0, 100.0, 30.0]);
    check("u-30-60-30.dxf", [60.0, 100.0, 30.0]);
    // a bend the other way measures from the other face: one thickness less
    check("z-30-40-30.dxf", [58.4, 100.0, 40.0]);
    check("hat.dxf", [66.8, 100.0, 25.0]);
}

#[test]
fn boxes_fold_back_to_their_outside_dimensions() {
    check("box-200x150x40.dxf", [200.0, 150.0, 40.0]);
    check("box-wrap-front-back.dxf", [200.0, 150.0, 40.0]);
    check("box-return-15.dxf", [200.0, 163.4, 40.0]);
}

#[test]
fn an_angle_written_next_to_a_bend_line_is_used() {
    // "45" beside the first bend: 50 + 30·cos45 by 30·sin45
    let s = 45f64.to_radians();
    check("angle-45.dxf", [50.0 + 30.0 * s.cos(), 100.0, 30.0 * s.sin()]);
}

#[test]
fn the_info_block_gives_name_material_and_thickness() {
    let m = fold_dxf(&fixture("sample-box.dxf"), &FoldOptions { d90: Some(2.8), ..FoldOptions::default() }).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(m.info.material.as_deref(), Some("SPCC"));
    assert!((m.thickness - 1.6).abs() < 1e-9);
    assert_eq!(m.info.qty, Some(1));
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    // the model is plain data: it serialises for the KerfLine side
    let json = serde_json::to_string(&m).unwrap_or_default();
    assert!(json.contains("\"faces\""));
}

#[test]
fn each_folded_plate_reports_its_outside_dimension() {
    let opts = FoldOptions { thickness: Some(1.6), d90: Some(2.8), ..FoldOptions::default() };
    let m = fold_dxf(&fixture("box-return-15.dxf"), &opts).unwrap_or_else(|e| panic!("{e}"));
    let mut outer: Vec<f64> = m.faces.iter().filter_map(|f| f.outer).collect();
    outer.sort_by(|a, b| a.total_cmp(b));
    let want = [15.0, 15.0, 40.0, 40.0, 40.0, 40.0];
    assert_eq!(outer.len(), want.len(), "{outer:?}");
    for (g, w) in outer.iter().zip(want) {
        assert!((g - w).abs() < 0.02, "{outer:?}");
    }
}

#[test]
fn without_a_deduction_a_flat_pattern_still_folds_and_says_why_it_is_short() {
    let m = fold_dxf(&fixture("box-200x150x40.dxf"), &FoldOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    assert!(m.warnings.iter().any(|w| w.contains("연신율")));
}

#[test]
fn hostile_input_never_panics() {
    let opts = FoldOptions::default();
    for text in ["", "0\nEOF\n", "0\nSECTION\n2\nENTITIES\n0\nLINE\n8\nNck-DG\n10\nNaN\n20\ninf\n0\nENDSEC\n0\nEOF\n", "garbage\n\n\n"] {
        let _ = fold_dxf(text, &opts);
    }
}

/// A small DXF: closed polylines on layer 0, lines on the given layers.
fn dxf(polys: &[Vec<[f64; 2]>], lines: &[(&str, [f64; 2], [f64; 2])]) -> String {
    let mut o = String::from("0\nSECTION\n2\nENTITIES\n");
    for p in polys {
        o += &format!("0\nLWPOLYLINE\n8\n0\n90\n{}\n70\n1\n", p.len());
        for q in p {
            o += &format!("10\n{}\n20\n{}\n", q[0], q[1]);
        }
    }
    for (layer, a, b) in lines {
        o += &format!("0\nLINE\n8\n{layer}\n10\n{}\n20\n{}\n11\n{}\n21\n{}\n", a[0], a[1], b[0], b[1]);
    }
    o + "0\nENDSEC\n0\nEOF\n"
}
fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<[f64; 2]> {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}
fn circle(cx: f64, cy: f64, r: f64, n: usize) -> Vec<[f64; 2]> {
    (0..n)
        .map(|k| {
            let a = k as f64 / n as f64 * std::f64::consts::TAU;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

/// 120 × 37.2 flat pattern of a 120 × 30 × 5 channel (0.8 t, deduction 1.4), two back bends,
/// with a Ø22 hole (drawn as a fine polyline) right at the middle of the web and two slots.
fn channel_with_holes() -> String {
    let slot = |x0: f64, x1: f64| {
        let mut p: Vec<[f64; 2]> = (0..=32)
            .map(|k| {
                let a = -std::f64::consts::FRAC_PI_2 + k as f64 / 32.0 * std::f64::consts::PI;
                [x1 + 2.5 * a.cos(), 18.6 + 2.5 * a.sin()]
            })
            .collect();
        p.extend((0..=32).map(|k| {
            let a = std::f64::consts::FRAC_PI_2 + k as f64 / 32.0 * std::f64::consts::PI;
            [x0 + 2.5 * a.cos(), 18.6 + 2.5 * a.sin()]
        }));
        p
    };
    dxf(
        &[
            rect(0.0, 0.0, 120.0, 37.2),
            circle(60.0, 18.6, 11.0, 128),
            circle(5.0, 18.6, 2.1, 128),
            circle(115.0, 18.6, 2.1, 128),
            slot(15.0, 40.0),
            slot(80.0, 105.0),
        ],
        &[("Nck-DF", [0.0, 4.3], [120.0, 4.3]), ("Nck-DF", [0.0, 32.9], [120.0, 32.9])],
    )
}

#[test]
fn a_hole_at_the_middle_of_a_plate_does_not_hide_the_plate() {
    let m = fold_dxf(&channel_with_holes(), &FoldOptions { thickness: Some(0.8), d90: Some(1.4), ..FoldOptions::default() })
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(m.mode, "한 장 전개도");
    assert_eq!(m.faces.len(), 3, "{:?}", m.warnings);
    let (got, exp) = (sorted(m.size), sorted([120.0, 30.0, 5.0]));
    for k in 0..3 {
        assert!((got[k] - exp[k]).abs() < 0.02, "size {:?}", m.size);
    }
    let root = m.faces.iter().find(|f| f.root).unwrap_or_else(|| panic!("no root"));
    assert_eq!(root.holes.len() + root.circles.len(), 5);
}

#[test]
fn every_plate_has_the_outside_dimension_its_bends_make() {
    let m = fold_dxf(&channel_with_holes(), &FoldOptions { thickness: Some(0.8), d90: Some(1.4), ..FoldOptions::default() })
        .unwrap_or_else(|e| panic!("{e}"));
    let mut v: Vec<f64> = m.dims.iter().map(|d| d.v).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    assert_eq!(v, vec![5.0, 5.0, 30.0]);
    for d in &m.dims {
        let len = ((d.b[0] - d.a[0]).powi(2) + (d.b[1] - d.a[1]).powi(2) + (d.b[2] - d.a[2]).powi(2)).sqrt();
        assert!((len - d.v).abs() < 0.01, "{d:?}");
    }
}

#[test]
fn without_bend_lines_the_part_is_a_plate_one_thickness_thick() {
    let m = fold_dxf(
        &dxf(&[rect(0.0, 0.0, 200.0, 100.0), circle(50.0, 50.0, 5.0, 32)], &[]),
        &FoldOptions { thickness: Some(1.6), ..FoldOptions::default() },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(m.mode, "평판");
    assert_eq!(m.size, [200.0, 100.0, 1.6]);
    assert!(m.dims.is_empty());
}
