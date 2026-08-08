//! Quality is a derivative. The baseline answers "did this change make
//! things worse"; "are we getting better" needs a series instead, and
//! Lehman's second law says that without deliberate work the answer is
//! no. This records one row per run and renders the deltas between them.
//!
//! Recording is explicit, never automatic: a trend line nobody chose to
//! keep is noise, and CI is the only place that should decide when a
//! measurement counts.

use std::error::Error;
use std::fmt::Write;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::metrics::METRICS;
use crate::report::Agg;

const HISTORY: &str = ".elegance/history.jsonl";

/// Rows are appended forever; unknown fields are tolerated on read so a
/// newer tool's history stays readable by an older one and vice versa.
#[derive(Serialize, Deserialize)]
struct Row {
    schema_version: u32,
    /// Commit the measurement describes, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    files: u32,
    lines: u64,
    units: u64,
    test_units: u64,
    gate_violations: u64,
    suspicions: u64,
    duplicated_pct: f64,
    /// Per-metric gated violation counts, so a regression can be traced
    /// to the metric that caused it rather than just a total.
    gated: Vec<(String, u64)>,
}

const ROW_SCHEMA: u32 = 1;

pub fn run(mode: &str, agg: &mut Agg, root: &Path, show: usize) -> Result<i32, Box<dyn Error>> {
    match mode {
        "record" => record(agg, root),
        "show" => show_trend(root, show),
        other => Err(format!("--history takes record|show, got {other:?}").into()),
    }
}

fn record(agg: &mut Agg, root: &Path) -> Result<i32, Box<dyn Error>> {
    let path = root.join(HISTORY);
    let verdict = crate::report::verdict(agg);
    let gated: Vec<(String, u64)> = METRICS
        .iter()
        .enumerate()
        .filter(|(m, def)| def.rung <= 2 && agg.violations_of(*m) > 0)
        .map(|(m, def)| (def.name.to_string(), agg.violations_of(m)))
        .collect();
    let row = Row {
        schema_version: ROW_SCHEMA,
        commit: git(root, &["rev-parse", "--short", "HEAD"]),
        branch: git(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        files: agg.files,
        lines: agg.lines,
        units: agg.units,
        test_units: agg.test_units,
        gate_violations: verdict.gates,
        suspicions: verdict.suspicions,
        duplicated_pct: crate::report::duplicated_pct(agg),
        gated,
    };
    std::fs::create_dir_all(path.parent().expect("history path has parent"))?;
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = writeln!(text, "{}", serde_json::to_string(&row)?);
    std::fs::write(&path, text)?;
    println!(
        "history: recorded {} gate violations, {} suspicions in {}",
        row.gate_violations,
        row.suspicions,
        path.display()
    );
    Ok(0)
}

fn show_trend(root: &Path, show: usize) -> Result<i32, Box<dyn Error>> {
    let path = root.join(HISTORY);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {e} (run --history record first)", path.display()))?;
    let mut rows: Vec<Row> = Vec::new();
    let mut skipped = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Row>(line) {
            Ok(row) => rows.push(row),
            // A corrupt line must not destroy the series around it.
            Err(_) => skipped += 1,
        }
    }
    if rows.is_empty() {
        println!("history: no readable rows in {}", path.display());
        return Ok(0);
    }
    let from = rows.len().saturating_sub(show);
    print!("{}", render(&rows[from..], skipped));
    Ok(0)
}

fn render(rows: &[Row], skipped: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<10} {:>8} {:>7} {:>9} {:>11} {:>7}",
        "commit", "lines", "units", "gates", "suspicions", "dup%"
    );
    let mut previous: Option<&Row> = None;
    for row in rows {
        let _ = writeln!(
            out,
            "{:<10} {:>8} {:>7} {:>9} {:>11} {:>6.1}%",
            row.commit.as_deref().unwrap_or("-"),
            row.lines,
            row.units,
            delta(previous.map(|p| p.gate_violations), row.gate_violations),
            delta(previous.map(|p| p.suspicions), row.suspicions),
            row.duplicated_pct,
        );
        previous = Some(row);
    }
    // Which metric moved is the actionable part; the totals only say
    // that something did.
    if let (Some(first), Some(last)) = (rows.first(), rows.last())
        && !std::ptr::eq(first, last)
    {
        let moved = metric_deltas(first, last);
        if !moved.is_empty() {
            let _ = writeln!(
                out,
                "\nsince {}:",
                first.commit.as_deref().unwrap_or("start")
            );
            for (metric, change) in moved {
                let _ = writeln!(out, "  {metric:<15} {change:+}");
            }
        }
    }
    if skipped > 0 {
        let _ = writeln!(out, "\n{skipped} unreadable rows skipped");
    }
    out
}

/// Counts with their change from the previous run, so a row reads as a
/// position and a direction at once.
fn delta(previous: Option<u64>, now: u64) -> String {
    match previous {
        Some(before) if before != now => {
            format!("{now} ({:+})", now as i64 - before as i64)
        }
        _ => now.to_string(),
    }
}

fn metric_deltas(first: &Row, last: &Row) -> Vec<(String, i64)> {
    let mut names: Vec<&str> = Vec::new();
    for (metric, _) in first.gated.iter().chain(&last.gated) {
        names.push(metric);
    }
    names.sort_unstable();
    names.dedup();

    let mut out: Vec<(String, i64)> = Vec::new();
    for name in names {
        let change = count_of(last, name) - count_of(first, name);
        if change != 0 {
            out.push((name.to_string(), change));
        }
    }
    out.sort_by(biggest_move_first);
    out
}

/// The largest movement first, in either direction: an improvement of
/// twenty matters as much as a regression of twenty.
fn biggest_move_first(a: &(String, i64), b: &(String, i64)) -> std::cmp::Ordering {
    let magnitude = b.1.abs().cmp(&a.1.abs());
    magnitude.then_with(|| a.0.cmp(&b.0))
}

fn count_of(row: &Row, name: &str) -> i64 {
    row.gated
        .iter()
        .find(|(metric, _)| metric == name)
        .map_or(0, |(_, n)| *n as i64)
}

/// Repository identity when there is a repository; history is useful
/// without one, so failure is silent.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(commit: &str, gates: u64, cognitive: u64) -> Row {
        Row {
            schema_version: ROW_SCHEMA,
            commit: Some(commit.to_string()),
            branch: None,
            files: 10,
            lines: 1000,
            units: 100,
            test_units: 20,
            gate_violations: gates,
            suspicions: 5,
            duplicated_pct: 1.5,
            gated: vec![("cognitive".to_string(), cognitive)],
        }
    }

    #[test]
    fn trend_shows_direction_and_names_the_metric_that_moved() {
        let rows = [row("aaa", 10, 6), row("bbb", 4, 2)];
        let out = render(&rows, 0);
        assert!(out.contains("10 "), "first row is a position, not a delta");
        assert!(out.contains("4 (-6)"), "second row carries its direction");
        assert!(out.contains("cognitive"), "the metric that moved is named");
        assert!(out.contains("-4"), "with its own change");
    }

    #[test]
    fn a_corrupt_line_does_not_destroy_the_series() {
        let dir = std::env::temp_dir().join(format!("elegance-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".elegance")).unwrap();
        let good = serde_json::to_string(&row("aaa", 3, 1)).unwrap();
        std::fs::write(
            dir.join(HISTORY),
            format!("{good}\nnot json at all\n{good}\n"),
        )
        .unwrap();
        assert_eq!(show_trend(&dir, 10).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
