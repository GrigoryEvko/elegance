//! Baseline/ratchet: the deployment model that works at any scale. Nobody
//! fixes 4000 findings; they stop the count growing. `write` records today's
//! violations as the ledger; `check` fails only on violations that are not
//! in it — new sludge is blocked, old sludge is tolerated until touched.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry::{Occupied, Vacant};
use std::error::Error;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::metrics::METRICS;
use crate::report::Agg;

/// Only ladder rungs 0-2 (violations) gate by default; suspicions and
/// reports never block a build. Teams tighten or loosen with --fail-on.
pub const GATED_MAX_RUNG: u8 = 2;

/// Shown on failure; the Infer lesson — overwhelmed engineers fix nothing.
const SHOW: usize = 10;

const BASELINE: &str = ".elegance/baseline.json";

/// Schema 2 records each violation's magnitude. Schema 1 did not, which
/// made the ratchet suppress by identity alone: a baselined unit could
/// degrade without limit and still check clean.
const SCHEMA_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct BaselineFile {
    schema_version: u32,
    /// The severity this ledger was recorded at. A baseline written at a
    /// looser severity does not contain the rungs a stricter check asks
    /// about, so every one of them would read as new.
    #[serde(default = "default_rung")]
    gated_max_rung: u8,
    violations: Vec<Entry>,
}

fn default_rung() -> u8 {
    GATED_MAX_RUNG
}

#[derive(Serialize, Deserialize, PartialEq, PartialOrd)]
struct Entry {
    metric: String,
    path: String,
    /// Scope-qualified unit name; lines are deliberately not identity —
    /// they shift with every unrelated edit.
    unit: String,
    /// The tolerated magnitude. Absent in schema 1 baselines, which are
    /// read as "any magnitude tolerated" so an upgrade never fails a
    /// build spuriously — rewrite the baseline to close that hole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<f32>,
}

/// Identity of a violation: what it measures, where, in which unit.
type Key = (String, String, String);

impl Entry {
    fn key(&self) -> Key {
        (self.metric.clone(), self.path.clone(), self.unit.clone())
    }
}

/// Of two tolerated magnitudes for one moved (metric, unit), the one
/// that tolerates MORE — several same-named units may have left one
/// path, and the ratchet errs toward tolerance on renames. `None`
/// (schema 1, unrecorded) is the most permissive of all.
fn more_permissive(metric: &str, a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (None, _) | (_, None) => None,
        (Some(x), Some(y)) => Some(if worsened(metric, y, x) { x } else { y }),
    }
}

/// Has `now` grown worse than the baselined `then`? Ceilings degrade
/// upward and floors downward; two-sided bands (comment ratio) are not
/// gated today, so their movement is tolerated either way.
fn worsened(metric: &str, then: f32, now: f32) -> bool {
    let Some(def) = METRICS.iter().find(|d| d.name == metric) else {
        return false; // metric retired since the baseline was written
    };
    match (def.lo, def.hi) {
        (None, Some(_)) => now > then,
        (Some(_), None) => now < then,
        _ => false,
    }
}

/// Returns process exit code: 0 clean, 1 new violations (check mode only).
pub fn run(mode: &str, agg: &Agg, root: &Path, max_rung: u8) -> Result<i32, Box<dyn Error>> {
    let path = root.join(BASELINE);
    match mode {
        "write" => write_ledger(agg, &path, max_rung),
        "check" => check_ledger(agg, &path, max_rung),
        other => Err(format!("--baseline takes write|check, got {other:?}").into()),
    }
}

fn write_ledger(agg: &Agg, path: &Path, max_rung: u8) -> Result<i32, Box<dyn Error>> {
    // Worst magnitude per identity: the tolerated ceiling.
    let mut worst: BTreeMap<Key, Entry> = BTreeMap::new();
    for e in gated(agg, max_rung) {
        match worst.entry(e.key()) {
            Occupied(mut slot) => {
                let kept = slot.get_mut();
                if worsened(&e.metric, kept.value.unwrap_or(0.0), e.value.unwrap_or(0.0)) {
                    *kept = e;
                }
            }
            Vacant(slot) => {
                slot.insert(e);
            }
        }
    }
    let file = BaselineFile {
        schema_version: SCHEMA_VERSION,
        gated_max_rung: max_rung,
        violations: worst.into_values().collect(),
    };
    std::fs::create_dir_all(path.parent().expect("baseline path has parent"))?;
    let mut text = serde_json::to_string_pretty(&file)?;
    text.push('\n');
    std::fs::write(path, text)?;
    println!(
        "baseline: {} violations recorded in {}",
        file.violations.len(),
        path.display()
    );
    Ok(0)
}

fn check_ledger(agg: &Agg, path: &Path, max_rung: u8) -> Result<i32, Box<dyn Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: {e} (run --baseline write first)", path.display()))?;
    let file: BaselineFile = serde_json::from_str(&text)?;
    if file.gated_max_rung < max_rung {
        return Err(format!(
            "{} was written at --fail-on {} but this check asks for {max_rung}: every \
             rung {}-{max_rung} violation would read as new. Rewrite it with \
             `--baseline write --fail-on {max_rung}`.",
            path.display(),
            file.gated_max_rung,
            file.gated_max_rung + 1,
        )
        .into());
    }
    let (known, moved) = split_ledger(file.violations, &agg.scanned_paths());
    let fresh: Vec<Entry> = gated(agg, max_rung)
        .filter(|e| is_fresh(e, &known, &moved))
        .collect();
    if fresh.is_empty() {
        println!("ratchet: clean — no new or worsened violations");
        return Ok(0);
    }
    println!(
        "ratchet: {} NEW or WORSENED violations (rungs 0-{max_rung}):",
        fresh.len()
    );
    for e in fresh.iter().take(SHOW) {
        let was = known
            .get(&e.key())
            .and_then(|v| *v)
            .map_or(String::new(), |v| format!(" (was {v:.0})"));
        println!(
            "  {:<12} {:>6}{was}  {}  {}",
            e.metric,
            e.value.map_or(String::new(), |v| format!("{v:.0}")),
            e.path,
            e.unit
        );
    }
    if fresh.len() > SHOW {
        println!("  ... and {} more", fresh.len() - SHOW);
    }
    Ok(1)
}

/// The floating-tolerance key of a moved entry: what and where-in-name,
/// but no longer which file.
type MovedKey = (String, String);

/// The ledger split for checking: exact identities, plus rename
/// tolerance — an entry whose recorded path is absent from THIS scan
/// may have moved, so its (metric, unit) becomes a floating tolerance
/// at the most permissive magnitude any such entry recorded.
fn split_ledger(
    entries: Vec<Entry>,
    scanned: &std::collections::HashSet<String>,
) -> (BTreeMap<Key, Option<f32>>, BTreeMap<MovedKey, Option<f32>>) {
    let mut known: BTreeMap<Key, Option<f32>> = BTreeMap::new();
    let mut moved: BTreeMap<MovedKey, Option<f32>> = BTreeMap::new();
    for e in entries {
        if !scanned.contains(&e.path) {
            let slot = moved
                .entry((e.metric.clone(), e.unit.clone()))
                .or_insert(e.value);
            *slot = more_permissive(&e.metric, *slot, e.value);
        }
        known.insert(e.key(), e.value);
    }
    (known, moved)
}

/// A violation the ledger does not already tolerate: unseen in place,
/// unseen among moved candidates, or grown past its recorded magnitude.
fn is_fresh(
    e: &Entry,
    known: &BTreeMap<Key, Option<f32>>,
    moved: &BTreeMap<MovedKey, Option<f32>>,
) -> bool {
    let grew_past = |then: &Option<f32>| match then {
        None => false, // schema 1: magnitude unrecorded, any tolerated
        Some(then) => worsened(&e.metric, *then, e.value.unwrap_or(*then)),
    };
    match known.get(&e.key()) {
        Some(then) => grew_past(then),
        None => match moved.get(&(e.metric.clone(), e.unit.clone())) {
            Some(then) => grew_past(then),
            None => true,
        },
    }
}

fn gated(agg: &Agg, max_rung: u8) -> impl Iterator<Item = Entry> + '_ {
    agg.violations().filter_map(move |v| {
        (METRICS[v.metric].rung <= max_rung).then(|| Entry {
            metric: METRICS[v.metric].name.to_string(),
            path: v.path.to_string(),
            unit: v.unit.to_string(),
            value: Some(v.value),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::extract;
    use crate::lang::Lang;

    fn agg_of(sources: &[(&str, &str)]) -> Agg {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let mut agg = Agg::complete();
        for (path, src) in sources {
            agg.add_file(&extract(pack, &mut parser, Path::new(path), src));
        }
        agg
    }

    const VIOLATOR: &str = "def NAME(xs):\n    t = 0\n    for x in xs:\n        if x > 0:\n            if x > 1:\n                if x > 2:\n                    if x > 3:\n                        if x > 4:\n                            t += x\n    return t\n";

    /// Builds a unit whose every gated metric scales with `branches`.
    fn grower(branches: usize) -> String {
        let mut src = String::from("def grow(x):\n");
        for i in 0..branches {
            let n = 1000 + i;
            src.push_str(&format!("    if x == {n}:\n        return {n}\n"));
        }
        src.push_str("    return 0\n");
        src
    }

    #[test]
    fn ratchet_blocks_a_baselined_unit_that_gets_worse() {
        // The whole promise of the ratchet is "new sludge blocked, old
        // sludge tolerated until touched". Suppressing by identity alone
        // let a baselined unit degrade without limit and still pass.
        let dir = std::env::temp_dir().join(format!("elegance-worse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let small = agg_of(&[("a.py", &grower(60))]);
        assert_eq!(run("write", &small, &dir, GATED_MAX_RUNG).unwrap(), 0);
        assert_eq!(
            run("check", &small, &dir, GATED_MAX_RUNG).unwrap(),
            0,
            "unchanged is clean"
        );

        // Same file, same unit name, ten times the complexity.
        let huge = agg_of(&[("a.py", &grower(600))]);
        assert_eq!(
            run("check", &huge, &dir, GATED_MAX_RUNG).unwrap(),
            1,
            "a 10x degradation of a baselined unit must fail the gate"
        );
        // Improvement is always welcome.
        let tiny = agg_of(&[("a.py", &grower(30))]);
        assert_eq!(
            run("check", &tiny, &dir, GATED_MAX_RUNG).unwrap(),
            0,
            "improvement passes"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fail_on_chooses_which_rungs_may_block() {
        // A team starts permissive and tightens; the ledger records only
        // what the chosen severity gates.
        let dir = std::env::temp_dir().join(format!("elegance-failon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let agg = agg_of(&[("a.py", &grower(60))]);
        // Rung 0 only (token hygiene): magic numbers gate, complexity does not.
        assert_eq!(run("write", &agg, &dir, 0).unwrap(), 0);
        let lenient = std::fs::read_to_string(dir.join(BASELINE)).unwrap();
        assert!(lenient.contains("magic numbers"));
        assert!(
            !lenient.contains("cognitive"),
            "rung 2 not gated at --fail-on 0"
        );

        // Checking stricter than the ledger was written is refused, not
        // silently reported as a wall of new violations.
        let err = run("check", &agg, &dir, GATED_MAX_RUNG)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--fail-on 0"), "{err}");
        assert!(err.contains("Rewrite it"), "{err}");
        // Rewriting at the stricter severity makes it valid again.
        assert_eq!(run("write", &agg, &dir, GATED_MAX_RUNG).unwrap(), 0);
        assert_eq!(run("check", &agg, &dir, GATED_MAX_RUNG).unwrap(), 0);
        // A ledger written STRICTER than the check is a superset: fine.
        assert_eq!(run("check", &agg, &dir, 0).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_does_not_manufacture_new_violations() {
        // Identity is (metric, path, unit), so renaming a file with 30
        // baselined violations used to fail CI with 30 "new" ones. An
        // entry whose path left the scan becomes a floating tolerance
        // for its (metric, unit) — and worsening hides behind a rename
        // no better than it does in place.
        let dir = std::env::temp_dir().join(format!("elegance-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let before = agg_of(&[("old_name.py", &grower(60))]);
        assert_eq!(run("write", &before, &dir, GATED_MAX_RUNG).unwrap(), 0);

        let renamed = agg_of(&[("new_name.py", &grower(60))]);
        assert_eq!(
            run("check", &renamed, &dir, GATED_MAX_RUNG).unwrap(),
            0,
            "a pure rename is not new sludge"
        );
        let worse = agg_of(&[("new_name.py", &grower(600))]);
        assert_eq!(
            run("check", &worse, &dir, GATED_MAX_RUNG).unwrap(),
            1,
            "worsening behind a rename still fails"
        );
        let grown = agg_of(&[
            ("old_name.py", &grower(60)),
            ("other.py", &VIOLATOR.replace("NAME", "fresh_mess")),
        ]);
        assert_eq!(
            run("check", &grown, &dir, GATED_MAX_RUNG).unwrap(),
            1,
            "a genuinely new violator still fails"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_1_baselines_stay_tolerated_after_upgrade() {
        // Reading an old baseline must never fail a build spuriously:
        // no recorded magnitude means any magnitude is tolerated.
        let dir = std::env::temp_dir().join(format!("elegance-schema1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".elegance")).unwrap();
        std::fs::write(
            dir.join(BASELINE),
            r#"{"schema_version":1,"violations":[
                 {"metric":"cognitive","path":"a.py","unit":"grow"},
                 {"metric":"cyclomatic","path":"a.py","unit":"grow"},
                 {"metric":"length","path":"a.py","unit":"grow"},
                 {"metric":"magic numbers","path":"a.py","unit":"grow"}]}"#,
        )
        .unwrap();
        let huge = agg_of(&[("a.py", &grower(600))]);
        assert_eq!(run("check", &huge, &dir, GATED_MAX_RUNG).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ratchet_blocks_only_new_violations() {
        let dir = std::env::temp_dir().join(format!("elegance-ratchet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let old = agg_of(&[("a.py", &VIOLATOR.replace("NAME", "old_mess"))]);
        assert_eq!(run("write", &old, &dir, GATED_MAX_RUNG).unwrap(), 0);
        // Unchanged codebase: clean.
        assert_eq!(run("check", &old, &dir, GATED_MAX_RUNG).unwrap(), 0);
        // A second violator appears: exit 1.
        let grown = agg_of(&[
            ("a.py", &VIOLATOR.replace("NAME", "old_mess")),
            ("b.py", &VIOLATOR.replace("NAME", "new_mess")),
        ]);
        assert_eq!(run("check", &grown, &dir, GATED_MAX_RUNG).unwrap(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
