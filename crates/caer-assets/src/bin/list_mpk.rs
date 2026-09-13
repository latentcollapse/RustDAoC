fn main() {
    let path = std::env::args().nth(1).expect("path");
    let entries = caer_assets::open(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let mut names: Vec<_> = entries
        .into_iter()
        .map(|m| (m.name, m.data.len()))
        .collect();
    names.sort_by_key(|a| a.0.to_ascii_lowercase());
    for (n, len) in names {
        println!("{len:>8}  {n}");
    }
}
