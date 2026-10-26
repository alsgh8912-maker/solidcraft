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
    check("z-30-40-30.dxf", [60.0, 100.0, 40.0]);
    check("hat.dxf", [70.0, 100.0, 25.0]);
}

#[test]
fn boxes_fold_back_to_their_outside_dimensions() {
    check("box-200x150x40.dxf", [200.0, 150.0, 40.0]);
    check("box-wrap-front-back.dxf", [200.0, 150.0, 40.0]);
    check("box-return-15.dxf", [200.0, 165.0, 40.0]);
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
