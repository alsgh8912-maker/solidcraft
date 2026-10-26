//! `cargo run -p solidcraft-sheetfold --example fold_file -- part.dxf [d90]` prints the folded size.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: fold_file <part.dxf> [d90]");
        return;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{path}: {e}");
            return;
        }
    };
    let d90 = args.get(2).and_then(|s| s.parse().ok());
    let opts = solidcraft_sheetfold::FoldOptions { d90, ..Default::default() };
    match solidcraft_sheetfold::fold_dxf(&text, &opts) {
        Ok(m) => println!("{} · {:?} · {} faces · size {:?} · {:?}", m.mode, m.info.name, m.faces.len(), m.size, m.warnings),
        Err(e) => eprintln!("{e}"),
    }
}
