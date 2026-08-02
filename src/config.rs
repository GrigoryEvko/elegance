//! Per-repo configuration (`.elegance.toml` at the scan root) and
//! generated-code detection. Config is the taste knob: exclusions and
//! budget overrides; everything else stays in code.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::metrics::{LangBudgets, METRICS};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Glob patterns excluded from the walk (`vendor/**`, `*_pb2.py`).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Extra directory names to skip anywhere in the tree.
    #[serde(default)]
    pub skip_dirs: Vec<String>,
    /// Budget overrides by metric name: `length = { hi = 100 }`.
    #[serde(default)]
    budgets: HashMap<String, Override>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    lo: Option<f32>,
    hi: Option<f32>,
}

/// Budgets that vary by where a file lives. A monorepo's vendored
/// service and its core library are different codebases wearing one
/// checkout, and one budget for both is either too loose for the core or
/// impossible for the service.
///
/// Nearest config wins, so a package tightens or relaxes only itself.
#[derive(Clone)]
pub struct Layers {
    root: LangBudgets,
    /// (directory, budgets), longest matching prefix wins. Shared rather
    /// than copied: rayon builds one aggregate per task and LangBudgets
    /// is several kilobytes.
    nested: std::sync::Arc<Vec<(std::path::PathBuf, LangBudgets)>>,
}

impl Layers {
    /// One budget for the whole tree — what a single-project repo has.
    pub fn flat(root: LangBudgets) -> Layers {
        Layers {
            root,
            nested: std::sync::Arc::new(Vec::new()),
        }
    }

    /// The root budgets, for labels and for anything the layers cannot
    /// attribute to a file.
    pub fn root(&self) -> &LangBudgets {
        &self.root
    }

    /// The budgets in force where this file lives.
    pub fn for_file(&self, path: &Path) -> &LangBudgets {
        self.nested
            .iter()
            .filter(|(dir, _)| path.starts_with(dir))
            .max_by_key(|(dir, _)| dir.components().count())
            .map_or(&self.root, |(_, b)| b)
    }

    /// How many packages set their own budgets — worth saying out loud,
    /// since a reader of one number should know it is not one number.
    pub fn count(&self) -> usize {
        self.nested.len()
    }
}

impl Config {
    /// Loads `.elegance.toml` from the scan root; absence is a default
    /// config, malformation is an error the user must see.
    pub fn load(root: &Path) -> Result<Config, String> {
        let path = root.join(".elegance.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Config::default());
        };
        let config: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for name in config.budgets.keys() {
            if !METRICS.iter().any(|d| d.name == name) {
                return Err(format!("{}: unknown metric {name:?}", path.display()));
            }
        }
        Ok(config)
    }

    /// Calibrated per-language budgets with this repo's overrides applied
    /// uniformly across languages.
    pub fn budgets(&self) -> LangBudgets {
        self.budgets_over(&Config::default())
    }

    /// This config's overrides on top of an outer one: a package states
    /// only what differs from the repository it lives in.
    pub fn budgets_over(&self, outer: &Config) -> LangBudgets {
        let mut lb = outer.apply(LangBudgets::calibrated());
        lb = self.apply(lb);
        lb
    }

    /// This config's overrides, applied uniformly across languages. An
    /// override names a metric, not a metric-and-language: a team that
    /// wants longer functions wants them everywhere.
    fn apply(&self, mut lb: LangBudgets) -> LangBudgets {
        for (name, o) in &self.budgets {
            let Some(m) = METRICS.iter().position(|d| d.name == name) else {
                continue;
            };
            for budgets in &mut lb.0 {
                override_band(&mut budgets.0[m], o);
            }
        }
        lb
    }
}

/// A stated bound replaces the calibrated one; an unstated bound keeps it.
fn override_band(band: &mut (Option<f32>, Option<f32>), o: &Override) {
    band.0 = o.lo.or(band.0);
    band.1 = o.hi.or(band.1);
}

/// Every `.elegance.toml` under the scan root, nearest-wins. The root's
/// own config is the base; each nested one is layered on top of it, so a
/// package states only its differences.
pub fn layers(root: &Path) -> Result<Layers, String> {
    let base = Config::load(root)?;
    let mut nested = Vec::new();
    // `.elegance.toml` is a dotfile, and the walker hides those by
    // default — the config would never find itself.
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.file_name().is_none_or(|n| n != ".elegance.toml") {
            continue;
        }
        let Some(dir) = path.parent() else { continue };
        if dir == root {
            continue; // that is the base
        }
        let local = Config::load(dir)?;
        nested.push((dir.to_path_buf(), local.budgets_over(&base)));
    }
    nested.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Layers {
        root: base.budgets(),
        nested: std::sync::Arc::new(nested),
    })
}

/// Markers conventionally placed near the top of machine-written files.
const GENERATED_MARKERS: &[&str] = &[
    "@generated",
    "do not edit",
    "automatically generated",
    "autogenerated",
    "auto-generated",
    "code generated by",
];

/// Machine-written code is content, not craft: it is skipped entirely.
/// Only the head of the file is checked, per convention.
pub fn is_generated(source: &str) -> bool {
    let mut end = source.len().min(1024);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let head = source[..end].to_ascii_lowercase();
    GENERATED_MARKERS.iter().any(|m| head.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_overrides_apply_and_unknown_metrics_are_rejected() {
        let config: Config = toml::from_str(
            "exclude = [\"vendor/**\"]\n[budgets]\nlength = { hi = 100.0 }\n\"comment ratio\" = { lo = 0.05 }\n",
        )
        .unwrap();
        let lb = config.budgets();
        let length = METRICS.iter().position(|d| d.name == "length").unwrap();
        let ratio = METRICS
            .iter()
            .position(|d| d.name == "comment ratio")
            .unwrap();
        // Overrides apply uniformly across languages, on top of calibration.
        for lang in crate::lang::LANGS {
            let b = lb.for_lang(lang);
            assert_eq!(b.0[length].1, Some(100.0), "{lang:?}");
            assert_eq!(b.0[ratio].0, Some(0.05), "{lang:?}");
        }

        let bad: Result<Config, _> = toml::from_str("[budgets]\nnope = { hi = 1.0 }\n");
        // Unknown metric names surface via load(); parsing alone accepts them.
        assert!(bad.is_ok());
    }

    #[test]
    fn the_nearest_config_wins_and_states_only_its_differences() {
        let dir = std::env::temp_dir().join(format!("elegance-layers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pkg/legacy")).unwrap();
        std::fs::write(
            dir.join(".elegance.toml"),
            "[budgets]\nlength = { hi = 50.0 }\ndepth = { hi = 3.0 }\n",
        )
        .unwrap();
        // The legacy package relaxes length only; depth must survive.
        std::fs::write(
            dir.join("pkg/legacy/.elegance.toml"),
            "[budgets]\nlength = { hi = 400.0 }\n",
        )
        .unwrap();

        let layers = layers(&dir).unwrap();
        assert_eq!(layers.count(), 1, "the root's own config is the base");
        let length = METRICS.iter().position(|d| d.name == "length").unwrap();
        let depth = METRICS.iter().position(|d| d.name == "depth").unwrap();
        let lang = crate::lang::Lang::Python;

        let core = layers.for_file(&dir.join("pkg/core/a.py"));
        assert_eq!(core.for_lang(lang).0[length].1, Some(50.0));

        let legacy = layers.for_file(&dir.join("pkg/legacy/b.py"));
        assert_eq!(legacy.for_lang(lang).0[length].1, Some(400.0));
        assert_eq!(
            legacy.for_lang(lang).0[depth].1,
            Some(3.0),
            "a package states only its differences; the rest is inherited"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_files_are_recognized_by_head_markers() {
        assert!(is_generated("# @generated by protoc\ndef f(): pass\n"));
        assert!(is_generated("// Code generated by mockgen. DO NOT EDIT.\n"));
        assert!(!is_generated("def generated_report():\n    pass\n"));
        // Multi-byte char straddling the 1024-byte head must not panic.
        let mut tricky = "x".repeat(1023);
        tricky.push('ͷ');
        tricky.push_str(&"y".repeat(100));
        assert!(!is_generated(&tricky));
    }
}
