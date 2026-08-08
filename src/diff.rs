//! Diff-time findings: violations only on units the change touched,
//! capped and ranked. Engineers act on findings about code they just
//! wrote, at the moment they wrote it. Batch reports get archived.
//! Diff findings get fixed.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::Path;
use std::process::Command;

use crate::metrics::{LangBudgets, METRICS};
use crate::report::Agg;
use crate::{config, facts, metrics};

/// The cap on findings shown at once. Past it, a list stops being
/// triaged.
const SHOW: usize = 10;

struct Finding {
    rung: u8,
    excess: f32,
    metric: usize,
    /// Budget label of the file's language (budgets vary per language).
    budget: String,
    path: String,
    line: u32,
    unit: String,
    value: f32,
    /// Language whose distribution positioned this finding.
    lang: &'static str,
    /// Position in this repository's own SAME-LANGUAGE distribution,
    /// when there are enough measurements for a percentile to mean
    /// anything. One mixed distribution would position a C function
    /// against a Python-heavy curve and label it "of repo".
    repo_pctl: Option<u8>,
    /// The named refactoring this finding calls for, where the facts
    /// justify naming one.
    suggest: Option<String>,
}

/// Returns the process exit code: 0 when the change adds no violation
/// at or below `max_rung`.
pub fn run(
    reference: &str,
    root: &Path,
    lang_budgets: LangBudgets,
    max_rung: u8,
) -> Result<i32, Box<dyn Error>> {
    let changed = changed_files(root, reference)?;
    let mut files_seen = 0u32;
    // A budget is a ceiling; a position says whether this unit reads like
    // the codebase or like its ugliest tail. That needs the whole repo's
    // SAME-LANGUAGE distribution, so this is a second scan, cached by
    // content because a pre-commit hook pays it on every commit.
    let cfg = config::Config::load(root)?;
    let all = crate::collect_files(std::slice::from_ref(&root.to_path_buf()), &cfg);
    let mut review = Review {
        budgets: lang_budgets,
        repo: distributions(&all, lang_budgets, root),
        findings: Vec::new(),
    };

    for (rel, ranges) in &changed {
        if review.collect(&root.join(rel), ranges) {
            files_seen += 1;
        }
    }
    let mut findings = review.findings;

    if findings.is_empty() {
        println!("diff vs {reference} — {files_seen} source files changed, no findings");
        return Ok(0);
    }
    // Findings are always SHOWN; only rungs at or below the threshold
    // decide the exit code, so a team can tighten gradually.
    let blocking = findings.iter().filter(|f| f.rung <= max_rung).count();
    findings.sort_by(|a, b| {
        a.rung
            .cmp(&b.rung)
            .then_with(|| b.excess.total_cmp(&a.excess))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    println!(
        "diff vs {reference} — {} findings on changed code:",
        findings.len()
    );
    for f in findings.iter().take(SHOW) {
        let position = f
            .repo_pctl
            .map_or(String::new(), |p| format!(" p{p} of repo {}", f.lang));
        println!(
            "  [r{}] {:<12} {:>5} ({}{})  {}:{}  {}",
            f.rung,
            METRICS[f.metric].name,
            format!("{:.0}", f.value),
            f.budget,
            position,
            f.path,
            f.line,
            f.unit,
        );
        if let Some(remedy) = &f.suggest {
            println!("       -> {remedy}");
        }
    }
    if findings.len() > SHOW {
        println!("  ... and {} more", findings.len() - SHOW);
    }
    if blocking == 0 {
        println!("  none at rung <= {max_rung}: reported, not blocking");
        return Ok(0);
    }
    Ok(1)
}

/// Every metric value in the repository, for positioning: one
/// distribution PER LANGUAGE, because a repository's Python p90 says
/// nothing about how its C reads. Only the distributions are
/// populated. See cache.rs for why this deliberately cannot serve the
/// full report.
fn distributions(files: &[std::path::PathBuf], budgets: LangBudgets, root: &Path) -> Vec<Agg> {
    let mut cache = crate::cache::Cache::load(root);
    let mut per: Vec<Agg> = crate::lang::LANGS
        .iter()
        .map(|_| {
            Agg::configured(
                crate::config::Layers::flat(budgets),
                false,
                crate::report::Wants::NONE,
            )
        })
        .collect();
    for path in files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some((lang, text)) = crate::measurable(path, &source) else {
            continue;
        };
        let agg = &mut per[lang as usize];
        let content = crate::cache::content_hash(&source);
        if let Some(values) = cache.get(path, content) {
            agg.add_values(lang, &values);
            continue;
        }
        if config::is_generated(path, &source) {
            continue;
        }
        let f = facts::extract(
            lang.pack_for(path),
            &mut lang.pack().make_parser(),
            path,
            &text,
        );
        if f.low_confidence() {
            continue;
        }
        let mut values = Vec::new();
        metrics::for_each(&f, |m, value, _, _| values.push((m as u8, value)));
        agg.add_values(lang, &values);
        cache.insert(path, content, values);
    }
    cache.save(root);
    per
}

/// Diff-time measurement state: the budgets to judge by, the repository
/// distributions (one per language) to position against, and the
/// findings so far.
struct Review {
    budgets: LangBudgets,
    repo: Vec<crate::report::Agg>,
    findings: Vec<Finding>,
}

impl Review {
    /// Measure one changed file, keeping violations on units the change
    /// touched. Returns whether the file was source this tool reads.
    fn collect(&mut self, path: &Path, ranges: &LineRanges) -> bool {
        let Ok(source) = std::fs::read_to_string(path) else {
            return false;
        };
        if config::is_generated(path, &source) {
            return false;
        }
        let Some((lang, text)) = crate::measurable(path, &source) else {
            return false;
        };
        let f = facts::extract(
            lang.pack_for(path),
            &mut lang.pack().make_parser(),
            path,
            &text,
        );
        if f.low_confidence() {
            return true;
        }
        let touched: HashSet<(u32, &str)> = f
            .units
            .iter()
            .filter(|u| {
                !u.is_module
                    && ranges
                        .iter()
                        .any(|&(a, b)| a <= u.line + u.lines.saturating_sub(1) && u.line <= b)
            })
            .map(|u| (u.line, &*u.qualname))
            .collect();
        let budgets = *self.budgets.for_lang(lang);
        let by_line: HashMap<(u32, &str), &crate::facts::UnitFacts> = f
            .units
            .iter()
            .map(|u| ((u.line, &*u.qualname), u))
            .collect();
        let (repo, findings) = (&mut self.repo, &mut self.findings);
        metrics::for_each(&f, |m, value, line, name| {
            // File-level metrics would nag on every edit; units only here.
            if name.is_empty() || !budgets.violates(m, value) || !touched.contains(&(line, name)) {
                return;
            }
            let (lo, hi) = budgets.0[m];
            let excess = match (lo, hi) {
                (_, Some(h)) if value > h => value - h,
                (Some(l), _) if value < l => l - value,
                _ => 0.0,
            };
            findings.push(Finding {
                rung: METRICS[m].rung,
                excess,
                metric: m,
                budget: budgets.label(m),
                path: path.display().to_string(),
                line,
                unit: name.to_string(),
                value,
                lang: lang.name(),
                repo_pctl: repo[lang as usize].percentile(m, value),
                suggest: by_line
                    .get(&(line, name))
                    .and_then(|u| crate::report::suggest::for_metric(m, u)),
            });
        });
        true
    }
}

/// Inclusive 1-based line ranges on the new side of a diff.
type LineRanges = Vec<(u32, u32)>;

/// Changed files (post-image, tracked) with their new-side line
/// ranges. The reference reaches git VERBATIM, so `origin/main...HEAD`
/// works: git resolves three dots to the merge base, and a pull
/// request is judged on what it added rather than on everything main
/// merged meanwhile. `HEAD` with no dots compares against the working
/// tree, which is what a pre-commit hook wants.
fn changed_files(
    root: &Path,
    reference: &str,
) -> Result<Vec<(String, LineRanges)>, Box<dyn Error>> {
    let names = git(
        root,
        &["diff", "--name-only", "--diff-filter=ACMR", "-M", reference],
    )?;
    let mut out = Vec::new();
    for name in names.lines().filter(|l| !l.is_empty()) {
        let hunks = git(root, &["diff", "-U0", "-M", reference, "--", name])?;
        let ranges = parse_hunks(&hunks);
        if !ranges.is_empty() {
            out.push((name.to_string(), ranges));
        }
    }
    Ok(out)
}

/// New-side line ranges from `@@ -a,b +c,d @@` hunk headers.
fn parse_hunks(diff: &str) -> Vec<(u32, u32)> {
    diff.lines()
        .filter(|l| l.starts_with("@@"))
        .filter_map(|l| {
            let plus = l.split_whitespace().find(|t| t.starts_with('+'))?;
            let mut it = plus[1..].split(',');
            let start: u32 = it.next()?.parse().ok()?;
            let count: u32 = it.next().map_or(1, |c| c.parse().unwrap_or(1));
            // Lazily: a deletion-only hunk (+N,0) must not evaluate
            // `start + count - 1`, which would underflow.
            (count > 0).then(|| (start, start + count - 1))
        })
        .collect()
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::LangBudgets;

    #[test]
    fn a_three_dot_reference_reaches_git_verbatim() {
        // The merge-base form keeps a pull-request gate honest:
        // `origin/main` alone also reports everything main merged since
        // the branch point, which the author cannot fix here. Nothing
        // between the flag and git may rewrite the reference. This test
        // runs against this repository, where HEAD~1...HEAD is always a
        // valid range.
        let root = std::path::Path::new(".");
        if git(root, &["rev-parse", "HEAD~1"]).is_err() {
            return; // a shallow or fresh checkout has no history to span
        }
        let spanned = changed_files(root, "HEAD~1...HEAD").expect("three dots resolve");
        let direct = changed_files(root, "HEAD~1").expect("two dots resolve");
        // On a clean tree the two agree; the point is that BOTH parse,
        // so the three-dot form is not silently mangled into an error.
        assert!(
            spanned.len() <= direct.len() + 1,
            "three-dot range should not explode into unrelated files"
        );
    }

    #[test]
    fn hunk_headers_parse_to_new_side_ranges() {
        let diff = "@@ -1,2 +10,3 @@ fn x\n@@ -5 +20 @@\n@@ -7,2 +0,0 @@ deleted\n";
        assert_eq!(parse_hunks(diff), [(10, 12), (20, 20)]);
    }

    use crate::lang::Lang;

    #[test]
    fn positioning_is_per_language_never_mixed() {
        // A TS finding in a Python-heavy repo must not be positioned
        // against Python's curve under a label saying "of repo".
        let budgets = LangBudgets::defaults();
        let mut repo: Vec<crate::report::Agg> = crate::lang::LANGS
            .iter()
            .map(|_| {
                crate::report::Agg::configured(
                    crate::config::Layers::flat(budgets),
                    false,
                    crate::report::Wants::ALL,
                )
            })
            .collect();
        let flat: Vec<(u8, f32)> = (0..250).map(|_| (0u8, 0.0)).collect();
        let tall: Vec<(u8, f32)> = (0..250).map(|_| (0u8, 50.0)).collect();
        repo[Lang::Python as usize].add_values(Lang::Python, &flat);
        repo[Lang::TypeScript as usize].add_values(Lang::TypeScript, &tall);
        let value = 10.0;
        let py = repo[Lang::Python as usize].percentile(0, value);
        let ts = repo[Lang::TypeScript as usize].percentile(0, value);
        assert_eq!(py, Some(100), "above everything Python measured");
        assert_eq!(ts, Some(0), "below everything TypeScript measured");
    }

    const VIOLATOR: &str = "def NAME(xs):\n    t = 0\n    for x in xs:\n        if x > 0:\n            if x > 1:\n                if x > 2:\n                    if x > 3:\n                        if x > 4:\n                            t += x\n    return t\n";

    #[test]
    fn diff_mode_flags_only_units_the_change_touched() {
        let dir = std::env::temp_dir().join(format!("elegance-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(ok.status.success(), "git {args:?}");
        };
        sh(&["init", "-q"]);
        let old = VIOLATOR.replace("NAME", "old_mess");
        std::fs::write(dir.join("a.py"), &old).unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-qm", "base"]);

        // Unchanged tree: clean exit.
        assert_eq!(run("HEAD", &dir, LangBudgets::defaults(), 2).unwrap(), 0);

        // Append a fresh violator; the old one is untouched by the diff.
        let grown = format!("{old}\n{}", VIOLATOR.replace("NAME", "new_mess"));
        std::fs::write(dir.join("a.py"), grown).unwrap();
        assert_eq!(run("HEAD", &dir, LangBudgets::defaults(), 2).unwrap(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
