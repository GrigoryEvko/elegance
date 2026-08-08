//! The code you did not write.
//!
//! A dependency tree is where a supply-chain problem hides, and the
//! ordinary scan never looks: `node_modules`, `vendor` and
//! `site-packages` are pruned by design, because nobody refactors a
//! dependency and a report full of somebody else's cyclomatic
//! complexity is noise.
//!
//! This mode looks anyway, and reports only what MATTERS about code you
//! cannot change: credentials compiled into it, constructs where the
//! text stops predicting the run, type-checker suppressions, how much
//! of it there is, and logic it shares with your own tree, which is
//! either a vendored copy of your code or your code copied out of it.
//!
//! Every unit-shape metric is deliberately absent. A dependency's
//! cognitive complexity is not a finding, it is trivia.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::facts::FileFacts;
use crate::lang::Lang;

/// Directory names holding code somebody else wrote. `target` and
/// `dist` are absent on purpose: those are BUILD output of the tree you
/// own, and reading your own bundle as a dependency would double-count
/// every finding it already reported.
const DEP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "third_party",
    "venv",
    ".venv",
    "site-packages",
];

/// Packages shown before the list stops being read.
const SHOW: usize = 20;

/// Finding sites shown; the per-package counts carry the totals.
const SITES: usize = 12;

/// Shared mass below which two trees written by different people
/// coinciding is likelier than one copying the other. Higher than the
/// within-tree clone bar for that reason.
const MIN_STRADDLE_MASS: u32 = 48;

/// What a dependency package contributes. Volume is the honest part of
/// a supply-chain answer: most trees are mostly code nobody chose.
#[derive(Default)]
struct Pkg {
    files: u32,
    lines: u64,
    secrets: u32,
    spooky: u32,
    suppressions: u32,
    /// Files too garbled or too deep (minified bundles) to measure.
    unmeasurable: u32,
}

impl Pkg {
    fn findings(&self) -> u32 {
        self.secrets + self.spooky + self.suppressions
    }

    fn absorb(&mut self, other: &Pkg) {
        self.files += other.files;
        self.lines += other.lines;
        self.secrets += other.secrets;
        self.spooky += other.spooky;
        self.suppressions += other.suppressions;
        self.unmeasurable += other.unmeasurable;
    }
}

struct Site {
    kind: &'static str,
    path: String,
    line: u32,
}

/// A normalized subtree, and where in the dependency tree it sits.
type Print = (String, u32, u32);

#[derive(Default)]
struct Scanned {
    packages: HashMap<String, Pkg>,
    sites: Vec<Site>,
    prints: HashMap<u64, Print>,
    generated: u32,
}

impl Scanned {
    fn merge(mut a: Scanned, b: Scanned) -> Scanned {
        for (name, pkg) in &b.packages {
            a.packages.entry(name.clone()).or_default().absorb(pkg);
        }
        a.sites.extend(b.sites);
        for (hash, site) in b.prints {
            a.prints.entry(hash).or_insert(site);
        }
        a.generated += b.generated;
        a
    }

    fn add_file(&mut self, facts: &FileFacts, package: &str) {
        let path = facts.path.display().to_string();
        let pkg = self.packages.entry(package.to_string()).or_default();
        pkg.files += 1;
        pkg.lines += facts.lines as u64;
        // A garbled parse cannot be trusted about credentials; the
        // volume still counts, and the file says so in its own column.
        if facts.low_confidence() {
            pkg.unmeasurable += 1;
            return;
        }
        pkg.secrets += facts.secrets.len() as u32;
        pkg.spooky += facts.spooky_lines.len() as u32;
        pkg.suppressions += facts.suppressions.len() as u32;
        for (kind, lines) in [
            ("secret", &facts.secrets),
            ("spooky", &facts.spooky_lines),
            ("suppression", &facts.suppressions),
        ] {
            for line in lines {
                self.sites.push(Site {
                    kind,
                    path: path.clone(),
                    line: *line,
                });
            }
        }
        for site in &facts.clone_sites {
            self.prints
                .entry(site.hash)
                .or_insert((path.clone(), site.line, site.mass));
        }
    }
}

pub fn run(root: &Path, top: usize) -> Result<i32, Box<dyn Error>> {
    let files = dep_files(root);
    if files.is_empty() {
        println!(
            "deps — no dependency directories ({}) under {}",
            DEP_DIRS.join(", "),
            root.display()
        );
        return Ok(0);
    }
    let scanned = crate::in_pool(|| scan(&files));
    let straddling = straddling(root, &scanned.prints)?;
    print!("{}", render(&scanned, &straddling, top.max(SHOW)));
    Ok(0)
}

fn scan(files: &[PathBuf]) -> Scanned {
    files
        .par_iter()
        .fold(
            || (Scanned::default(), crate::Parsers::default()),
            |(mut acc, mut parsers), path| {
                let package = package_of(path).unwrap_or_else(|| "?".to_string());
                match std::fs::read_to_string(path) {
                    Ok(source) if crate::config::is_generated(path, &source) => acc.generated += 1,
                    Ok(source) => {
                        if let Some((lang, text)) = crate::measurable(path, &source) {
                            let f = crate::facts::extract(
                                lang.pack_for(path),
                                parsers.get(lang),
                                path,
                                &text,
                            );
                            acc.add_file(&f, &package);
                        }
                    }
                    // Unreadable is not a finding about a dependency.
                    Err(_) => {}
                }
                (acc, parsers)
            },
        )
        .map(|(acc, _)| acc)
        .reduce(Scanned::default, Scanned::merge)
}

/// Logic present in BOTH trees: a vendored copy of your code, or your
/// code copied out of a dependency. Either way it is a maintenance
/// obligation nobody declared: the dependency will not carry your
/// fixes, and your tree will not carry its.
struct Straddle {
    mass: u32,
    own: String,
    own_line: u32,
    dep: String,
    dep_line: u32,
}

fn straddling(root: &Path, prints: &HashMap<u64, Print>) -> Result<Vec<Straddle>, Box<dyn Error>> {
    let cfg = crate::config::Config::load(root)?;
    let own = crate::collect_files(std::slice::from_ref(&root.to_path_buf()), &cfg);
    let mut found: Vec<Straddle> = crate::in_pool(|| {
        own.par_iter()
            .fold(
                || (Vec::new(), crate::Parsers::default()),
                |(mut hits, mut parsers), path| {
                    if let Ok(source) = std::fs::read_to_string(path)
                        && !crate::config::is_generated(path, &source)
                    {
                        let lang = Lang::of_source(path, &source)
                            .expect("collect_files filters by language");
                        let f = crate::facts::extract(
                            lang.pack_for(path),
                            parsers.get(lang),
                            path,
                            &source,
                        );
                        if !f.low_confidence() {
                            collect_straddles(&f, prints, &mut hits);
                        }
                    }
                    (hits, parsers)
                },
            )
            .map(|(hits, _)| hits)
            .reduce(Vec::new, |mut a, mut b| {
                a.append(&mut b);
                a
            })
    });
    found.sort_by(|a, b| {
        b.mass
            .cmp(&a.mass)
            .then_with(|| a.own.cmp(&b.own))
            .then_with(|| a.own_line.cmp(&b.own_line))
    });
    found.dedup_by(|a, b| a.own == b.own && a.own_line == b.own_line);
    Ok(found)
}

fn collect_straddles(f: &FileFacts, prints: &HashMap<u64, Print>, into: &mut Vec<Straddle>) {
    let path = f.path.display().to_string();
    for site in &f.clone_sites {
        if site.mass < MIN_STRADDLE_MASS {
            continue;
        }
        let Some((dep, dep_line, _)) = prints.get(&site.hash) else {
            continue;
        };
        into.push(Straddle {
            mass: site.mass,
            own: path.clone(),
            own_line: site.line,
            dep: dep.clone(),
            dep_line: *dep_line,
        });
    }
}

/// Every analyzable file inside a dependency directory. The walk
/// deliberately ignores .gitignore: a dependency tree is ignored by
/// definition, which is why nothing ever looks at it.
fn dep_files(root: &Path) -> Vec<PathBuf> {
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .hidden(false)
        // A vendored repository's own history is not its code.
        .filter_entry(|e| e.file_name() != ".git");
    let mut files: Vec<PathBuf> = walker
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|p| Lang::from_path(p).is_some() && package_of(p).is_some())
        .collect();
    files.sort_unstable();
    files.dedup();
    files
}

/// Which package a dependency file belongs to. The LAST dependency
/// directory on the path wins, so `node_modules/a/node_modules/b`
/// belongs to b. An npm scope carries two components (`@scope/pkg`),
/// and Go's vendor tree is domain-qualified, so a dotted first
/// component takes three (`github.com/org/repo`). The Go import
/// resolver reads the same evidence.
fn package_of(path: &Path) -> Option<String> {
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let at = parts.iter().rposition(|p| DEP_DIRS.contains(p))?;
    let rest = &parts[at + 1..];
    let first = rest.first()?;
    let take = match first {
        f if f.starts_with('@') => 2,
        f if f.contains('.') => 3,
        _ => 1,
    };
    Some(rest[..take.min(rest.len())].join("/"))
}

fn render(scanned: &Scanned, straddling: &[Straddle], show: usize) -> String {
    let mut out = String::new();
    let mut ranked: Vec<(&String, &Pkg)> = scanned.packages.iter().collect();
    ranked.sort_by(|(a_name, a), (b_name, b)| {
        b.findings()
            .cmp(&a.findings())
            .then_with(|| b.lines.cmp(&a.lines))
            .then_with(|| a_name.cmp(b_name))
    });
    let totals = ranked.iter().fold(Pkg::default(), |mut acc, (_, p)| {
        acc.absorb(p);
        acc
    });
    let _ = writeln!(
        out,
        "deps — {} packages, {} files, {} lines you did not write\n\
         \x20 credentials, action at a distance, checker suppressions, volume and\n\
         \x20 shared logic only: a dependency's complexity is not yours to refactor.\n",
        ranked.len(),
        totals.files,
        totals.lines,
    );
    let _ = writeln!(
        out,
        "{:>8} {:>8} {:>8} {:>6}  package",
        "findings", "lines", "files", "?"
    );
    for (name, pkg) in ranked.iter().take(show) {
        let _ = writeln!(
            out,
            "{:>8} {:>8} {:>8} {:>6}  {name}",
            pkg.findings(),
            pkg.lines,
            pkg.files,
            pkg.unmeasurable,
        );
    }
    if ranked.len() > show {
        let _ = writeln!(out, "  ... and {} more packages", ranked.len() - show);
    }
    let _ = writeln!(
        out,
        "\n? = files too garbled or too deep to measure (minified bundles);\n\
         their volume counts, their findings cannot."
    );
    render_sites(scanned, &totals, &mut out);
    render_straddling(straddling, show, &mut out);
    out
}

fn render_sites(scanned: &Scanned, totals: &Pkg, out: &mut String) {
    let _ = writeln!(
        out,
        "\nsecrets {} · spooky {} · suppressions {}{}",
        totals.secrets,
        totals.spooky,
        totals.suppressions,
        if scanned.generated > 0 {
            format!(" · {} generated files skipped", scanned.generated)
        } else {
            String::new()
        },
    );
    let mut sites: Vec<&Site> = scanned.sites.iter().collect();
    sites.sort_by(|a, b| {
        a.kind
            .cmp(b.kind)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    for site in sites.iter().take(SITES) {
        let _ = writeln!(out, "  {:<12} {}:{}", site.kind, site.path, site.line);
    }
    if sites.len() > SITES {
        let _ = writeln!(out, "  ... and {} more sites", sites.len() - SITES);
    }
}

fn render_straddling(straddling: &[Straddle], show: usize, out: &mut String) {
    if straddling.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\nshared logic — {} places your tree and a dependency both contain\n\
         (a vendored copy, or code copied out of a dependency: the fixes\n\
         will not travel in either direction):",
        straddling.len()
    );
    for s in straddling.iter().take(show) {
        let _ = writeln!(
            out,
            "  mass {:<5} {}:{}\n{:<12} {}:{}",
            s.mass, s.own, s.own_line, "", s.dep, s.dep_line
        );
    }
    if straddling.len() > show {
        let _ = writeln!(out, "  ... and {} more", straddling.len() - show);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_names_the_package_it_shipped_in() {
        let cases = [
            ("app/node_modules/ajv/lib/compile.js", Some("ajv")),
            (
                "node_modules/@sentry/node/dist/index.js",
                Some("@sentry/node"),
            ),
            // The last dependency directory wins: a nested install
            // belongs to the inner package.
            ("node_modules/a/node_modules/b/index.js", Some("b")),
            (
                "vendor/github.com/evanw/esbuild/pkg/api.go",
                Some("github.com/evanw/esbuild"),
            ),
            ("venv/lib/site-packages/click/core.py", Some("click")),
            // Your own tree is not a dependency.
            ("src/app/index.ts", None),
        ];
        for (path, want) in cases {
            assert_eq!(package_of(Path::new(path)).as_deref(), want, "{path}");
        }
    }

    #[test]
    fn the_walk_sees_what_the_ordinary_scan_prunes() {
        let dir = std::env::temp_dir().join(format!("elegance-deps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules/left-pad")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        // node_modules is ignored by convention, which is why nothing
        // ever looks inside it.
        std::fs::write(dir.join(".gitignore"), "node_modules\n").unwrap();
        std::fs::write(
            dir.join("node_modules/left-pad/index.js"),
            "const apiKey = 'sk9f3kd0asdf8812jjd';\nmodule.exports = () => eval(apiKey);\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/app.js"), "export const go = () => 1;\n").unwrap();

        let cfg = crate::config::Config::default();
        let ordinary = crate::collect_files(std::slice::from_ref(&dir), &cfg);
        assert!(
            ordinary
                .iter()
                .all(|p| !p.to_string_lossy().contains("node_modules")),
            "the ordinary scan prunes dependencies"
        );

        let found = dep_files(&dir);
        assert_eq!(found.len(), 1, "the deps walk sees past the gitignore");

        let scanned = scan(&found);
        let pkg = scanned.packages.get("left-pad").expect("package named");
        assert_eq!(pkg.secrets, 1, "a credential in a dependency is a finding");
        assert_eq!(pkg.spooky, 1, "so is eval");
        let out = render(&scanned, &[], 10);
        assert!(out.contains("left-pad"), "{out}");
        // Unit-shape metrics are deliberately absent from the mode.
        assert!(!out.contains("cognitive"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
