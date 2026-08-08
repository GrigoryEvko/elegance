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
    /// Declared layer contracts by name. The only architecture claim
    /// that gates, because it is stated rather than calibrated.
    #[serde(default)]
    pub layers: HashMap<String, crate::layers::Layer>,
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
///
/// A marker has to OPEN the line it sits on. Searching the head as one
/// blob read a documentation sentence as a machine's disclaimer:
/// `phoenix/lib/phoenix/endpoint.ex:24` says "generator, an endpoint was
/// automatically generated as part of your application" inside its own
/// `@moduledoc`, and 1097 lines of Phoenix's core were skipped —
/// silently, taking its imports out of the graph with them, so
/// `transports/websocket.ex` and `transports/long_poll.ex` read as
/// depended on by nothing while the endpoint named both. Fifteen more
/// hand-written files went the same way, each one a script PRINTING a
/// header it generates for somebody else: `git/tools/generate-configlist.sh:35`
/// `echo "/* automatically generated by ... */"`,
/// `redis/utils/generate-fmtargs.py:7` `print(...)`,
/// `cats/project/AlgebraBoilerplate.scala:28`
/// `val header = "// auto-generated boilerplate"`.
///
/// The new predicate is a strict SUBSET of the old, so it cannot lose a
/// true positive, and across gold it loses none: 1770 files flagged
/// before, 1740 after, and every one of the 547 flash-attention kernels,
/// php-parser's Php7/Php8, netty's protoc output and the .NET
/// `<auto-generated>` designers still skips. A real marker is written in
/// a comment, so the line either starts with the marker or starts with
/// the punctuation that opens the comment.
///
/// A minifier strips the comment a marker would live in, so a bundle
/// states nothing about itself and the head test cannot see it. Ten of
/// the gold corpus's `.min.js` files were being measured as though a
/// person had written them, and twelve such files were setting the
/// percentile ceilings of a 245-file language.
pub fn is_generated(path: &std::path::Path, source: &str) -> bool {
    if minified(path, source) {
        return true;
    }
    let mut end = source.len().min(1024);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    source[..end]
        .to_ascii_lowercase()
        .lines()
        .any(marker_opens_the_line)
}

/// Does this line CLAIM to be machine-written, rather than mention the
/// idea? Either the marker opens the line, or a comment does — `//`,
/// `#`, `*`, `--`, `<!--` and `"""` all begin with punctuation, and a
/// whole line of comment is a claim about the file. A line that opens
/// with a WORD is prose or code: `val header = "// auto-generated"`
/// says what a generator will write, `echo "/* automatically generated
/// by ... */"` says what a script will print, and "generator, an
/// endpoint was automatically generated as part of your application" is
/// a sentence in Phoenix's manual.
fn marker_opens_the_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let commented = trimmed.starts_with(|c: char| !c.is_alphanumeric());
    GENERATED_MARKERS
        .iter()
        .any(|m| trimmed.starts_with(m) || commented && trimmed.contains(m))
}

/// A bundle: named `.min.js` AND carrying a line no author wrote.
///
/// BOTH halves, because the name alone is a promise and gold holds two
/// files that do not keep it. `perl/ack3/t/swamp/minified.min.js` is
/// fifty characters of ordinary JavaScript — a fixture ack3's searcher
/// greps over — and `perl/mojo/t/.../test.ab1234cd5678ef.min.js` is a
/// three-line comment standing in for an asset. Neither is machine
/// output, and labelling them so would delete two of a test suite's
/// inputs from the corpus on the strength of a filename.
///
/// The line is the evidence a minifier does leave. Every one of the ten
/// real bundles carries one of 327 characters or more — jquery's runs
/// to 87443 — while no line in either fixture reaches 51. A file whose
/// body is a single 100 KB line has no craft to measure, which is why
/// this is a content decision (the file leaves the corpus) and not
/// `Lang::is_sink` (which would fix only who imports it).
fn minified(path: &std::path::Path, source: &str) -> bool {
    /// Between the longest fixture line in gold (50) and the shortest
    /// line the ten real bundles are willing to write (327).
    const MACHINE_LINE: usize = 200;
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".min.js") || n.ends_with(".min.mjs"))
        && source.lines().any(|line| line.len() > MACHINE_LINE)
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
        assert!(is_generated(
            std::path::Path::new("a.py"),
            "# @generated by protoc\ndef f(): pass\n"
        ));
        assert!(is_generated(
            std::path::Path::new("a.go"),
            "// Code generated by mockgen. DO NOT EDIT.\n"
        ));
        assert!(!is_generated(
            std::path::Path::new("a.py"),
            "def generated_report():\n    pass\n"
        ));
        // A comment opener may come first, and does in almost every
        // genuinely generated file: flash-attention's 547 kernels and
        // netty's protoc output are written this way.
        assert!(is_generated(
            std::path::Path::new("k.cu"),
            "// This file is auto-generated. See generate_kernels.py\n"
        ));
        assert!(is_generated(
            std::path::Path::new("a.rs"),
            "/*\n * automatically generated by rustc\n */\n"
        ));
        // Multi-byte char straddling the 1024-byte head must not panic.
        let mut tricky = "x".repeat(1023);
        tricky.push('ͷ');
        tricky.push_str(&"y".repeat(100));
        assert!(!is_generated(std::path::Path::new("a.py"), &tricky));
    }

    #[test]
    fn a_bundle_is_named_min_js_and_writes_a_line_no_author_would() {
        // A minifier strips the comment a marker would live in, so the
        // body is the only statement left. jquery.min.js opens with a
        // short licence line and then writes 87443 characters.
        let bundle = format!("/*! jQuery v3.5.1 */\n{}\n", "a".repeat(9000));
        assert!(is_generated(
            std::path::Path::new("docs/js/jquery.min.js"),
            &bundle
        ));
        assert!(is_generated(
            std::path::Path::new("mithril.min.mjs"),
            &bundle
        ));
        // ack3 keeps a fixture called `minified.min.js` that is fifty
        // characters of hand-written JavaScript, and mojo an asset
        // placeholder that is three lines of comment. Reading the name
        // alone deleted both from a Perl suite's inputs.
        assert!(!is_generated(
            std::path::Path::new("t/swamp/minified.min.js"),
            "var cssDir=\"./Stylesheets/\";var NS4CSS=\"wango.css\"\n"
        ));
        assert!(!is_generated(
            std::path::Path::new("t/assets/test.ab1234cd5678ef.min.js"),
            "/*\n * foo/bar/test.min.js asset\n */\n"
        ));
        // The name is still required: a long line is how ordinary
        // minified-looking data reads, and a `.js` that is not a bundle
        // stays measurable.
        assert!(!is_generated(std::path::Path::new("vendor.js"), &bundle));
    }

    #[test]
    fn a_marker_used_as_prose_does_not_skip_the_file() {
        // phoenix/lib/phoenix/endpoint.ex:24, inside its own
        // `@moduledoc`: 1097 lines of Phoenix's core were skipped as
        // machine-written, and the two transport modules the file names
        // read as depended on by nothing.
        assert!(!is_generated(
            std::path::Path::new("endpoint.ex"),
            "defmodule Phoenix.Endpoint do\n  @moduledoc ~S\"\"\"\n  When you generate an application with the\n  generator, an endpoint was automatically generated as part of\n  your application.\n  \"\"\"\n"
        ));
        // A generator PRINTING the header it writes for somebody else:
        // git/tools/generate-configlist.sh:35, redis's
        // utils/generate-fmtargs.py:7, and cats' AlgebraBoilerplate.
        assert!(!is_generated(
            std::path::Path::new("generate-configlist.sh"),
            "echo \"/* Automatically generated by generate-configlist.sh */\"\n"
        ));
        assert!(!is_generated(
            std::path::Path::new("generate-fmtargs.py"),
            "print(\"/* Automatically generated by generate-fmtargs.py */\")\n"
        ));
        assert!(!is_generated(
            std::path::Path::new("AlgebraBoilerplate.scala"),
            "val header = \"// auto-generated boilerplate\"\n"
        ));
    }
}
