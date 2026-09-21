//! Every loop in `src` names the constant that bounds it, and the listing this
//! test prints is the receipt: one line per loop against its budget.

use std::fs;
use std::path::{Path, PathBuf};

/// How far above a loop its budget comment may sit.
const BUDGET_LOOKBACK: usize = 6;

const BUDGET_PREFIX: &str = "// budget:";

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    // budget: one directory entry per push; every pushed path comes from a
    // bounded read_dir listing
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn first_constant(budget: &str) -> Option<String> {
    budget
        .split(|character: char| !(character.is_ascii_uppercase() || character == '_'))
        .filter(|word| word.len() > 2 && word.chars().any(|c| c.is_ascii_uppercase()))
        .map(str::to_owned)
        .next()
}

#[test]
fn every_loop_names_the_constant_that_bounds_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&root);
    assert!(!files.is_empty(), "no sources found under {}", root.display());

    let mut listing: Vec<String> = Vec::new();
    let mut unbudgeted: Vec<String> = Vec::new();
    let mut unnamed: Vec<String> = Vec::new();
    let mut loops = 0usize;

    for path in &files {
        let text = fs::read_to_string(path).expect("source file");
        let lines: Vec<&str> = text.lines().collect();
        let relative = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        for (index, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if !(code.starts_with("loop {") || code.starts_with("while ") || code == "loop") {
                continue;
            }
            loops += 1;
            let start = index.saturating_sub(BUDGET_LOOKBACK);
            let budget = lines[start..index]
                .iter()
                .rev()
                .find(|candidate| candidate.trim_start().starts_with(BUDGET_PREFIX));
            match budget {
                Some(budget) => {
                    let named = first_constant(budget);
                    let declared = named
                        .as_ref()
                        .is_some_and(|name| text.contains(&format!("const {name}")));
                    if !declared {
                        unnamed.push(format!(
                            "{}:{} {}",
                            relative,
                            index + 1,
                            budget.trim()
                        ));
                    }
                    listing.push(format!(
                        "{}:{} {} <- {}",
                        relative,
                        index + 1,
                        code.trim(),
                        budget.trim()
                    ));
                }
                None => unbudgeted.push(format!("{}:{} {}", relative, index + 1, code)),
            }
        }
    }

    let receipt = listing.join("\n");
    assert!(
        unbudgeted.is_empty(),
        "loops without a budget line:\n{}\n\nbudgeted:\n{receipt}",
        unbudgeted.join("\n")
    );
    assert!(
        unnamed.is_empty(),
        "budget lines that name no declared constant:\n{}\n\nbudgeted:\n{receipt}",
        unnamed.join("\n")
    );
    assert!(loops > 0, "the scanner found no loops, so it proves nothing");
    println!("{loops} bounded loops:\n{receipt}");
}