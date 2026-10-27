//! Open a flat pattern as a sheet-metal part: a laser DXF (KerfLine's, or any with bend lines on
//! the `Nck-DG` / `Nck-DF` layers) is folded back by `solidcraft-sheetfold` and rebuilt from
//! ordinary commands — a sheet rule from the thickness and the 90° bend deduction, the base plate
//! as a base flange (its holes cut), and every chain of plates hanging off it as an edge flange
//! with its further bends — so the result is an editable sheet part like one modelled by hand.

use serde_json::{Value, json};
use solidcraft_sheetfold::{Face, FoldOptions, Model, P2, Rigid, fold_dxf};

use super::CommandSpec;
use crate::params::{bad, num, str_};
use crate::schema::sheet_import::{ImportFlatParams, ImportFlatResult};
use crate::schema::{TypedContract, output_schema_for};
use crate::{EngineError, Result, Session};

const IMPORT_FLAT_CONTRACT: TypedContract<ImportFlatParams> = TypedContract::new();

pub static COMMANDS: &[CommandSpec] = &[CommandSpec::new("sheet.import_flat", "Open Flat Pattern", crate::checked!("sheet.import_flat", import_flat))
    .at("SHEET METAL", "CREATE")
    .icon("unfold")
    .contract(&IMPORT_FLAT_CONTRACT)
    .output(output_schema_for::<ImportFlatResult>)
    .params(
        "text | path: a flat-pattern DXF (outline and holes on any layer; bend lines on Nck-DG (towards the viewer) or Nck-DF (away); \
         a number beside a bend line is its angle, else 90; a note `P:name M:material T:thickness D:deduction` is read); thickness? (mm; default: the drawing's T:, else 1); \
         d90? (the 90° bend deduction, mm: the sheet rule's K-factor comes from it with the inner radius = thickness; default: the drawing's D:, else K 0.44); \
         name? (the body). Folds the pattern back to its outside dimensions and builds the part: sheet rule, base flange, edge flanges. One undo step",
    )];

/// Largest DXF read from a file.
const MAX_DXF_BYTES: usize = 64 << 20;
/// Most edge flanges built from one pattern (each chain of plates off the base is one).
const MAX_FLANGES: usize = 256;

/// K-factor that gives the 90° deduction `d90` with inner radius `r` and thickness `t`.
pub fn k_from_d90(d90: f64, t: f64, r: f64) -> f64 {
    ((2.0 * (r + t) - d90) / std::f64::consts::FRAC_PI_2 - r) / t
}

fn f4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn invert(m: &Rigid) -> Rigid {
    let r = [m.r[0], m.r[3], m.r[6], m.r[1], m.r[4], m.r[7], m.r[2], m.r[5], m.r[8]];
    let t = [0, 1, 2].map(|i| -(r[i * 3] * m.t[0] + r[i * 3 + 1] * m.t[1] + r[i * 3 + 2] * m.t[2]));
    Rigid { r, t }
}

/// Where `child` meets `parent`: the longest run of the child's outline lying on the parent's
/// plane, in the parent's drawing coordinates.
fn shared_edge(parent: &Face, child: &Face) -> Option<(P2, P2)> {
    let inv = invert(&parent.place);
    let on: Vec<[f64; 3]> = child.pts.iter().map(|p| inv.apply(child.place.apply([p[0], p[1], 0.0]))).filter(|q| q[2].abs() < 1e-3).collect();
    let mut best: Option<(f64, P2, P2)> = None;
    for i in 0..on.len() {
        for j in i + 1..on.len() {
            let d = (on[i][0] - on[j][0]).hypot(on[i][1] - on[j][1]);
            if best.is_none_or(|b| d > b.0) {
                best = Some((d, [on[i][0], on[i][1]], [on[j][0], on[j][1]]));
            }
        }
    }
    best.filter(|b| b.0 > 1e-3).map(|b| (b.1, b.2))
}

/// A plate's DXF: closed polylines (outline, then holes) and circles, on layer 0.
fn plate_dxf(loops: &[Vec<P2>], circles: &[[f64; 3]]) -> String {
    let mut o = String::from("0\nSECTION\n2\nENTITIES\n");
    for l in loops.iter().filter(|l| l.len() >= 3) {
        o += &format!("0\nLWPOLYLINE\n8\n0\n90\n{}\n70\n1\n", l.len());
        for p in l {
            o += &format!("10\n{}\n20\n{}\n", f4(p[0]), f4(p[1]));
        }
    }
    for c in circles {
        o += &format!("0\nCIRCLE\n8\n0\n10\n{}\n20\n{}\n40\n{}\n", f4(c[0]), f4(c[1]), f4(c[2]));
    }
    o + "0\nENDSEC\n0\nEOF\n"
}

/// The commands that build `m`, and what could not be carried over.
struct Plan {
    rule: Value,
    base: String,
    flanges: Vec<Value>,
    k: f64,
    warnings: Vec<String>,
}

fn plan(m: &Model, d90: Option<f64>) -> std::result::Result<Plan, String> {
    let t = m.thickness;
    let mut warnings = Vec::new();
    let root = m.faces.iter().find(|f| f.root).ok_or("the pattern has no base plate")?;
    let k = match d90 {
        Some(d) if d > 0.0 => k_from_d90(d, t, t).clamp(0.0, 1.0),
        _ => {
            if !m.bends.is_empty() {
                warnings.push("no bend deduction (d90): the rule's K-factor is 0.44".into());
            }
            0.44
        }
    };
    let rule = json!({"name": format!("Kerf {} mm", f4(t)), "thickness": format!("{} mm", f4(t)), "k_factor": f4(k), "bend_radius": format!("{} mm", f4(t)), "active": true});
    // The base plate at the origin: its lower-left corner at (0, 0).
    let (ox, oy) = root.pts.iter().fold((f64::INFINITY, f64::INFINITY), |(x, y), p| (x.min(p[0]), y.min(p[1])));
    let at = |p: P2| [p[0] - ox, p[1] - oy];
    let mut loops = vec![root.pts.iter().map(|p| at(*p)).collect::<Vec<_>>()];
    loops.extend(root.holes.iter().map(|h| h.iter().map(|p| at(*p)).collect()));
    let circles: Vec<[f64; 3]> = root.circles.iter().map(|c| [c[0] - ox, c[1] - oy, c[2]]).collect();
    let base = plate_dxf(&loops, &circles);
    // One edge flange per chain of plates off the base.
    let kids = |id: usize| m.faces.iter().filter(move |f| f.parent == Some(id));
    let dir = |f: &Face| if f.bend.as_ref().is_some_and(|b| b.layer.to_ascii_uppercase().contains("DF")) { -1.0 } else { 1.0 };
    let mut flanges = Vec::new();
    for first in kids(root.id) {
        if flanges.len() >= MAX_FLANGES {
            warnings.push(format!("more than {MAX_FLANGES} flanges: the rest were left out"));
            break;
        }
        let mut chain = vec![first];
        let mut cur = first;
        loop {
            let next: Vec<&Face> = kids(cur.id).collect();
            let Some(n) = next.first() else { break };
            if next.len() > 1 {
                warnings.push(format!("plate {} branches: only its first plate was built", cur.id));
            }
            chain.push(n);
            cur = n;
            if chain.len() > 17 {
                warnings.push("a chain of more than 16 further bends was cut short".into());
                break;
            }
        }
        let Some((a, b)) = shared_edge(root, first) else {
            warnings.push(format!("plate {}: could not find the edge it hangs from, left out", first.id));
            continue;
        };
        let mid = at([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]);
        let height = |f: &Face| f.outer.unwrap_or(0.0);
        let angle = |f: &Face| f.bend.as_ref().map_or(90.0, |b| b.angle);
        // A flange folds up from the edge picked on the top face (z = t) and down from the bottom
        // one (z = 0): Nck-DG folds towards the viewer, Nck-DF away.
        let mut p = json!({
            "edges": [[f4(mid[0]), f4(mid[1]), if dir(first) > 0.0 { f4(t) } else { 0.0 }]],
            "height": format!("{} mm", f4(height(first))),
            "angle": format!("{} deg", f4(angle(first))),
        });
        if chain.len() > 1 {
            // Further bends turn the way the first one did; one folded back the other way flips.
            let then: Vec<Value> = chain[1..]
                .iter()
                .map(|f| {
                    let mut b = json!({"angle": format!("{} deg", f4(angle(f))), "height": format!("{} mm", f4(height(f)))});
                    if dir(f) < 0.0 {
                        b["flip"] = json!(true);
                    }
                    b
                })
                .collect();
            p["then"] = json!(then);
        }
        let lost: usize = chain.iter().map(|f| f.holes.len() + f.circles.len()).sum();
        if lost > 0 {
            warnings.push(format!("{lost} hole(s) in the flanges are not cut yet (only the base plate's)"));
        }
        flanges.push(p);
    }
    Ok(Plan { rule, base, flanges, k, warnings })
}

fn import_flat(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "sheet.import_flat";
    let text = match (str_(p, "text"), str_(p, "path")) {
        (Some(t), _) => t.to_string(),
        (None, Some(path)) => {
            let bytes = solidcraft_io::vfs::read(path, MAX_DXF_BYTES).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => return Err(bad(cmd, "give `text` or `path`")),
    };
    for k in ["thickness", "d90"] {
        if p.get(k).is_some_and(|v| !v.is_null() && !v.is_number()) {
            return Err(bad(cmd, format!("`{k}` must be a number (mm)")));
        }
    }
    let thickness = match num(p, "thickness") {
        Some(t) if t.is_finite() && t > 0.0 && t <= 100.0 => Some(t),
        Some(_) => return Err(bad(cmd, "`thickness` must be more than 0 and at most 100 mm")),
        None => None,
    };
    let d90 = match num(p, "d90") {
        Some(d) if d.is_finite() && (0.0..=200.0).contains(&d) => Some(d),
        Some(_) => return Err(bad(cmd, "`d90` must be 0 to 200 mm")),
        None => None,
    };
    let m = fold_dxf(&text, &FoldOptions { thickness, d90, ..FoldOptions::default() }).map_err(|e| bad(cmd, e))?;
    // The drawing's own `D:` note (KerfLine writes the material's deduction) when not given here.
    let d90 = d90.or(m.info.d90);
    let plan = plan(&m, d90).map_err(|e| bad(cmd, e))?;
    let undo_before = s.undo.len();
    let built = (|| -> Result<String> {
        s.execute("sheet.manage_rules", &plan.rule)?;
        let sk = s.execute("sketch.insert_dxf", &json!({"text": plan.base, "name": "Flat base"}))?;
        let sketch = sk["sketch"].clone();
        if s.active_sketch.is_some() {
            s.execute("sketch.finish", &json!({}))?;
        }
        // The plate is the profile with the holes: leave out the profiles that only fill a hole.
        let fills: Vec<usize> = sketch
            .as_u64()
            .and_then(|id| {
                let st = s.model.state();
                let ss = st.sketch(id)?;
                let sorted = |v: &[String]| {
                    let mut v = v.to_vec();
                    v.sort();
                    v
                };
                let holes: Vec<Vec<String>> = ss.profiles.iter().flat_map(|p| p.hole_curves.iter().map(|h| sorted(h))).collect();
                Some(ss.profiles.iter().enumerate().filter(|(_, p)| !holes.contains(&sorted(&p.outer_curves))).map(|(i, _)| i).collect())
            })
            .unwrap_or_default();
        let mut base = json!({"sketch": sketch});
        if !fills.is_empty() {
            base["profiles"] = json!(fills);
        }
        if let Some(name) = str_(p, "name").map(str::trim).filter(|n| !n.is_empty()) {
            base["body_name"] = json!(name);
        }
        let r = s.execute("sheet.flange", &base)?;
        let body = r["created_bodies"]
            .get(0)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| s.model.state().sheets.last().map(|x| x.body.clone()))
            .unwrap_or_default();
        for f in &plan.flanges {
            let mut f = f.clone();
            if !body.is_empty() {
                f["body"] = json!(body);
            }
            s.execute("sheet.flange", &f)?;
        }
        Ok(body)
    })();
    // One undo step for the whole part (the command's own).
    s.undo.truncate(undo_before);
    let body = built?;
    let mut warnings = m.warnings.clone();
    warnings.extend(plan.warnings);
    let reply = ImportFlatResult {
        body,
        mode: m.mode.clone(),
        thickness: m.thickness,
        k_factor: f4(plan.k),
        size: m.size,
        flanges: plan.flanges.len(),
        bend_dims: m.dims.iter().map(|d| d.v).collect(),
        name: m.info.name.clone(),
        material: m.info.material.clone(),
        warnings,
        param_warnings: Vec::new(),
    };
    serde_json::to_value(reply).map_err(|e| EngineError::Other(e.to_string()))
}

/// Does this DXF text draw bend lines on the flat-pattern layers (`Nck-DG` / `Nck-DF`)?
/// `doc.open` folds such a file into a sheet part instead of reading it as a sketch.
pub fn is_flat_pattern(text: &str) -> bool {
    text.lines().map(str::trim).any(|l| l.eq_ignore_ascii_case("Nck-DG") || l.eq_ignore_ascii_case("Nck-DF"))
}

#[cfg(test)]
#[path = "sheet_import_tests.rs"]
mod tests;
