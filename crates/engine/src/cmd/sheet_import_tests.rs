//! `sheet.import_flat`: flat patterns made by unfolding parts of known outside dimensions
//! (SPCC 1.6 mm, 90° deduction 2.8 mm; the fixtures of `solidcraft-sheetfold`) fold back into
//! sheet parts of those dimensions, built from ordinary sheet commands.

use serde_json::{Value, json};

use crate::Session;

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    match s.execute(id, &p) {
        Ok(v) => v,
        Err(e) => panic!("{id} {p}: {e}"),
    }
}

fn fixture(name: &str) -> String {
    let path = format!("{}/../sheetfold/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn size(s: &mut Session) -> [f64; 3] {
    let m = run(s, "inspect.measure", json!({}));
    let b = &m["bodies"][0]["bbox"];
    [0, 1, 2].map(|i| b["max"][i].as_f64().unwrap_or(f64::NAN) - b["min"][i].as_f64().unwrap_or(f64::NAN))
}

fn sorted(mut v: [f64; 3]) -> [f64; 3] {
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

fn check(name: &str, expect: [f64; 3]) {
    let mut s = Session::default();
    let r = run(&mut s, "sheet.import_flat", json!({"text": fixture(name), "thickness": 1.6, "d90": 2.8}));
    assert_eq!(s.model.state().sheets.len(), 1, "{name}: one sheet body ({r})");
    let (got, exp) = (sorted(size(&mut s)), sorted(expect));
    for k in 0..3 {
        assert!((got[k] - exp[k]).abs() < 0.05, "{name}: built {got:?}, expected {exp:?} ({r})");
    }
    // The sheet rule gives back the shop's deduction: K from d90 with inner radius = thickness.
    let k = r["k_factor"].as_f64().unwrap_or(f64::NAN);
    assert!((super::k_from_d90(2.8, 1.6, 1.6) - k).abs() < 1e-3, "{r}");
    // One undo step takes the whole part away.
    run(&mut s, "edit.undo", json!({}));
    assert!(s.model.state().sheets.is_empty(), "{name}: undo removes the part");
}

#[test]
fn flat_patterns_open_as_sheet_parts_of_their_outside_dimensions() {
    check("l-50x30.dxf", [50.0, 100.0, 30.0]);
    check("u-30-60-30.dxf", [60.0, 100.0, 30.0]);
    check("box-200x150x40.dxf", [200.0, 150.0, 40.0]);
}

#[test]
fn a_flat_plate_opens_as_a_base_flange() {
    let dxf = "0\nSECTION\n2\nENTITIES\n0\nLWPOLYLINE\n8\n0\n90\n4\n70\n1\n10\n0\n20\n0\n10\n200\n20\n0\n10\n200\n20\n100\n10\n0\n20\n100\n0\nCIRCLE\n8\n0\n10\n50\n20\n50\n40\n5\n0\nENDSEC\n0\nEOF\n";
    let mut s = Session::default();
    let r = run(&mut s, "sheet.import_flat", json!({"text": dxf, "thickness": 2}));
    assert_eq!(r["mode"], "평판");
    assert_eq!(r["flanges"], 0);
    let got = size(&mut s);
    assert!((got[0] - 200.0).abs() < 1e-6 && (got[1] - 100.0).abs() < 1e-6 && (got[2] - 2.0).abs() < 1e-6, "{got:?}");
    // The hole is cut: the plate is 200 × 100 × 2 less the Ø10 hole.
    let vol = run(&mut s, "inspect.measure", json!({}))["total"]["volume_mm3"].as_f64().unwrap_or(f64::NAN);
    let want = 200.0 * 100.0 * 2.0 - std::f64::consts::PI * 25.0 * 2.0;
    assert!((vol - want).abs() / want < 2e-3, "volume {vol}, expected {want}");
}

#[test]
fn bad_input_is_refused_and_leaves_the_design_alone() {
    let mut s = Session::default();
    for p in [
        json!({}),
        json!({"text": "0\nEOF\n"}),
        json!({"text": fixture("l-50x30.dxf"), "thickness": -1}),
        json!({"text": fixture("l-50x30.dxf"), "d90": "x"}),
    ] {
        assert!(s.execute("sheet.import_flat", &p).is_err(), "{p}");
    }
    assert!(s.doc.features.is_empty() && s.undo.is_empty());
}

#[test]
fn doc_open_folds_a_flat_pattern_dxf() {
    let dir = std::env::temp_dir().join(format!("solidcraft-import-flat-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("box.dxf");
    std::fs::write(&path, fixture("sample-box.dxf")).unwrap_or_else(|e| panic!("{e}"));
    let mut s = Session::default();
    run(&mut s, "doc.open", json!({"path": path.to_string_lossy()}));
    assert_eq!(s.model.state().sheets.len(), 1, "a DXF with Nck bend lines opens as a sheet part");
    assert!(super::is_flat_pattern(&fixture("sample-box.dxf")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_drawings_own_deduction_note_is_used() {
    // KerfLine writes the material's 90° deduction into the pattern as `D:2.8`.
    let mut dxf = fixture("box-200x150x40.dxf").replace("\r\n", "\n");
    let at = dxf.find("ENTITIES").and_then(|i| dxf[i..].find("0\nENDSEC").map(|j| i + j)).unwrap_or(0);
    dxf.insert_str(at, "0\nTEXT\n8\n0\n10\n0\n20\n-20\n40\n3\n1\nP:box M:SPCC T:1.6 D:2.8\n");
    let mut s = Session::default();
    let r = run(&mut s, "sheet.import_flat", json!({"text": dxf}));
    assert_eq!((r["name"].as_str(), r["material"].as_str()), (Some("box"), Some("SPCC")));
    let (got, exp) = (sorted(size(&mut s)), sorted([200.0, 150.0, 40.0]));
    for k in 0..3 {
        assert!((got[k] - exp[k]).abs() < 0.05, "built {got:?} ({r})");
    }
}

#[test]
fn the_folded_part_saves_as_step() {
    let mut s = Session::default();
    run(&mut s, "sheet.import_flat", json!({"text": fixture("u-30-60-30.dxf"), "thickness": 1.6, "d90": 2.8}));
    let dir = std::env::temp_dir().join(format!("solidcraft-import-step-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("u.step");
    run(&mut s, "file.export", json!({"path": path.to_string_lossy()}));
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(text.starts_with("ISO-10303-21;") && text.contains("MANIFOLD_SOLID_BREP"), "{}", &text[..text.len().min(200)]);
    let _ = std::fs::remove_dir_all(&dir);
}
