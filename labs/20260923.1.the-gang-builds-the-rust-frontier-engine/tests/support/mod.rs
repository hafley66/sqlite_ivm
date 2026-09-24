//! Shared helpers: load the packet's expected TSVs and format actual rows the
//! same way, so every committed frontier is compared field for field.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The packet directory of this lab, relative to the crate.
pub fn packet() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plans/engine-iso")
}

/// Packet TSVs grouped by frontier label. `EMPTY` lines and absent frontiers
/// both yield no rows; the delta TSVs name empty frontiers explicitly, so
/// emptiness is asserted per frontier against the named group.
pub struct TsvGroups(BTreeMap<String, Vec<Vec<String>>>);

impl TsvGroups {
    pub fn load(path: &Path) -> Self {
        let text = fs::read_to_string(path).expect("packet TSV readable");
        let mut groups: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let fields: Vec<String> = line.split('\t').map(str::to_owned).collect();
            let frontier = fields[0].clone();
            if fields.len() == 2 && fields[1] == "EMPTY" {
                groups.entry(frontier).or_default();
                continue;
            }
            groups
                .entry(frontier)
                .or_default()
                .push(fields[1..].to_vec());
        }
        TsvGroups(groups)
    }

    /// Field rows of one frontier (first column stripped). Absent frontiers
    /// yield no rows.
    pub fn rows(&self, frontier: &str) -> &[Vec<String>] {
        self.0.get(frontier).map(Vec::as_slice).unwrap_or(&[])
    }
    /// Render a delta change as oracle fields: row cells then the signed
    /// weight column (`1` or `-1`).
    pub fn delta_fields(row: &[i64], weight: i64) -> Vec<String> {
        let mut fields: Vec<String> = row.iter().map(|cell| cell.to_string()).collect();
        fields.push(format!("{weight}"));
        fields
    }
}

/// Join frontier name and fields into one TSV line for comparison.
pub fn line(frontier: &str, fields: &[String]) -> Vec<String> {
    let mut full = vec![frontier.to_owned()];
    full.extend(fields.iter().cloned());
    full
}
