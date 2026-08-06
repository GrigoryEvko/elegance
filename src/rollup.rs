//! Where the sludge lives, by directory.
//!
//! A flat offender list is the wrong shape for a monorepo: the worst
//! twenty units may all sit in one package nobody on your team owns, and
//! the list gives no way to see that. Rolling findings up to a directory
//! answers the question a team lead actually asks — which part of this
//! tree is in trouble — and it does it without a new measurement, since
//! every violation already carries a path.
//!
//! Density, not totals: a big directory has more of everything. The
//! ranking is violations per file, so a small rotten package outranks a
//! large healthy one.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

use crate::metrics::METRICS;
use crate::report::Agg;

/// Directories shown; beyond this nobody re-plans their week.
const SHOW: usize = 15;

/// A directory needs this many files before its rate means anything —
/// one file with one violation is a 100% rate and says nothing.
const MIN_FILES: u32 = 3;

#[derive(Default)]
struct Dir {
    files: u32,
    gates: u32,
    suspicions: u32,
    /// The metric contributing the most gated violations here.
    worst: HashMap<&'static str, u32>,
}

impl Dir {
    /// Gated violations per file: a small rotten package must outrank a
    /// large healthy one.
    fn density(&self) -> f64 {
        self.gates as f64 / self.files.max(1) as f64
    }

    fn driver(&self) -> &'static str {
        self.worst
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map_or("", |(name, _)| name)
    }
}

/// The worst directories by gated-violation density, for the summary
/// report, which has room for a line rather than a table.
pub fn worst_dirs(agg: &Agg, k: usize) -> Vec<(String, f64)> {
    let mut ranked: Vec<(String, Dir)> = tally(agg)
        .into_iter()
        // A directory whose density rounds to 0.0 tells the reader
        // nothing but occupies a slot a real hotspot wanted.
        .filter(|(_, d)| d.files >= MIN_FILES && d.density() >= 0.05)
        .collect();
    ranked.sort_by(|(a_dir, a), (b_dir, b)| {
        b.density()
            .total_cmp(&a.density())
            .then_with(|| b.gates.cmp(&a.gates))
            .then_with(|| a_dir.cmp(b_dir))
    });
    let parent_of = |(dir, _): &(String, Dir)| dir.rfind('/').map_or(0, |cut| cut + 1);
    let trim = ranked.iter().map(parent_of).min().unwrap_or(0);
    let named = |(dir, d): (String, Dir)| {
        let shown = dir[trim.min(dir.len())..].to_string();
        (shown, d.density())
    };
    ranked.into_iter().take(k).map(named).collect()
}

fn tally(agg: &Agg) -> HashMap<String, Dir> {
    let mut dirs: HashMap<String, Dir> = HashMap::new();
    for g in agg.graph.read() {
        let dir = dirs.entry(dir_of(&g.path)).or_default();
        dir.files += 1;
    }
    for v in agg.violations() {
        let def = &METRICS[v.metric];
        let dir = dir_of(Path::new(v.path));
        let entry = dirs.entry(dir).or_default();
        match def.rung {
            0..=2 => {
                entry.gates += 1;
                let driver = entry.worst.entry(def.name).or_insert(0);
                *driver += 1;
            }
            3..=4 => entry.suspicions += 1,
            _ => {}
        }
    }
    dirs
}

pub fn run(agg: &Agg, top: usize) -> String {
    render(tally(agg), top.max(SHOW))
}

fn render(dirs: HashMap<String, Dir>, show: usize) -> String {
    let mut ranked: Vec<(String, Dir)> = dirs
        .into_iter()
        .filter(|(_, d)| d.files >= MIN_FILES && d.gates > 0)
        .collect();
    if ranked.is_empty() {
        return "rollup — no directory carries a gated violation\n".to_string();
    }
    // Worst first, ties broken by gate count then by name so the
    // order never depends on the map's.
    ranked.sort_by(|(a_dir, a), (b_dir, b)| {
        b.density()
            .total_cmp(&a.density())
            .then_with(|| b.gates.cmp(&a.gates))
            .then_with(|| a_dir.cmp(b_dir))
    });
    let mut out = String::new();
    let _ = writeln!(
        out,
        "rollup — gated violations per file, worst first\n\n\
         {:>7} {:>6} {:>6} {:>11}  {:<14} directory",
        "per file", "gates", "files", "suspicions", "driver"
    );
    for (path, d) in ranked.iter().take(show) {
        let _ = writeln!(
            out,
            "{:>7.2} {:>6} {:>6} {:>11}  {:<14} {}",
            d.density(),
            d.gates,
            d.files,
            d.suspicions,
            d.driver(),
            if path.is_empty() { "." } else { path },
        );
    }
    if ranked.len() > show {
        let _ = writeln!(out, "  ... and {} more directories", ranked.len() - show);
    }
    let _ = writeln!(
        out,
        "\ndirectories under {MIN_FILES} files are omitted: one file with one\n\
         violation is a 100% rate and says nothing."
    );
    out
}

fn dir_of(path: &Path) -> String {
    path.parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::extract;
    use crate::lang::Lang;

    const VIOLATOR: &str = "def NAME(xs):\n    t = 0\n    for x in xs:\n        if x > 0:\n            if x > 1:\n                if x > 2:\n                    if x > 3:\n                        if x > 4:\n                            t += x\n    return t\n";

    #[test]
    fn a_small_rotten_directory_outranks_a_large_healthy_one() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let mut agg = Agg::complete();
        // Three bad files in one directory, twelve clean ones in another.
        for i in 0..3 {
            agg.add_file(&extract(
                pack,
                &mut parser,
                Path::new(&format!("bad/f{i}.py")),
                &VIOLATOR.replace("NAME", &format!("mess{i}")),
            ));
        }
        for i in 0..12 {
            agg.add_file(&extract(
                pack,
                &mut parser,
                Path::new(&format!("good/f{i}.py")),
                "def add(a, b):\n    return a + b\n",
            ));
        }
        let out = run(&agg, 5);
        let bad = out.find("bad").expect("bad directory ranked");
        assert!(
            !out.contains(" good"),
            "a directory with no gated violation is not a finding:\n{out}"
        );
        assert!(out[..bad].contains("per file"), "header precedes the rows");
        assert!(out.contains("depth") || out.contains("cognitive"), "{out}");
    }

    #[test]
    fn a_single_file_directory_is_not_a_rate() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let mut agg = Agg::complete();
        agg.add_file(&extract(
            pack,
            &mut parser,
            Path::new("lonely/one.py"),
            &VIOLATOR.replace("NAME", "mess"),
        ));
        assert!(run(&agg, 5).contains("no directory carries"));
    }
}
