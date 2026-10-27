//! 전개 DXF → 3D 접기 (판금 전용판).
//!
//! 두 가지 도면을 읽는다.
//! - 한 장 전개도: 외곽선 + 판을 가로지르는 절곡선(`Nck-DG` / `Nck-DF`). 절곡마다 90° 연신율(공제값)의
//!   절반씩을 양쪽 면에 더해 외치수로 되돌린 뒤 접는다.
//! - 면별로 펼친 도면: 각 면이 떨어져 있고 절곡선은 그 면이 붙는 변에 있다. 면 치수가 외치수이므로
//!   틈만 메워 접는다. 절곡선이 없는 면이 기준 면이다.
//!
//! 규칙: `Nck-DG` = 앞(보는 쪽)으로 절곡, `Nck-DF` = 뒤로 절곡, 각도 기입이 없으면 90°.
//! `Nck-RK` = 레이저 마킹. 두께는 외치수면에서 안쪽(오목한 쪽)으로 붙인다. MTEXT `P: M: T: Q:` 정보를 읽는다.
//!
//! KerfLine(웹/Electron)의 `fold3d.js`와 같은 계산을 한다. 두 구현의 결과는 `tests/`의 같은 도면으로 맞춘다.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

const DEG: f64 = PI / 180.0;
const CHORD: f64 = 0.05;
type P2 = [f64; 2];

/// 읽어 들인 도면 요소 (모든 좌표는 도면 단위, mm 가정).
#[derive(Debug, Clone)]
pub enum Entity {
    Line {
        layer: String,
        a: P2,
        b: P2,
    },
    Circle {
        layer: String,
        c: P2,
        r: f64,
    },
    Arc {
        layer: String,
        c: P2,
        r: f64,
        a0: f64,
        a1: f64,
    },
    /// 폴리라인 꼭짓점: (x, y, bulge)
    Poly {
        layer: String,
        pts: Vec<[f64; 3]>,
        closed: bool,
    },
    Text {
        layer: String,
        p: P2,
        h: f64,
        text: String,
        mtext: bool,
    },
}

#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub entities: Vec<Entity>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------- DXF

type Pairs = Vec<(i32, String)>;

fn num(body: &[(i32, String)], code: i32, def: f64) -> f64 {
    body.iter().find(|p| p.0 == code).and_then(|p| p.1.trim().parse().ok()).unwrap_or(def)
}
fn text_of(body: &[(i32, String)], code: i32) -> String {
    body.iter().filter(|p| p.0 == code).map(|p| p.1.as_str()).collect::<Vec<_>>().join("")
}

#[derive(Debug, Clone)]
struct RawEnt {
    kind: String,
    body: Pairs,
    verts: Vec<Pairs>,
}

/// 2D 아핀 변환 x' = a x + b y + c, y' = d x + e y + f
#[derive(Debug, Clone, Copy)]
struct Xf([f64; 6]);
impl Xf {
    const I: Xf = Xf([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    fn ap(&self, p: P2) -> P2 {
        let m = self.0;
        [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]]
    }
    fn det(&self) -> f64 {
        self.0[0] * self.0[4] - self.0[1] * self.0[3]
    }
    fn then(&self, outer: &Xf) -> Xf {
        // outer ∘ self
        let (a, b) = (outer.0, self.0);
        Xf([
            a[0] * b[0] + a[1] * b[3],
            a[0] * b[1] + a[1] * b[4],
            a[0] * b[2] + a[1] * b[5] + a[2],
            a[3] * b[0] + a[4] * b[3],
            a[3] * b[1] + a[4] * b[4],
            a[3] * b[2] + a[4] * b[5] + a[5],
        ])
    }
    fn is_identity(&self) -> bool {
        self.0.iter().zip(Xf::I.0.iter()).all(|(x, y)| (x - y).abs() < 1e-12)
    }
}

/// ASCII DXF를 읽는다. LINE, ARC, CIRCLE, LWPOLYLINE, POLYLINE, TEXT, MTEXT, INSERT(블록 펼침).
pub fn parse_dxf(text: &str) -> Parsed {
    let lines: Vec<&str> = text.split(['\n']).map(|l| l.trim_end_matches('\r')).collect();
    let mut pairs: Pairs = Vec::with_capacity(lines.len() / 2);
    let mut i = 0;
    while i + 1 < lines.len() {
        let code = lines[i].trim().parse::<i32>().unwrap_or(-1);
        pairs.push((code, lines[i + 1].to_string()));
        i += 2;
    }
    let mut warn: Vec<String> = Vec::new();
    let mut ents: Vec<RawEnt> = Vec::new();
    let mut blocks: HashMap<String, (P2, Vec<RawEnt>)> = HashMap::new();

    let read_ent = |pairs: &Pairs, start: usize| -> (Pairs, usize) {
        let mut j = start + 1;
        let mut body = Vec::new();
        while j < pairs.len() && pairs[j].0 != 0 {
            body.push(pairs[j].clone());
            j += 1;
        }
        (body, j)
    };
    let collect = |pairs: &Pairs, i: &mut usize, end: &str, into: &mut Vec<RawEnt>| {
        while *i < pairs.len() {
            let (c, v) = (&pairs[*i].0, pairs[*i].1.trim());
            if *c == 0 && (v == end || v == "ENDSEC") {
                return;
            }
            if *c == 0 {
                let kind = v.to_string();
                let (body, j) = read_ent(pairs, *i);
                *i = j;
                let mut verts = Vec::new();
                if kind == "POLYLINE" {
                    while *i < pairs.len() && pairs[*i].1.trim() == "VERTEX" {
                        let (vb, k) = read_ent(pairs, *i);
                        verts.push(vb);
                        *i = k;
                    }
                    if *i < pairs.len() && pairs[*i].1.trim() == "SEQEND" {
                        let (_, k) = read_ent(pairs, *i);
                        *i = k;
                    }
                }
                into.push(RawEnt { kind, body, verts });
                continue;
            }
            *i += 1;
        }
    };
    let mut i = 0;
    while i < pairs.len() {
        if pairs[i].0 == 0 && pairs[i].1.trim() == "SECTION" {
            let name = pairs.get(i + 1).map(|p| p.1.trim().to_string()).unwrap_or_default();
            i += 2;
            if name == "ENTITIES" {
                collect(&pairs, &mut i, "ENDSEC", &mut ents);
            } else if name == "BLOCKS" {
                while i < pairs.len() && !(pairs[i].0 == 0 && pairs[i].1.trim() == "ENDSEC") {
                    if pairs[i].0 == 0 && pairs[i].1.trim() == "BLOCK" {
                        let (head, j) = read_ent(&pairs, i);
                        i = j;
                        let nm = text_of(&head, 2).trim().to_string();
                        let base = [num(&head, 10, 0.0), num(&head, 20, 0.0)];
                        let mut list = Vec::new();
                        collect(&pairs, &mut i, "ENDBLK", &mut list);
                        if i < pairs.len() && pairs[i].1.trim() == "ENDBLK" {
                            let (_, k) = read_ent(&pairs, i);
                            i = k;
                        }
                        blocks.insert(nm, (base, list));
                    } else {
                        i += 1;
                    }
                }
            } else {
                while i < pairs.len() && !(pairs[i].0 == 0 && pairs[i].1.trim() == "ENDSEC") {
                    i += 1;
                }
            }
            continue;
        }
        i += 1;
    }
    let mut out = Vec::new();
    fn emit(r: &RawEnt, xf: &Xf, depth: usize, blocks: &HashMap<String, (P2, Vec<RawEnt>)>, out: &mut Vec<Entity>, warn: &mut Vec<String>) {
        if r.kind == "INSERT" {
            let name = text_of(&r.body, 2).trim().to_string();
            let Some((base, list)) = blocks.get(&name) else {
                push_warn(warn, format!("블록 {name}을(를) 펼치지 못했습니다."));
                return;
            };
            if depth > 8 {
                return;
            }
            let (sx, sy) = (num(&r.body, 41, 1.0), num(&r.body, 42, 1.0));
            let rot = num(&r.body, 50, 0.0) * DEG;
            let (ca, sa) = (rot.cos(), rot.sin());
            let (x, y) = (num(&r.body, 10, 0.0), num(&r.body, 20, 0.0));
            // p → R·S·(p - base) + ins
            let inner =
                Xf([ca * sx, -sa * sy, x - (ca * sx * base[0] - sa * sy * base[1]), sa * sx, ca * sy, y - (sa * sx * base[0] + ca * sy * base[1])])
                    .then(xf);
            for e in list {
                emit(e, &inner, depth + 1, blocks, out, warn);
            }
            return;
        }
        if let Some(e) = decode(r, warn) {
            out.push(apply_xf(e, xf));
        }
    }
    for r in &ents {
        emit(r, &Xf::I, 0, &blocks, &mut out, &mut warn);
    }
    Parsed { entities: out, warnings: warn }
}

fn push_warn(w: &mut Vec<String>, s: String) {
    if !w.contains(&s) {
        w.push(s);
    }
}

fn decode(r: &RawEnt, warn: &mut Vec<String>) -> Option<Entity> {
    let b = &r.body;
    let layer = {
        let l = text_of(b, 8).trim().to_string();
        if l.is_empty() { "0".to_string() } else { l }
    };
    let fx = if num(b, 230, 1.0) < 0.0 { -1.0 } else { 1.0 };
    Some(match r.kind.as_str() {
        "LINE" => Entity::Line { layer, a: [num(b, 10, 0.0), num(b, 20, 0.0)], b: [num(b, 11, 0.0), num(b, 21, 0.0)] },
        "CIRCLE" => Entity::Circle { layer, c: [fx * num(b, 10, 0.0), num(b, 20, 0.0)], r: num(b, 40, 0.0) },
        "ARC" => {
            let (mut a0, mut a1) = (num(b, 50, 0.0), num(b, 51, 0.0));
            if fx < 0.0 {
                let (t0, t1) = (180.0 - a1, 180.0 - a0);
                a0 = t0;
                a1 = t1;
            }
            Entity::Arc { layer, c: [fx * num(b, 10, 0.0), num(b, 20, 0.0)], r: num(b, 40, 0.0), a0: a0 * DEG, a1: a1 * DEG }
        }
        "LWPOLYLINE" => {
            let mut pts: Vec<[f64; 3]> = Vec::new();
            for (c, v) in b {
                let val: f64 = v.trim().parse().unwrap_or(0.0);
                match c {
                    10 => pts.push([fx * val, 0.0, 0.0]),
                    20 => {
                        if let Some(p) = pts.last_mut() {
                            p[1] = val
                        }
                    }
                    42 => {
                        if let Some(p) = pts.last_mut() {
                            p[2] = fx * val
                        }
                    }
                    _ => {}
                }
            }
            Entity::Poly { layer, pts, closed: (num(b, 70, 0.0) as i64) & 1 == 1 }
        }
        "POLYLINE" => {
            let flags = num(b, 70, 0.0) as i64;
            if flags & (16 | 64) != 0 {
                push_warn(warn, "3D 메쉬 폴리라인은 건너뜁니다.".into());
                return None;
            }
            let pts = r
                .verts
                .iter()
                .filter(|v| (num(v, 70, 0.0) as i64) & 16 == 0)
                .map(|v| [fx * num(v, 10, 0.0), num(v, 20, 0.0), fx * num(v, 42, 0.0)])
                .collect();
            Entity::Poly { layer, pts, closed: flags & 1 == 1 }
        }
        "TEXT" => Entity::Text { layer, p: [num(b, 10, 0.0), num(b, 20, 0.0)], h: num(b, 40, 2.5), text: text_of(b, 1), mtext: false },
        "MTEXT" => Entity::Text {
            layer,
            p: [num(b, 10, 0.0), num(b, 20, 0.0)],
            h: num(b, 40, 2.5),
            text: format!("{}{}", text_of(b, 3), text_of(b, 1)),
            mtext: true,
        },
        "ELLIPSE" | "SPLINE" => {
            push_warn(warn, format!("{}은(는) 아직 읽지 않습니다. 선·호·폴리라인으로 바꿔 주세요.", r.kind));
            return None;
        }
        _ => return None,
    })
}

fn apply_xf(e: Entity, xf: &Xf) -> Entity {
    if xf.is_identity() {
        return e;
    }
    let s = xf.det().signum();
    match e {
        Entity::Line { layer, a, b } => Entity::Line { layer, a: xf.ap(a), b: xf.ap(b) },
        Entity::Circle { layer, c, r } => Entity::Circle { layer, c: xf.ap(c), r: r * xf.det().abs().sqrt() },
        Entity::Arc { layer, c, r, a0, a1 } => Entity::Poly {
            layer,
            pts: arc_pts(c, r, a0, a1)
                .into_iter()
                .map(|p| {
                    let q = xf.ap(p);
                    [q[0], q[1], 0.0]
                })
                .collect(),
            closed: false,
        },
        Entity::Poly { layer, pts, closed } => Entity::Poly {
            layer,
            pts: pts
                .into_iter()
                .map(|p| {
                    let q = xf.ap([p[0], p[1]]);
                    [q[0], q[1], p[2] * s]
                })
                .collect(),
            closed,
        },
        Entity::Text { layer, p, h, text, mtext } => Entity::Text { layer, p: xf.ap(p), h, text, mtext },
    }
}

// ---------------------------------------------------------------- 기하

fn arc_steps(r: f64, sweep: f64) -> usize {
    let step = 2.0 * (1.0 - CHORD / r.max(1e-6)).max(-1.0).acos();
    ((sweep.abs() / step).ceil() as usize).max(2)
}
fn arc_pts(c: P2, r: f64, a0: f64, a1: f64) -> Vec<P2> {
    let mut sweep = a1 - a0;
    while sweep <= 0.0 {
        sweep += 2.0 * PI;
    }
    let n = arc_steps(r, sweep);
    (0..=n)
        .map(|k| {
            let a = a0 + sweep * k as f64 / n as f64;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}
fn bulge_pts(p: [f64; 3], q: [f64; 3], b: f64) -> Vec<P2> {
    if b == 0.0 {
        return vec![[q[0], q[1]]];
    }
    let th = 4.0 * b.atan();
    let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
    let ch = dx.hypot(dy);
    if ch < 1e-9 {
        return vec![];
    }
    let (nx, ny) = (-dy / ch, dx / ch);
    let off = ch * (1.0 - b * b) / (4.0 * b);
    let (cx, cy) = ((p[0] + q[0]) / 2.0 + nx * off, (p[1] + q[1]) / 2.0 + ny * off);
    let r = (p[0] - cx).hypot(p[1] - cy);
    let a0 = (p[1] - cy).atan2(p[0] - cx);
    let n = arc_steps(r, th);
    let mut out: Vec<P2> = (1..=n)
        .map(|k| {
            let a = a0 + th * k as f64 / n as f64;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect();
    if let Some(l) = out.last_mut() {
        *l = [q[0], q[1]];
    }
    out
}
fn poly_segs(pts: &[[f64; 3]], closed: bool) -> Vec<(P2, P2)> {
    if pts.len() < 2 {
        return vec![];
    }
    let mut all = vec![[pts[0][0], pts[0][1]]];
    let n = if closed { pts.len() } else { pts.len() - 1 };
    for k in 0..n {
        all.extend(bulge_pts(pts[k], pts[(k + 1) % pts.len()], pts[k][2]));
    }
    all.windows(2).map(|w| (w[0], w[1])).collect()
}
fn poly_circle(pts: &[[f64; 3]], closed: bool) -> Option<(P2, f64)> {
    if !closed || pts.len() != 2 {
        return None;
    }
    let (a, b) = (pts[0], pts[1]);
    if ((a[2].abs() - 1.0).abs() > 1e-6) || (a[2] - b[2]).abs() > 1e-6 {
        return None;
    }
    Some(([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0], (a[0] - b[0]).hypot(a[1] - b[1]) / 2.0))
}
fn area(p: &[P2]) -> f64 {
    let mut s = 0.0;
    for k in 0..p.len() {
        let (a, b) = (p[k], p[(k + 1) % p.len()]);
        s += a[0] * b[1] - b[0] * a[1];
    }
    s / 2.0
}
fn point_in_poly(p: P2, poly: &[P2]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut ins = false;
    let mut j = poly.len() - 1;
    for k in 0..poly.len() {
        let (a, b) = (poly[k], poly[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            ins = !ins;
        }
        j = k;
    }
    ins
}
/// 다각형 안쪽 점 (가로 스캔). `others` = 다른 닫힌 선들 — 이 다각형 안에 든 구멍(원·장공) 안쪽은 피한다
/// (판 한가운데에 구멍이 있으면 판의 한가운데 점이 구멍 안에 들어가 판을 빈 곳으로 잘못 보던 문제).
fn interior_point(poly: &[P2], others: &[&Vec<P2>]) -> P2 {
    let Some(&first) = poly.first() else { return [0.0, 0.0] };
    let (y0, y1) = poly.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    let xs_of = |q: &[P2], y: f64, out: &mut Vec<f64>| {
        for k in 0..q.len() {
            let (a, b) = (q[k], q[(k + 1) % q.len()]);
            if (a[1] > y) != (b[1] > y) {
                out.push(a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1]));
            }
        }
    };
    let own = area(poly).abs();
    // 이 다각형 안에 든 더 작은 닫힌 선 (같은 선을 반대로 돈 루프는 넓이가 같아 빠진다)
    let inner: Vec<&Vec<P2>> = others.iter().copied().filter(|q| q.len() >= 3 && area(q).abs() < own - 1e-6).collect();
    let mut fallback: Option<P2> = None;
    for f in [0.5, 0.37, 0.63, 0.21, 0.79, 0.11, 0.89, 0.03, 0.97, 0.45, 0.55, 0.29, 0.71, 0.16, 0.84, 0.07, 0.93] {
        let y = y0 + (y1 - y0) * f;
        let mut xs: Vec<f64> = Vec::new();
        xs_of(poly, y, &mut xs);
        xs.sort_by(|a, b| a.total_cmp(b));
        if fallback.is_none() {
            let mut best: Option<(f64, f64)> = None;
            for k in (0..xs.len().saturating_sub(1)).step_by(2) {
                if best.is_none_or(|b| xs[k + 1] - xs[k] > b.1 - b.0) {
                    best = Some((xs[k], xs[k + 1]));
                }
            }
            if let Some(b) = best
                && b.1 - b.0 > 1e-6
            {
                fallback = Some([(b.0 + b.1) / 2.0, y]);
            }
        }
        let mut all = xs.clone();
        for q in &inner {
            xs_of(q, y, &mut all);
        }
        all.sort_by(|a, b| a.total_cmp(b));
        let mut best: Option<(f64, P2)> = None;
        for k in 0..all.len().saturating_sub(1) {
            let w = all[k + 1] - all[k];
            if w <= 1e-6 || best.is_some_and(|b| w <= b.0) {
                continue;
            }
            let p = [(all[k] + all[k + 1]) / 2.0, y];
            if !point_in_poly(p, poly) || inner.iter().any(|q| point_in_poly(p, q)) {
                continue;
            }
            best = Some((w, p));
        }
        if let Some((_, p)) = best {
            return p;
        }
    }
    fallback.unwrap_or(first)
}
fn seg_dist(p: P2, a: P2, b: P2) -> (f64, f64) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    ((p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy), t)
}
fn seg_x(a: P2, b: P2, c: P2, d: P2) -> Option<(f64, f64)> {
    let r = [b[0] - a[0], b[1] - a[1]];
    let s = [d[0] - c[0], d[1] - c[1]];
    let den = r[0] * s[1] - r[1] * s[0];
    if den.abs() < 1e-12 {
        return None;
    }
    let q = [c[0] - a[0], c[1] - a[1]];
    Some(((q[0] * s[1] - q[1] * s[0]) / den, (q[0] * r[1] - q[1] * r[0]) / den))
}
fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot(a: P2, b: P2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn unit(v: P2) -> P2 {
    let l = v[0].hypot(v[1]);
    if l > 0.0 { [v[0] / l, v[1] / l] } else { [1.0, 0.0] }
}

// ---------------------------------------------------------------- 평면 그래프

struct Graph {
    edges: Vec<(usize, usize, Option<usize>)>, // (u, v, 절곡선 번호)
    half: Vec<(usize, usize, usize)>,          // (from, to, edge)
    loops: Vec<(Vec<usize>, Vec<P2>, f64)>,    // (반간선들, 점들, 넓이)
    dangling: HashSet<usize>,
}

fn build_faces(out_segs: &[(P2, P2)], bend_segs: &[(P2, P2)], tol: f64) -> Graph {
    struct S {
        a: P2,
        b: P2,
        bend: Option<usize>,
    }
    let mut all: Vec<S> = out_segs.iter().map(|s| S { a: s.0, b: s.1, bend: None }).collect();
    let n_out = all.len();
    for (bi, s) in bend_segs.iter().enumerate() {
        let fix = |p: P2, other: P2, all: &[S]| -> P2 {
            let mut best: Option<(f64, P2)> = None;
            for o in &all[..n_out] {
                let Some((t, u)) = seg_x(other, p, o.a, o.b) else { continue };
                if !(-1e-6..=1.0 + 1e-6).contains(&u) {
                    continue;
                }
                let l = (p[0] - other[0]).hypot(p[1] - other[1]);
                let dist = (t - 1.0).abs() * l;
                if t > 0.5 && dist <= tol && best.is_none_or(|b| dist < b.0) {
                    best = Some((dist, [other[0] + (p[0] - other[0]) * t, other[1] + (p[1] - other[1]) * t]));
                }
            }
            best.map(|b| b.1).unwrap_or(p)
        };
        let a = fix(s.0, s.1, &all);
        let b = fix(s.1, a, &all);
        all.push(S { a, b, bend: Some(bi) });
    }
    // 교차점에서 자르기
    let mut cuts: Vec<Vec<f64>> = all.iter().map(|_| vec![0.0, 1.0]).collect();
    let cell = 50.0;
    let mut bucket: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (k, s) in all.iter().enumerate() {
        let x0 = (s.a[0].min(s.b[0]) / cell).floor() as i64;
        let x1 = (s.a[0].max(s.b[0]) / cell).floor() as i64;
        let y0 = (s.a[1].min(s.b[1]) / cell).floor() as i64;
        let y1 = (s.a[1].max(s.b[1]) / cell).floor() as i64;
        for i in x0..=x1 {
            for j in y0..=y1 {
                bucket.entry((i, j)).or_default().push(k);
            }
        }
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for list in bucket.values() {
        for p in 0..list.len() {
            for q in p + 1..list.len() {
                let (i, j) = (list[p].min(list[q]), list[p].max(list[q]));
                if !seen.insert((i, j)) {
                    continue;
                }
                let (s, o) = (&all[i], &all[j]);
                let ls = (s.b[0] - s.a[0]).hypot(s.b[1] - s.a[1]).max(1e-12);
                let lo = (o.b[0] - o.a[0]).hypot(o.b[1] - o.a[1]).max(1e-12);
                if let Some((t, u)) = seg_x(s.a, s.b, o.a, o.b) {
                    let (et, eu) = (1e-4 / ls, 1e-4 / lo);
                    if t >= -et && t <= 1.0 + et && u >= -eu && u <= 1.0 + eu {
                        cuts[i].push(t.clamp(0.0, 1.0));
                        cuts[j].push(u.clamp(0.0, 1.0));
                    }
                } else {
                    for (pt, k, (qa, qb)) in [(o.a, i, (s.a, s.b)), (o.b, i, (s.a, s.b)), (s.a, j, (o.a, o.b)), (s.b, j, (o.a, o.b))] {
                        let (d, t) = seg_dist(pt, qa, qb);
                        if d < 1e-4 && t > 0.0 && t < 1.0 {
                            cuts[k].push(t);
                        }
                    }
                }
            }
        }
    }
    // 노드·간선
    let snap = 1e-3;
    let mut nodes: Vec<P2> = Vec::new();
    let mut node_map: HashMap<(i64, i64), usize> = HashMap::new();
    let mut node_of = |p: P2, nodes: &mut Vec<P2>| -> usize {
        let (kx, ky) = ((p[0] / snap).round() as i64, (p[1] / snap).round() as i64);
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(&id) = node_map.get(&(kx + dx, ky + dy)) {
                    return id;
                }
            }
        }
        nodes.push(p);
        node_map.insert((kx, ky), nodes.len() - 1);
        nodes.len() - 1
    };
    let mut edges: Vec<(usize, usize, Option<usize>)> = Vec::new();
    let mut edge_map: HashMap<(usize, usize), usize> = HashMap::new();
    for (k, s) in all.iter().enumerate() {
        let mut ts: Vec<f64> = cuts[k].iter().map(|t| (t * 1e9).round() / 1e9).collect();
        ts.sort_by(|a, b| a.total_cmp(b));
        ts.dedup();
        for m in 0..ts.len().saturating_sub(1) {
            let at = |t: f64| [s.a[0] + (s.b[0] - s.a[0]) * t, s.a[1] + (s.b[1] - s.a[1]) * t];
            let u = node_of(at(ts[m]), &mut nodes);
            let v = node_of(at(ts[m + 1]), &mut nodes);
            if u == v {
                continue;
            }
            let key = (u.min(v), u.max(v));
            if let Some(&e) = edge_map.get(&key) {
                if s.bend.is_some() {
                    edges[e].2 = s.bend;
                }
                continue;
            }
            edge_map.insert(key, edges.len());
            edges.push((u, v, s.bend));
        }
    }
    // 매달린 선 제거
    let mut deg = vec![0usize; nodes.len()];
    for e in &edges {
        deg[e.0] += 1;
        deg[e.1] += 1;
    }
    let mut alive = vec![true; edges.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (k, e) in edges.iter().enumerate() {
            if alive[k] && (deg[e.0] == 1 || deg[e.1] == 1) {
                alive[k] = false;
                deg[e.0] -= 1;
                deg[e.1] -= 1;
                changed = true;
            }
        }
    }
    let dangling: HashSet<usize> = edges.iter().enumerate().filter(|(k, e)| !alive[*k] && e.2.is_some()).map(|(_, e)| e.2.unwrap_or(0)).collect();
    // 반간선과 면 추적 (면이 왼쪽)
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut half: Vec<(usize, usize, usize)> = Vec::new();
    for (k, e) in edges.iter().enumerate() {
        if !alive[k] {
            continue;
        }
        let h = half.len();
        half.push((e.0, e.1, k));
        half.push((e.1, e.0, k));
        out[e.0].push(h);
        out[e.1].push(h + 1);
    }
    let ang = |h: usize, half: &[(usize, usize, usize)]| {
        let (a, b) = (nodes[half[h].0], nodes[half[h].1]);
        (b[1] - a[1]).atan2(b[0] - a[0])
    };
    for list in out.iter_mut() {
        list.sort_by(|&x, &y| ang(x, &half).total_cmp(&ang(y, &half)));
    }
    let mut pos_in = vec![0usize; half.len()];
    for list in &out {
        for (idx, &h) in list.iter().enumerate() {
            pos_in[h] = idx;
        }
    }
    let mut used = vec![false; half.len()];
    let mut loops = Vec::new();
    for h0 in 0..half.len() {
        if used[h0] {
            continue;
        }
        let mut hs = Vec::new();
        let mut h = h0;
        while !used[h] {
            used[h] = true;
            hs.push(h);
            let twin = h ^ 1;
            let list = &out[half[h].1];
            h = list[(pos_in[twin] + list.len() - 1) % list.len()];
        }
        let pts: Vec<P2> = hs.iter().map(|&k| nodes[half[k].0]).collect();
        let a = area(&pts);
        loops.push((hs, pts, a));
    }
    Graph { edges, half, loops, dangling }
}

// ---------------------------------------------------------------- 접기

/// 3x3 회전 + 이동. x → R x + t
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Rigid {
    pub r: [f64; 9],
    pub t: [f64; 3],
}
impl Rigid {
    pub const I: Rigid = Rigid { r: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], t: [0.0; 3] };
    /// self ∘ b
    fn mul(&self, b: &Rigid) -> Rigid {
        let mut r = [0.0; 9];
        for i in 0..3 {
            for j in 0..3 {
                r[i * 3 + j] = (0..3).map(|k| self.r[i * 3 + k] * b.r[k * 3 + j]).sum();
            }
        }
        let t = [0, 1, 2].map(|i| (0..3).map(|k| self.r[i * 3 + k] * b.t[k]).sum::<f64>() + self.t[i]);
        Rigid { r, t }
    }
    fn rot_about(p: [f64; 3], k: [f64; 3], ang: f64) -> Rigid {
        let [x, y, z] = k;
        let (c, s) = (ang.cos(), ang.sin());
        let cc = 1.0 - c;
        let r = [
            c + x * x * cc,
            x * y * cc - z * s,
            x * z * cc + y * s,
            y * x * cc + z * s,
            c + y * y * cc,
            y * z * cc - x * s,
            z * x * cc - y * s,
            z * y * cc + x * s,
            c + z * z * cc,
        ];
        let rp = [0, 1, 2].map(|i| r[i * 3] * p[0] + r[i * 3 + 1] * p[1] + r[i * 3 + 2] * p[2]);
        Rigid { r, t: [p[0] - rp[0], p[1] - rp[1], p[2] - rp[2]] }
    }
    fn trans(v: [f64; 3]) -> Rigid {
        Rigid { t: v, ..Rigid::I }
    }
    pub fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        [0, 1, 2].map(|i| self.r[i * 3] * p[0] + self.r[i * 3 + 1] * p[1] + self.r[i * 3 + 2] * p[2] + self.t[i])
    }
}

/// 90° 연신율(공제값) d90과 두께 t로 다른 각도의 공제값을 K-factor로 환산한다(내R = t 가정).
pub fn deduction_from_d90(d90: f64, t: f64) -> impl Fn(f64) -> f64 {
    let r = t;
    let k = ((2.0 * (r + t) - d90) / (PI / 2.0) - r) / t;
    move |ang: f64| {
        let a = ang * DEG;
        2.0 * (a / 2.0).tan() * (r + t) - a * (r + k * t)
    }
}

pub struct FoldOptions<'a> {
    /// 두께. 없으면 MTEXT `T:` 값, 그것도 없으면 1
    pub thickness: Option<f64>,
    /// 각도 → 공제값. 한 장 전개도에만 쓴다. 없으면 `d90`, 그것도 없으면 0 (경고)
    pub deduction: Option<&'a dyn Fn(f64) -> f64>,
    pub d90: Option<f64>,
    /// 절곡선 옆 각도 숫자가 내각이면 true
    pub angle_is_included: bool,
    pub root_face: Option<usize>,
    /// 절곡선 끝을 외곽에 붙이는 허용 오차 (mm)
    pub tol: f64,
}
impl Default for FoldOptions<'_> {
    fn default() -> Self {
        FoldOptions { thickness: None, deduction: None, d90: None, angle_is_included: false, root_face: None, tol: 1.0 }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Info {
    pub name: Option<String>,
    pub material: Option<String>,
    pub thickness: f64,
    pub qty: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BendOfFace {
    pub layer: String,
    pub angle: f64,
    /// 이 절곡에 쓴 연신율 (펼친 면 형식이면 0)
    pub d: f64,
    pub flat: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Face {
    pub id: usize,
    pub root: bool,
    /// 외치수까지 늘린 판 모양 (도면 좌표)
    pub pts: Vec<P2>,
    /// 도면에 그려진 원래 판 모양
    pub src: Vec<P2>,
    pub holes: Vec<Vec<P2>>,
    /// 원형 구멍 [x, y, r]
    pub circles: Vec<[f64; 3]>,
    pub marks: Vec<Vec<P2>>,
    /// 도면 좌표 (x, y, 0) → 3D
    pub place: Rigid,
    /// 두께 방향: 면 법선(place.r의 z열) × side
    pub side: f64,
    pub parent: Option<usize>,
    /// 부모와 만나는 선에서 이 면 끝까지의 외치수 (기준 면은 없음)
    pub outer: Option<f64>,
    pub bend: Option<BendOfFace>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bend {
    pub layer: String,
    pub angle: f64,
    pub flat: bool,
    pub a: P2,
    pub b: P2,
}

/// 절곡으로 생기는 치수 하나: 면 `face`의, 그 면에 닿는 절곡선에 직각인 외치수.
/// `a`→`b`는 외치수면 위(3D), `o`는 치수선을 밀어낼 방향(단위 벡터), `v`는 값(mm).
#[derive(Debug, Clone, Serialize)]
pub struct FaceDim {
    pub face: usize,
    pub a: [f64; 3],
    pub b: [f64; 3],
    pub o: [f64; 3],
    pub v: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Model {
    /// "한 장 전개도" | "펼친 면" | "혼합" | "평판"
    pub mode: String,
    pub info: Info,
    pub thickness: f64,
    pub faces: Vec<Face>,
    pub bends: Vec<Bend>,
    /// 3D 외곽 크기 (외치수면 기준, 평판은 두께 포함)
    pub size: [f64; 3],
    /// 절곡으로 생기는 치수 (면마다, 닿는 절곡선 방향마다 하나)
    pub dims: Vec<FaceDim>,
    pub warnings: Vec<String>,
}

fn bend_dir(layer: &str) -> Option<f64> {
    match layer.to_ascii_uppercase().as_str() {
        "NCK-DG" => Some(1.0),
        "NCK-DF" => Some(-1.0),
        _ => None,
    }
}

fn clean_poly(p: &[P2]) -> Vec<P2> {
    let mut out: Vec<P2> = Vec::new();
    for &q in p {
        if out.last().is_none_or(|l| (q[0] - l[0]).hypot(q[1] - l[1]) > 1e-6) {
            out.push(q);
        }
    }
    if out.len() > 1 {
        let (f, l) = (out[0], out[out.len() - 1]);
        if (f[0] - l[0]).hypot(f[1] - l[1]) < 1e-6 {
            out.pop();
        }
    }
    let n = out.len();
    (0..n)
        .filter(|&k| {
            let (a, q, b) = (out[(k + n - 1) % n], out[k], out[(k + 1) % n]);
            ((q[0] - a[0]) * (b[1] - q[1]) - (q[1] - a[1]) * (b[0] - q[0])).abs() > 1e-9
        })
        .map(|k| out[k])
        .collect()
}

fn read_info(entities: &[Entity]) -> Info {
    let mut info = Info::default();
    for e in entities {
        let Entity::Text { text, .. } = e else { continue };
        let t = strip_mtext(text);
        let t = t.as_str();
        let field = |key: char| -> Option<String> {
            let pat = format!("{key}:");
            let start = t.match_indices(&pat).find(|(i, _)| *i == 0 || t[..*i].ends_with(' '))?.0 + 2;
            let rest = t[start..].trim_start();
            // 다음 " X:" 앞까지
            let mut end = rest.len();
            let bytes: Vec<(usize, char)> = rest.char_indices().collect();
            for w in 0..bytes.len() {
                if bytes[w].1 == ' ' && w + 2 < bytes.len() && bytes[w + 1].1.is_ascii_uppercase() && bytes[w + 2].1 == ':' {
                    end = bytes[w].0;
                    break;
                }
            }
            Some(rest[..end].trim().to_string())
        };
        if let Some(v) = field('P') {
            info.name = Some(v);
        }
        if let Some(v) = field('M') {
            info.material = v.split_whitespace().next().map(str::to_string);
        }
        if let Some(v) = field('T')
            && let Ok(x) = v.split_whitespace().next().unwrap_or("").parse::<f64>()
        {
            info.thickness = x;
        }
        if let Some(v) = field('Q')
            && let Ok(x) = v.split_whitespace().next().unwrap_or("").parse::<u32>()
        {
            info.qty = Some(x);
        }
    }
    info
}
fn strip_mtext(s: &str) -> String {
    // \P → 공백, \X...; 서식 제거, { } 제거
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.peek() {
                Some('P') | Some('p') => {
                    it.next();
                    out.push(' ');
                }
                Some(&n) if n.is_ascii_alphabetic() => {
                    for d in it.by_ref() {
                        if d == ';' {
                            break;
                        }
                    }
                    out.push(' ');
                }
                _ => out.push(c),
            }
        } else if c != '{' && c != '}' {
            out.push(c);
        }
    }
    out
}
fn angle_text(s: &str) -> Option<f64> {
    let t = strip_mtext(s);
    let t = t.trim().trim_end_matches('°').trim_end_matches("%%d").trim_end_matches("%%D").trim_end_matches('도').trim();
    let digits: String = t.chars().skip_while(|c| !c.is_ascii_digit()).collect();
    let prefix_ok = t.chars().take_while(|c| !c.is_ascii_digit()).all(|c| c.is_alphabetic() || c.is_whitespace());
    if !prefix_ok {
        return None;
    }
    let v: f64 = digits.trim().parse().ok()?;
    (v > 0.0 && v < 180.0).then_some(v)
}

/// 접기. 실패하면 사용자에게 보여줄 문장을 돌려준다.
pub fn fold(parsed: &Parsed, opts: &FoldOptions) -> Result<Model, String> {
    let mut warn = parsed.warnings.clone();
    let mut info = read_info(&parsed.entities);
    let t = opts.thickness.unwrap_or(if info.thickness > 0.0 { info.thickness } else { 1.0 });
    if opts.thickness.is_none() && info.thickness <= 0.0 {
        warn.push("두께 정보(T:)가 없어 1mm로 그렸습니다.".into());
    }
    info.thickness = t;
    let d90_fn = opts.d90.map(|d| deduction_from_d90(d, t));
    let mut out_segs = Vec::new();
    let mut bends_in: Vec<(P2, P2, f64, String)> = Vec::new();
    let mut marks: Vec<Vec<P2>> = Vec::new();
    let mut circles: Vec<(P2, f64)> = Vec::new();
    let mut texts: Vec<(P2, f64, String, bool)> = Vec::new();
    for e in &parsed.entities {
        let layer = match e {
            Entity::Line { layer, .. }
            | Entity::Circle { layer, .. }
            | Entity::Arc { layer, .. }
            | Entity::Poly { layer, .. }
            | Entity::Text { layer, .. } => layer,
        };
        let up = layer.to_ascii_uppercase();
        if up == "DEFPOINTS" {
            continue;
        }
        if let Entity::Text { p, h, text, mtext, .. } = e {
            texts.push((*p, *h, text.clone(), *mtext));
            continue;
        }
        if up == "NCK-RK" {
            match e {
                Entity::Line { a, b, .. } => marks.push(vec![*a, *b]),
                Entity::Poly { pts, closed, .. } => {
                    let s = poly_segs(pts, *closed);
                    if let Some(f) = s.first() {
                        let mut v = vec![f.0];
                        v.extend(s.iter().map(|q| q.1));
                        marks.push(v);
                    }
                }
                Entity::Circle { c, r, .. } => marks.push(arc_pts(*c, *r, 0.0, 2.0 * PI)),
                Entity::Arc { c, r, a0, a1, .. } => marks.push(arc_pts(*c, *r, *a0, *a1)),
                _ => {}
            }
            continue;
        }
        if let Some(dir) = bend_dir(layer) {
            match e {
                Entity::Line { a, b, .. } => bends_in.push((*a, *b, dir, layer.clone())),
                Entity::Poly { pts, closed, .. } => {
                    for s in poly_segs(pts, *closed) {
                        bends_in.push((s.0, s.1, dir, layer.clone()));
                    }
                }
                _ => {}
            }
            continue;
        }
        match e {
            Entity::Line { a, b, .. } => out_segs.push((*a, *b)),
            Entity::Circle { c, r, .. } => circles.push((*c, *r)),
            Entity::Arc { c, r, a0, a1, .. } => out_segs.extend(arc_pts(*c, *r, *a0, *a1).windows(2).map(|w| (w[0], w[1]))),
            Entity::Poly { pts, closed, .. } => match poly_circle(pts, *closed) {
                Some(c) => circles.push(c),
                None => out_segs.extend(poly_segs(pts, *closed)),
            },
            Entity::Text { .. } => {}
        }
    }
    if out_segs.is_empty() {
        return Err("외곽선을 찾지 못했습니다.".into());
    }
    // 절곡 각도 글자
    let mut bend_ang: Vec<f64> = vec![90.0; bends_in.len()];
    for (p, h, text, mtext) in &texts {
        if *mtext && ["P:", "M:", "T:", "Q:"].iter().any(|k| text.contains(k)) {
            continue;
        }
        let Some(mut v) = angle_text(text) else { continue };
        if opts.angle_is_included {
            v = 180.0 - v;
        }
        let lim = (h * 4.0).max(15.0);
        let mut best: Option<(f64, usize)> = None;
        for (k, b) in bends_in.iter().enumerate() {
            let (d, _) = seg_dist(*p, b.0, b.1);
            if d < lim && best.is_none_or(|x| d < x.0) {
                best = Some((d, k));
            }
        }
        if let Some((_, k)) = best {
            bend_ang[k] = v;
        }
    }
    let bend_pairs: Vec<(P2, P2)> = bends_in.iter().map(|b| (b.0, b.1)).collect();
    let g = build_faces(&out_segs, &bend_pairs, opts.tol);
    if !g.dangling.is_empty() {
        warn.push(format!("외곽선에 닿지 않는 절곡선 {}개는 건너뛰었습니다.", g.dangling.len()));
    }
    // 판 / 빈 공간
    struct F {
        pts: Vec<P2>,
        halves: Vec<usize>,
        ip: P2,
        area: f64,
        depth: usize,
        holes: Vec<Vec<P2>>,
        circles: Vec<[f64; 3]>,
        marks: Vec<Vec<P2>>,
    }
    let outer: Vec<&Vec<P2>> = g.loops.iter().filter(|l| l.2 < -1e-6).map(|l| &l.1).collect();
    let mut faces: Vec<F> = Vec::new();
    let mut voids: Vec<P2> = Vec::new();
    let mut void_pts: Vec<Vec<P2>> = Vec::new();
    let all_loops: Vec<&Vec<P2>> = g.loops.iter().map(|l| &l.1).collect();
    for l in g.loops.iter().filter(|l| l.2 > 1e-6) {
        let ip = interior_point(&l.1, &all_loops);
        let depth = outer.iter().filter(|o| point_in_poly(ip, o)).count();
        if depth % 2 == 1 {
            faces.push(F { pts: l.1.clone(), halves: l.0.clone(), ip, area: l.2, depth, holes: vec![], circles: vec![], marks: vec![] });
        } else {
            voids.push(ip);
            void_pts.push(l.1.clone());
        }
    }
    if faces.is_empty() {
        return Err("닫힌 판 모양을 찾지 못했습니다. 외곽선이 끊어져 있는지 확인하세요.".into());
    }
    let face_at = |p: P2, faces: &[F]| -> Option<usize> {
        let mut best: Option<usize> = None;
        for (k, f) in faces.iter().enumerate() {
            if point_in_poly(p, &f.pts) && best.is_none_or(|b| f.depth > faces[b].depth) {
                best = Some(k);
            }
        }
        best
    };
    for (ip, pts) in voids.iter().zip(void_pts) {
        if let Some(k) = face_at(*ip, &faces) {
            faces[k].holes.push(pts);
        }
    }
    for (c, r) in &circles {
        if let Some(k) = face_at(*c, &faces) {
            faces[k].circles.push([c[0], c[1], *r]);
        }
    }
    for m in marks {
        let (Some(&m0), Some(&mm)) = (m.first(), m.get(m.len() / 2)) else { continue };
        if let Some(k) = face_at(m0, &faces).or_else(|| face_at(mm, &faces)) {
            faces[k].marks.push(m);
        }
    }
    let mut face_of_half: HashMap<usize, usize> = HashMap::new();
    for (k, f) in faces.iter().enumerate() {
        for &h in &f.halves {
            face_of_half.insert(h, k);
        }
    }
    let mut bend_faces: Vec<Vec<usize>> = vec![Vec::new(); bends_in.len()];
    for (hk, h) in g.half.iter().enumerate() {
        if let Some(bi) = g.edges[h.2].2
            && let Some(&f) = face_of_half.get(&hk)
            && !bend_faces[bi].contains(&f)
        {
            bend_faces[bi].push(f);
        }
    }
    // 연결: (절곡, f1=부모 후보, f2, flat, gap)
    struct Link {
        bend: usize,
        f1: usize,
        f2: usize,
        flat: bool,
        gap: f64,
    }
    let mut links: Vec<Link> = Vec::new();
    let (mut n_flat, mut n_expl) = (0, 0);
    for (bi, fs) in bend_faces.iter().enumerate() {
        if g.dangling.contains(&bi) {
            continue;
        }
        if fs.len() >= 2 {
            links.push(Link { bend: bi, f1: fs[0], f2: fs[1], flat: true, gap: 0.0 });
            n_flat += 1;
        } else if fs.len() == 1 {
            let child = fs[0];
            let (a, b) = (bends_in[bi].0, bends_in[bi].1);
            let ax = unit(sub(b, a));
            let len = (b[0] - a[0]).hypot(b[1] - a[1]);
            let mut n = [-ax[1], ax[0]];
            if dot(sub(faces[child].ip, a), n) > 0.0 {
                n = [-n[0], -n[1]];
            }
            let mut best: Option<(f64, usize)> = None;
            for (k, f) in faces.iter().enumerate() {
                if k == child {
                    continue;
                }
                let ds: Vec<f64> = f.pts.iter().map(|p| dot(sub(*p, a), n)).collect();
                if ds.iter().cloned().fold(f64::INFINITY, f64::min) < -1e-6 {
                    continue;
                }
                let ts: Vec<f64> = f.pts.iter().map(|p| dot(sub(*p, a), ax)).collect();
                if ts.iter().cloned().fold(f64::NEG_INFINITY, f64::max) < 1e-6 || ts.iter().cloned().fold(f64::INFINITY, f64::min) > len - 1e-6 {
                    continue;
                }
                let gap = ds.iter().cloned().fold(f64::INFINITY, f64::min);
                if best.is_none_or(|x| gap < x.0) {
                    best = Some((gap, k));
                }
            }
            if let Some((gap, p)) = best {
                links.push(Link { bend: bi, f1: p, f2: child, flat: false, gap });
                n_expl += 1;
            } else {
                push_warn(&mut warn, "붙일 면을 찾지 못한 절곡선이 있습니다.".into());
            }
        }
    }
    let zero = |_: f64| 0.0;
    let deduct: &dyn Fn(f64) -> f64 = match (opts.deduction, &d90_fn) {
        (Some(f), _) => f,
        (None, Some(f)) => f,
        (None, None) => {
            if n_flat > 0 {
                warn.push("한 장 전개도인데 연신율 값이 없어 0으로 접었습니다. 재질·두께별 연신율을 넣으면 외치수가 맞춰집니다.".into());
            }
            &zero
        }
    };
    // 기준 면
    let child_set: HashSet<usize> = links.iter().filter(|l| !l.flat).map(|l| l.f2).collect();
    let root = opts
        .root_face
        .filter(|&r| r < faces.len())
        .unwrap_or_else(|| (0..faces.len()).filter(|k| !child_set.contains(k)).max_by(|&a, &b| faces[a].area.total_cmp(&faces[b].area)).unwrap_or(0));
    // 트리
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); faces.len()];
    for (li, l) in links.iter().enumerate() {
        adj[l.f1].push(li);
        if l.flat {
            adj[l.f2].push(li);
        }
    }
    let mut place: Vec<Option<Rigid>> = vec![None; faces.len()];
    let mut parent: Vec<Option<(usize, usize, f64)>> = vec![None; faces.len()]; // (부모, 링크, D)
    let mut axis_of: Vec<Option<(P2, P2)>> = vec![None; faces.len()]; // (축 위 점, 부모→자식 방향)
    let root_sum: f64 = links.iter().filter(|l| l.f1 == root || l.f2 == root).map(|l| bends_in[l.bend].2).sum();
    // 두께가 붙는 쪽 (면 법선 기준 +1 / -1)
    let mut side_of: Vec<f64> = vec![1.0; faces.len()];
    if let Some(s) = side_of.get_mut(root) {
        *s = if root_sum >= 0.0 { 1.0 } else { -1.0 };
    }
    place[root] = Some(Rigid::I);
    let mut order = vec![root];
    let mut qi = 0;
    while qi < order.len() {
        let f = order[qi];
        qi += 1;
        for &li in &adj[f] {
            let l = &links[li];
            let c = if l.f1 == f { l.f2 } else { l.f1 };
            if place[c].is_some() {
                continue;
            }
            let (a, b, dir, _) = &bends_in[l.bend];
            let ax = unit(sub(*b, *a));
            let mut n = [-ax[1], ax[0]];
            if dot(sub(faces[c].ip, *a), n) < 0.0 {
                n = [-n[0], -n[1]];
            }
            let ang = bend_ang[l.bend];
            let (shift, axis_off, d) = if l.flat {
                let d = deduct(ang);
                (d, d / 2.0, d)
            } else {
                (-l.gap, -l.gap, 0.0)
            };
            // 절곡의 바깥(외치수) 쪽: 부모 두께와 같은 쪽으로 꺾으면 부모의 외치수면, 반대로 꺾으면 두께만큼 떨어진 반대 면
            let parent_side = side_of.get(f).copied().unwrap_or(1.0);
            let z_off = if *dir == parent_side { 0.0 } else { parent_side * t };
            let p = [a[0] + n[0] * axis_off, a[1] + n[1] * axis_off, z_off];
            let k = [n[1], -n[0], 0.0];
            let m = Rigid::rot_about(p, k, dir * ang * DEG).mul(&Rigid::trans([n[0] * shift, n[1] * shift, z_off]));
            if let Some(s) = side_of.get_mut(c) {
                *s = *dir;
            }
            place[c] = Some(place[f].unwrap_or(Rigid::I).mul(&m));
            parent[c] = Some((f, li, d));
            axis_of[c] = Some(([a[0] + n[0] * (axis_off - shift), a[1] + n[1] * (axis_off - shift)], n));
            order.push(c);
        }
    }
    let unplaced = place.iter().filter(|p| p.is_none()).count();
    if unplaced > 0 {
        warn.push(format!("연결되지 않은 판 {unplaced}개가 있어 빼고 그렸습니다."));
    }
    // 외치수까지 늘리기 + 결과
    let mut out_faces = Vec::new();
    let mut dims = Vec::new();
    let (mut mn, mut mx) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for &f in &order {
        let mut pts = faces[f].pts.clone();
        for l in links.iter().filter(|l| l.flat && (l.f1 == f || l.f2 == f)) {
            let other = if l.f1 == f { l.f2 } else { l.f1 };
            if place[other].is_none() {
                continue;
            }
            let (a, b, _, _) = &bends_in[l.bend];
            let ax = unit(sub(*b, *a));
            let mut n_out = [-ax[1], ax[0]];
            if dot(sub(faces[f].ip, *a), n_out) > 0.0 {
                n_out = [-n_out[0], -n_out[1]];
            }
            let d = deduct(bend_ang[l.bend]);
            if d == 0.0 {
                continue;
            }
            let e = [n_out[0] * d / 2.0, n_out[1] * d / 2.0];
            let on = |p: P2| seg_dist(p, *a, *b).0 < 1e-3;
            pts = clean_poly(&pts);
            let mut res = Vec::new();
            for k in 0..pts.len() {
                let (p, q) = (pts[k], pts[(k + 1) % pts.len()]);
                res.push(p);
                if on(p) && on(q) {
                    res.push([p[0] + e[0], p[1] + e[1]]);
                    res.push([q[0] + e[0], q[1] + e[1]]);
                }
            }
            pts = clean_poly(&res);
        }
        let pl = place[f].unwrap_or(Rigid::I);
        let side = side_of.get(f).copied().unwrap_or(1.0);
        for p in &pts {
            let q = pl.apply([p[0], p[1], 0.0]);
            for k in 0..3 {
                mn[k] = mn[k].min(q[k]);
                mx[k] = mx[k].max(q[k]);
            }
        }
        let bend = parent[f].map(|(_, li, d)| {
            let l = &links[li];
            BendOfFace { layer: bends_in[l.bend].3.clone(), angle: bend_ang[l.bend], d, flat: l.flat }
        });
        let outer = axis_of[f].map(|(o, n)| (pts.iter().map(|p| dot(sub(*p, o), n)).fold(f64::NEG_INFINITY, f64::max) * 1000.0).round() / 1000.0);
        // 절곡으로 생기는 치수: 이 면에 닿는 절곡선마다(같은 방향은 한 번) 절곡선에 직각인 외치수,
        // 외치수면(z=0) 위, 면 끝(절곡선 방향으로 가장 앞) 모서리에
        let mut seen: Vec<P2> = Vec::new();
        for l in links.iter().filter(|l| l.f1 == f || l.f2 == f) {
            let other = if l.f1 == f { l.f2 } else { l.f1 };
            if place[other].is_none() {
                continue;
            }
            let (a, b, _, _) = &bends_in[l.bend];
            let ax = unit(sub(*b, *a));
            if seen.iter().any(|s| dot(*s, ax).abs() > 0.999) {
                continue;
            }
            seen.push(ax);
            let n = [-ax[1], ax[0]];
            let (mut d0, mut d1, mut t0) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY);
            for p in &pts {
                let (d, t) = (dot(*p, n), dot(*p, ax));
                d0 = d0.min(d);
                d1 = d1.max(d);
                t0 = t0.min(t);
            }
            if !(d1 - d0 > 0.05) {
                continue;
            }
            let pa = [n[0] * d0 + ax[0] * t0, n[1] * d0 + ax[1] * t0];
            let pb = [n[0] * d1 + ax[0] * t0, n[1] * d1 + ax[1] * t0];
            let r = &pl.r;
            let o = [-(r[0] * ax[0] + r[1] * ax[1]), -(r[3] * ax[0] + r[4] * ax[1]), -(r[6] * ax[0] + r[7] * ax[1])];
            dims.push(FaceDim {
                face: f,
                a: pl.apply([pa[0], pa[1], 0.0]),
                b: pl.apply([pb[0], pb[1], 0.0]),
                o,
                v: ((d1 - d0) * 1000.0).round() / 1000.0,
            });
        }
        out_faces.push(Face {
            id: f,
            root: f == root,
            pts,
            src: faces[f].pts.clone(),
            holes: faces[f].holes.clone(),
            circles: faces[f].circles.clone(),
            marks: faces[f].marks.clone(),
            place: pl,
            side,
            parent: parent[f].map(|p| p.0),
            outer,
            bend,
        });
    }
    let mode = match (n_flat > 0, n_expl > 0) {
        (true, true) => "혼합",
        (true, false) => "한 장 전개도",
        (false, true) => "펼친 면",
        _ => "평판",
    };
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    Ok(Model {
        mode: mode.into(),
        info,
        thickness: t,
        faces: out_faces,
        bends: links
            .iter()
            .map(|l| Bend { layer: bends_in[l.bend].3.clone(), angle: bend_ang[l.bend], flat: l.flat, a: bends_in[l.bend].0, b: bends_in[l.bend].1 })
            .collect(),
        // 평판은 두께가 높이 (면이 하나라 외치수면만으로는 0)
        size: [r3(mx[0] - mn[0]), r3(mx[1] - mn[1]), if mode == "평판" { r3(t) } else { r3(mx[2] - mn[2]) }],
        dims,
        warnings: warn,
    })
}

/// DXF 글자를 바로 접는다.
pub fn fold_dxf(text: &str, opts: &FoldOptions) -> Result<Model, String> {
    fold(&parse_dxf(text), opts)
}
