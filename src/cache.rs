//! Content-addressed cache for the distribution scan.
//!
//! `--diff` positions a finding inside the repository's own distribution,
//! which needs every file's measurements even though the diff itself
//! touches a handful. Re-parsing the whole tree on every commit is the
//! cost of that; this pays it once.
//!
//! Only per-file METRIC VALUES are cached, and only the distribution
//! scan reads them. Clone hashes, the module graph and the recurrence
//! detectors are cross-file facts that a per-file cache cannot
//! reconstruct, so the full report never uses this. A cache that
//! silently degrades a report would cost more than the parsing it saves.
//!
//! Staleness is structural: entries are keyed by content hash, and the
//! whole file is discarded when the tool's behavioural fingerprint
//! (metric registry + grammar versions + version string) changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lang::LANGS;
use crate::metrics::METRICS;

const CACHE: &str = ".elegance/cache.json";

#[derive(Serialize, Deserialize, Default)]
pub struct Cache {
    /// What the tool computed with. A mismatch invalidates everything.
    fingerprint: u64,
    files: HashMap<PathBuf, Entry>,
    /// Entries touched this run; the rest are dropped on save so the
    /// cache tracks the working tree instead of growing forever.
    #[serde(skip)]
    live: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    content: u64,
    /// (metric index, value): everything a distribution needs.
    values: Vec<(u8, f32)>,
}

impl Cache {
    pub fn load(root: &Path) -> Cache {
        let fingerprint = fingerprint();
        let cached: Option<Cache> = std::fs::read_to_string(root.join(CACHE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        match cached {
            Some(c) if c.fingerprint == fingerprint => c,
            // Either absent, unreadable, or measured by a different tool:
            // all three mean "start over", none of them mean "fail".
            _ => Cache {
                fingerprint,
                ..Cache::default()
            },
        }
    }

    pub fn save(&self, root: &Path) {
        let kept: HashMap<&PathBuf, &Entry> = self
            .live
            .iter()
            .filter_map(|p| self.files.get_key_value(p))
            .collect();
        let file = serde_json::json!({
            "fingerprint": self.fingerprint,
            "files": kept,
        });
        let path = root.join(CACHE);
        if std::fs::create_dir_all(path.parent().expect("cache path has parent")).is_ok() {
            // A cache that cannot be written is a slow run, not an error.
            let _ = std::fs::write(path, file.to_string());
        }
    }

    /// Measurements for a file whose content matches what was cached.
    pub fn get(&mut self, path: &Path, content: u64) -> Option<Vec<(u8, f32)>> {
        let entry = self.files.get(path).filter(|e| e.content == content)?;
        let values = entry.values.clone();
        self.live.push(path.to_path_buf());
        Some(values)
    }

    pub fn insert(&mut self, path: &Path, content: u64, values: Vec<(u8, f32)>) {
        self.files
            .insert(path.to_path_buf(), Entry { content, values });
        self.live.push(path.to_path_buf());
    }
}

/// FNV-1a over the file's bytes: a content address rather than a
/// security hash. A collision costs one stale measurement, and no
/// verdict elsewhere.
pub fn content_hash(source: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for byte in source.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Everything that changes what a measurement MEANS: the metric
/// registry, the grammars, and the tool version. Any drift discards the
/// cache rather than mixing measurements from two different tools.
fn fingerprint() -> u64 {
    use std::fmt::Write;
    let mut parts = String::from(env!("CARGO_PKG_VERSION"));
    for def in METRICS {
        let _ = write!(parts, "{}{}{:?}{:?}", def.name, def.rung, def.lo, def.hi);
    }
    for lang in LANGS {
        let abi = lang.pack().ts.abi_version();
        let _ = write!(parts, "{}{abi}", lang.name());
    }
    content_hash(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_file_misses_and_an_unchanged_one_hits() {
        let mut cache = Cache::load(Path::new("/nonexistent"));
        let path = Path::new("a.py");
        let before = content_hash("def f(): pass\n");
        assert_eq!(cache.get(path, before), None, "cold cache");

        cache.insert(path, before, vec![(0, 3.0), (1, 4.0)]);
        assert_eq!(cache.get(path, before), Some(vec![(0, 3.0), (1, 4.0)]));

        let after = content_hash("def f(): return 1\n");
        assert_eq!(cache.get(path, after), None, "edited file must re-measure");
    }

    #[test]
    fn a_cache_from_a_different_tool_is_discarded() {
        let dir = std::env::temp_dir().join(format!("elegance-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".elegance")).unwrap();
        std::fs::write(
            dir.join(CACHE),
            r#"{"fingerprint":1,"files":{"a.py":{"content":7,"values":[[0,9.0]]}}}"#,
        )
        .unwrap();
        let mut stale = Cache::load(&dir);
        assert_eq!(
            stale.get(Path::new("a.py"), 7),
            None,
            "measurements from another registry must not be reused"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
