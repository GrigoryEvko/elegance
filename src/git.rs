//! One walk of the history, shared by everything that reads it.
//!
//! Hotspots want per-file churn; coupling wants which files moved
//! together and who moved them. Both are the same `git log`, so it is
//! parsed once here rather than twice badly.

use std::error::Error;
use std::path::Path;
use std::process::Command;

/// Seconds in a day, for turning commit timestamps into ages.
pub const DAY: f64 = 86_400.0;

pub struct Commit {
    pub at: u64,
    pub author: Box<str>,
    /// Paths this commit touched, as git reports them (repo-relative).
    pub files: Vec<Box<str>>,
}

/// Every non-merge commit, newest first. A repository without history is
/// an empty list, not an error — most modes are useful without one.
pub fn log(root: &Path) -> Result<Vec<Commit>, Box<dyn Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            "--no-merges",
            "--numstat",
            "--format=%H%x09%at%x09%an",
        ])
        .output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits: Vec<Commit> = Vec::new();
    for line in text.lines() {
        match header(line) {
            Some((at, author)) => commits.push(Commit {
                at,
                author: author.into(),
                files: Vec::new(),
            }),
            None => {
                if let (Some(path), Some(c)) = (numstat_path(line), commits.last_mut()) {
                    c.files.push(path.into());
                }
            }
        }
    }
    Ok(commits)
}

/// The newest timestamp in the log, so a stale checkout still ranks its
/// own history correctly.
pub fn newest(commits: &[Commit]) -> u64 {
    commits.iter().map(|c| c.at).max().unwrap_or(0)
}

/// A shallow clone has one commit, so every file's churn is 1.0 and
/// every co-change is unobservable. Callers say so rather than present a
/// flat column as a measurement.
pub fn is_shallow(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-shallow-repository"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// `<sha>\t<unix time>\t<author>` — the format we asked git for.
fn header(line: &str) -> Option<(u64, &str)> {
    let mut parts = line.split('\t');
    let sha = parts.next()?;
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let at = parts.next()?.parse().ok()?;
    Some((at, parts.next().unwrap_or("")))
}

/// `<added>\t<deleted>\t<path>`; binary files report `-` for the counts
/// but still name their file.
fn numstat_path(line: &str) -> Option<&str> {
    let mut parts = line.split('\t');
    let added = parts.next()?;
    parts.next()?;
    let path = parts.next()?;
    (!added.is_empty() && !path.is_empty()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
0123456789abcdef0123456789abcdef01234567\t1700000000\tAda
10\t2\tsrc/hot.rs
1\t0\tsrc/cold.rs
fedcba9876543210fedcba9876543210fedcba98\t1600000000\tGrace
5\t5\tsrc/cold.rs
-\t-\tassets/logo.png
";

    /// Reproduces the walk `log` performs over already-fetched output.
    fn parse(text: &str) -> Vec<Commit> {
        let mut commits: Vec<Commit> = Vec::new();
        for line in text.lines() {
            match header(line) {
                Some((at, author)) => commits.push(Commit {
                    at,
                    author: author.into(),
                    files: Vec::new(),
                }),
                None => {
                    if let (Some(p), Some(c)) = (numstat_path(line), commits.last_mut()) {
                        c.files.push(p.into());
                    }
                }
            }
        }
        commits
    }

    #[test]
    fn commits_carry_their_author_time_and_files() {
        let commits = parse(LOG);
        assert_eq!(commits.len(), 2);
        assert_eq!(&*commits[0].author, "Ada");
        assert_eq!(commits[0].at, 1_700_000_000);
        assert_eq!(
            commits[0].files,
            ["src/hot.rs".into(), "src/cold.rs".into()]
        );
        assert_eq!(
            commits[1].files,
            ["src/cold.rs".into(), "assets/logo.png".into()],
            "binary rows still name their file; only the counts are '-'"
        );
        assert_eq!(newest(&commits), 1_700_000_000);
    }
}
