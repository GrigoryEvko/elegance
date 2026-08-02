//! What the module graph cannot see: files that keep changing together,
//! and files only one person has ever touched.
//!
//! The import graph shows dependencies someone declared. History shows
//! the ones nobody did. Two files with no edge between them that change
//! together in four commits out of five share a decision the code does
//! not express — the coupling is real, it is just undeclared, and it is
//! what makes a "small" change touch six packages.
//!
//! Cross-directory pairs only. Files in one directory changing together
//! is what a directory IS; the finding is coupling that crosses a
//! boundary someone drew on purpose.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Write;
use std::path::Path;

use crate::git;

/// Pairs and owners shown.
const SHOW: usize = 10;

/// A commit touching more than this is a rename sweep, a reformat or a
/// dependency bump — mechanical breadth, not a shared decision. It also
/// bounds the pair count, which is quadratic in files per commit.
const MAX_COMMIT_BREADTH: usize = 30;

/// Below this many shared commits a pair is coincidence.
const MIN_SHARED: u32 = 3;

/// How much of a pair's changes must be shared before it is coupling.
const MIN_STRENGTH: f64 = 0.6;

/// A file needs this much history before "one author" means anything.
const MIN_COMMITS_FOR_OWNERSHIP: u32 = 5;

pub fn run(root: &Path, show: usize) -> Result<i32, Box<dyn Error>> {
    let commits = git::log(root)?;
    if commits.is_empty() {
        println!("coupling: no git history found in {}", root.display());
        return Ok(0);
    }
    if git::is_shallow(root) {
        println!(
            "coupling: this is a SHALLOW clone — nothing changed together here.\n\
             Fetch full history (`git fetch --unshallow`) for a real reading.\n"
        );
    }
    let show = show.max(SHOW);
    print!("{}", render_pairs(&commits, show));
    print!("{}", render_owners(&commits, show));
    Ok(0)
}

/// Shared commits per unordered cross-directory pair.
type Pairs<'a> = HashMap<(&'a str, &'a str), u32>;
/// Each file's own commit count — the denominator for strength.
type Totals<'a> = HashMap<&'a str, u32>;

/// Both, from one pass over the log.
fn tally(commits: &[git::Commit]) -> (Pairs<'_>, Totals<'_>) {
    let mut pairs: HashMap<(&str, &str), u32> = HashMap::new();
    let mut totals: HashMap<&str, u32> = HashMap::new();
    for c in commits {
        let mut files: Vec<&str> = c.files.iter().map(|f| &**f).collect();
        files.sort_unstable();
        files.dedup();
        for f in &files {
            *totals.entry(f).or_insert(0) += 1;
        }
        if files.len() <= MAX_COMMIT_BREADTH {
            add_pairs(&files, &mut pairs);
        }
    }
    (pairs, totals)
}

/// Every unordered cross-directory pair in one commit. Same-directory
/// pairs are skipped: files in one directory changing together is what a
/// directory IS.
fn add_pairs<'a>(files: &[&'a str], pairs: &mut Pairs<'a>) {
    for (i, a) in files.iter().enumerate() {
        for b in &files[i + 1..] {
            if dir_of(a) != dir_of(b) {
                *pairs.entry((a, b)).or_insert(0) += 1;
            }
        }
    }
}

fn render_pairs(commits: &[git::Commit], show: usize) -> String {
    let (pairs, totals) = tally(commits);
    let strength = |(a, b): (&str, &str), shared: u32| {
        let floor = totals[a].min(totals[b]).max(1);
        shared as f64 / floor as f64
    };
    let mut coupled: Vec<((&str, &str), u32, f64)> = pairs
        .into_iter()
        .filter(|(_, n)| *n >= MIN_SHARED)
        .map(|(k, n)| (k, n, strength(k, n)))
        .filter(|(_, _, s)| *s >= MIN_STRENGTH)
        .collect();
    if coupled.is_empty() {
        return "coupling — no cross-directory pair changes together often enough\n".to_string();
    }
    coupled.sort_by(|x, y| {
        y.2.total_cmp(&x.2)
            .then_with(|| y.1.cmp(&x.1))
            .then_with(|| x.0.cmp(&y.0))
    });
    let mut out = String::new();
    let _ = writeln!(
        out,
        "coupling — files that change together across a directory boundary\n\n\
         {:>8} {:>7}  files",
        "strength", "shared"
    );
    for ((a, b), shared, s) in coupled.iter().take(show) {
        let _ = writeln!(out, "{:>7.0}% {shared:>7}  {a}\n{:>17}{b}", s * 100.0, "");
    }
    if coupled.len() > show {
        let _ = writeln!(out, "  ... and {} more pairs", coupled.len() - show);
    }
    let _ = writeln!(
        out,
        "\nstrength is shared commits over the rarer file's own commits; a\n\
         commit touching over {MAX_COMMIT_BREADTH} files is a sweep, not a shared decision,\n\
         and is excluded."
    );
    out
}

/// Files one person has effectively written alone. Not a fault — someone
/// has to write it first — but a fact worth knowing before they leave,
/// and the reason to route the next change there through review.
fn render_owners(commits: &[git::Commit], show: usize) -> String {
    let mut by_file: HashMap<&str, HashMap<&str, u32>> = HashMap::new();
    for c in commits {
        for f in &c.files {
            let authors = by_file.entry(f).or_default();
            let n = authors.entry(&*c.author).or_insert(0);
            *n += 1;
        }
    }
    let mut owned: Vec<(&str, &str, u32, f64)> = by_file
        .into_iter()
        .filter_map(|(file, authors)| {
            let total: u32 = authors.values().sum();
            if total < MIN_COMMITS_FOR_OWNERSHIP {
                return None;
            }
            let (who, n) = dominant(authors)?;
            let share = n as f64 / total as f64;
            (share == 1.0).then_some((file, who, total, share))
        })
        .collect();
    if owned.is_empty() {
        return String::new();
    }
    owned.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nsole authorship — every commit by one person (bus factor 1)\n\n\
         {:>8}  {:<20} file",
        "commits", "author"
    );
    for (file, who, total, _) in owned.iter().take(show) {
        let _ = writeln!(out, "{total:>8}  {who:<20} {file}");
    }
    if owned.len() > show {
        let _ = writeln!(out, "  ... and {} more files", owned.len() - show);
    }
    out
}

/// The author of the most commits, name tie-broken for determinism.
fn dominant(authors: HashMap<&str, u32>) -> Option<(&str, u32)> {
    authors
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(author: &str, files: &[&str]) -> git::Commit {
        git::Commit {
            at: 1_700_000_000,
            author: author.into(),
            files: files.iter().map(|f| (*f).into()).collect(),
        }
    }

    #[test]
    fn coupling_crosses_directories_and_needs_repetition() {
        let together = || commit("Ada", &["api/handler.rs", "db/schema.rs"]);
        let siblings = || commit("Ada", &["api/handler.rs", "api/router.rs"]);
        let commits = vec![together(), together(), together(), siblings(), siblings()];
        let out = render_pairs(&commits, 10);
        assert!(out.contains("api/handler.rs"), "{out}");
        assert!(
            out.contains("db/schema.rs"),
            "cross-directory pair reported"
        );
        assert!(
            !out.contains("api/router.rs"),
            "files in one directory changing together is what a directory IS:\n{out}"
        );

        // Two shared commits is coincidence, not coupling.
        let thin = vec![together(), together()];
        assert!(render_pairs(&thin, 10).contains("no cross-directory pair"));
    }

    #[test]
    fn a_sweep_is_not_a_shared_decision() {
        // A reformat touching everything must not couple everything to
        // everything — that is 435 pairs from one commit.
        let mut wide: Vec<Box<str>> = Vec::new();
        for i in 0..31 {
            let path = format!("p{i}/f.rs");
            wide.push(path.into());
        }
        let sweep = git::Commit {
            at: 1,
            author: "bot".into(),
            files: wide,
        };
        let commits = vec![sweep, commit("Ada", &["a/x.rs", "b/y.rs"])];
        let (pairs, _) = tally(&commits);
        assert_eq!(pairs.len(), 1, "only the narrow commit contributes pairs");
    }

    #[test]
    fn sole_authorship_needs_enough_history_to_mean_anything() {
        let mine = |f: &str| commit("Ada", &[f]);
        let mut commits: Vec<git::Commit> = (0..5).map(|_| mine("solo/only.rs")).collect();
        commits.push(commit("Grace", &["shared/both.rs"]));
        commits.extend((0..5).map(|_| mine("shared/both.rs")));
        let out = render_owners(&commits, 10);
        assert!(out.contains("solo/only.rs"), "{out}");
        assert!(!out.contains("shared/both.rs"), "two authors is not sole");

        // Four commits by one person says nothing yet.
        let young: Vec<git::Commit> = (0..4).map(|_| mine("new/file.rs")).collect();
        assert!(render_owners(&young, 10).is_empty());
    }
}
