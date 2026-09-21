use std::{fs, path::Path};

/// Every construct that can iterate without a compiler-visible end carries a
/// `Bound:` comment within three lines above it.
#[test]
fn every_unbounded_construct_names_its_budget() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut offenders = Vec::new();
    let mut files = fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|kind| kind == "rs"))
        .collect::<Vec<_>>();
    files.sort();
    for path in &files {
        let text = fs::read_to_string(path).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        for (at, line) in lines.iter().enumerate() {
            let body = line.trim();
            let unbounded =
                body.starts_with("loop {") || body.starts_with("while ") || body == "loop";
            if !unbounded {
                continue;
            }
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            let budget = lines[at.saturating_sub(3)..at]
                .iter()
                .find(|above| above.contains("Bound:"))
                .map(|above| above.trim().trim_start_matches("// ").to_string());
            match budget {
                Some(budget) => found.push(format!("{file} {budget}")),
                None => offenders.push(format!("{file}:{}", at + 1)),
            }
        }
    }
    assert_eq!(offenders, Vec::<String>::new());
    assert_eq!(
        found,
        vec![
            "collector.rs Bound: staged.len() + spilled.len(). Each step moves one row out of one"
                .to_string()
        ]
    );
}
