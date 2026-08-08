//! Churn × complexity: which complexity is actually costing anyone.
//!
//! A 600-line monster nobody has touched in three years is a museum
//! piece; the same monster edited weekly is where the bugs and the
//! reading time go (Tornhill's hotspot analysis). Complexity alone
//! ranks the museum first, which is why static reports get archived.
//!
//! Recency-weighted: a commit from last month says more about where
//! work is happening than one from four years ago, so churn decays with
//! a half-life rather than counting every commit equally.

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

use crate::git;

/// Commits older than this contribute half as much per half-life.
const HALF_LIFE_DAYS: f64 = 180.0;

/// Hotspots shown; beyond a handful nobody re-plans their week.
const SHOW: usize = 12;

pub struct Hotspot {
    pub path: String,
    /// Recency-weighted commit count.
    pub churn: f64,
    pub commits: u32,
    pub authors: usize,
    /// The file's worst gated metric value, and which metric it was.
    pub complexity: f32,
    pub metric: &'static str,
}

/// Churn alone ranks generated files; complexity alone ranks museums.
/// Tornhill's analysis ranks by the product of the two.
pub fn score(h: &Hotspot) -> f64 {
    h.churn * h.complexity as f64
}

pub fn run(agg: &mut crate::report::Agg, root: &Path, show: usize) -> Result<i32, Box<dyn Error>> {
    let churn = churn_by_file(root)?;
    if churn.is_empty() {
        println!("hotspots: no git history found in {}", root.display());
        return Ok(0);
    }
    // A shallow clone has one commit, so every file's churn is 1.0 and
    // the ranking degenerates into plain complexity. Say so rather than
    // presenting a flat column as a measurement.
    if git::is_shallow(root) {
        println!(
            "hotspots: this is a SHALLOW clone — churn is meaningless here.\n\
             Fetch full history (`git fetch --unshallow`) for a real ranking.\n"
        );
    }
    let mut spots = join(agg, &churn, root);
    spots.sort_by(worst_first);
    spots.truncate(show.max(SHOW));
    print!("{}", render(&spots));
    Ok(0)
}

fn worst_first(a: &Hotspot, b: &Hotspot) -> std::cmp::Ordering {
    score(b)
        .total_cmp(&score(a))
        .then_with(|| a.path.cmp(&b.path))
}

fn render(spots: &[Hotspot]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    if spots.is_empty() {
        return "hotspots: no file is both changing and complex\n".to_string();
    }
    let _ = writeln!(
        out,
        "hotspots — complexity that is actually being paid for\n\n\
         {:<9} {:>7} {:>7} {:>7}  {:<14} file",
        "score", "churn", "commits", "authors", "worst metric"
    );
    for h in spots {
        let _ = writeln!(
            out,
            "{:<9.0} {:>7.1} {:>7} {:>7}  {:<14} {}",
            score(h),
            h.churn,
            h.commits,
            h.authors,
            format!("{} {:.0}", h.metric, h.complexity),
            h.path,
        );
    }
    let _ = writeln!(
        out,
        "\nchurn is recency-weighted (half-life {HALF_LIFE_DAYS:.0} days): a file edited last\n\
         month outranks one edited equally often four years ago."
    );
    out
}

struct Churn {
    weighted: f64,
    commits: u32,
    authors: std::collections::HashSet<String>,
}

/// Every file's commit history, weighted by recency.
fn churn_by_file(root: &Path) -> Result<HashMap<String, Churn>, Box<dyn Error>> {
    let commits = git::log(root)?;
    let newest = git::newest(&commits);
    let mut by_file: HashMap<String, Churn> = HashMap::new();
    for c in &commits {
        // Age relative to the newest commit, so a stale checkout still
        // ranks its own history correctly.
        let age_days = (newest.saturating_sub(c.at)) as f64 / git::DAY;
        let weight = 0.5_f64.powf(age_days / HALF_LIFE_DAYS);
        for path in &c.files {
            let entry = by_file.entry(path.to_string()).or_insert_with(|| Churn {
                weighted: 0.0,
                commits: 0,
                authors: std::collections::HashSet::new(),
            });
            entry.weighted += weight;
            entry.commits += 1;
            entry.authors.insert(c.author.to_string());
        }
    }
    Ok(by_file)
}

/// Join churn against the worst gated measurement per file. Files with
/// no measured complexity (deleted, unsupported, generated) drop out:
/// churn alone is not a finding.
fn join(agg: &crate::report::Agg, churn: &HashMap<String, Churn>, root: &Path) -> Vec<Hotspot> {
    let mut worst: HashMap<String, (f32, &'static str)> = HashMap::new();
    for v in agg.violations() {
        if crate::metrics::METRICS[v.metric].rung > 2 {
            continue;
        }
        let relative = v
            .path
            .trim_start_matches("./")
            .trim_start_matches(&format!("{}/", root.display()))
            .to_string();
        let slot = worst.entry(relative).or_insert((0.0, ""));
        if v.value > slot.0 {
            *slot = (v.value, crate::metrics::METRICS[v.metric].name);
        }
    }
    worst
        .into_iter()
        .filter_map(|(path, (complexity, metric))| {
            let c = churn.get(&path)?;
            Some(Hotspot {
                path,
                churn: c.weighted,
                commits: c.commits,
                authors: c.authors.len(),
                complexity,
                metric,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_product_ranks_changing_complexity_over_stable_complexity() {
        let spot = |path: &str, churn: f64, complexity: f32| Hotspot {
            path: path.to_string(),
            churn,
            commits: 1,
            authors: 1,
            complexity,
            metric: "cognitive",
        };
        // A museum piece: enormous, untouched. A hotspot: smaller, hot.
        let museum = spot("legacy.rs", 0.2, 400.0);
        let hot = spot("parser.rs", 9.0, 60.0);
        assert!(
            score(&hot) > score(&museum),
            "churn x complexity must rank the file being paid for first"
        );
    }
}
