//! Module-graph facts and import resolution — the dependency tier's
//! foundation. Imports resolve against the scanned file set only:
//! internal edges become graph material, external ones are dependency
//! surface, and internal-looking targets that resolve to nothing are
//! reported as unresolved — the honesty bucket.
//!
//! Module identity is language-true where it diverges from files:
//! `pkg/__init__.py` answers to `pkg`, `foo/mod.rs` to `foo`,
//! `dir/index.ts` to `dir`; Go packages are directories (edges land on
//! a representative file of the package).

pub mod metrics;

pub use metrics::analyze;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::lang::Lang;

/// Slim per-file graph facts retained by the aggregate (full facts are
/// transient). Callers sort by path before analysis so ambiguous
/// resolutions pick deterministically.
pub struct GraphFacts {
    pub path: PathBuf,
    pub lang: Lang,
    pub is_test: bool,
    pub imports: Vec<crate::facts::ImportFact>,
    /// Names of the module's declared surface (public units and types).
    pub exports: Vec<Box<str>>,
    /// Named-node mass — the volume a module hides behind its surface.
    pub mass: u32,
    /// Ousterhout surface cost: Σ over exported units of
    /// 1 + params + 2×flag params (exported types cost 1).
    pub surface_cost: u32,
}

#[derive(Default, Debug, PartialEq)]
pub struct Resolution {
    pub internal: u32,
    pub external: u32,
    pub unresolved: u32,
}

impl Resolution {
    /// Share of internal-looking imports that resolved.
    pub fn rate(&self) -> f64 {
        let looking = self.internal + self.unresolved;
        if looking == 0 {
            1.0
        } else {
            self.internal as f64 / looking as f64
        }
    }
}

/// Resolve every import: per-file, per-import internal target indices
/// (None for external/unresolved), aligned with each file's imports.
pub fn resolve_imports(files: &[GraphFacts]) -> (Resolution, Vec<Vec<Option<usize>>>) {
    let index = Index::build(files);
    let mut r = Resolution::default();
    let mut targets = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        let row = f
            .imports
            .iter()
            .map(|imp| match index.classify(i, f, &imp.target) {
                Class::Internal(j) => {
                    r.internal += 1;
                    Some(j)
                }
                Class::External => {
                    r.external += 1;
                    None
                }
                Class::Unresolved => {
                    r.unresolved += 1;
                    None
                }
            })
            .collect();
        targets.push(row);
    }
    (r, targets)
}

/// Deduplicated internal edges (importer, target), self-edges dropped.
pub fn edges(files: &[GraphFacts]) -> (Resolution, Vec<(u32, u32)>) {
    let (r, targets) = resolve_imports(files);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, row) in targets.iter().enumerate() {
        for j in row.iter().flatten() {
            if i != *j && seen.insert((i, *j)) {
                out.push((i as u32, *j as u32));
            }
        }
    }
    (r, out)
}

pub fn resolve(files: &[GraphFacts]) -> Resolution {
    edges(files).0
}

enum Class {
    Internal(usize),
    External,
    Unresolved,
}

/// Module component path (`["src", "attr", "validators"]`) plus the
/// index of the file it belongs to.
type CompEntry = (Vec<Box<str>>, usize);

struct Index {
    paths: HashMap<PathBuf, usize>,
    /// Language-true module component vectors: exact map and last-segment
    /// suffix index (`attr.validators` matches [..,"attr","validators"]).
    modules: HashMap<Vec<Box<str>>, usize>,
    by_last: HashMap<Box<str>, Vec<CompEntry>>,
    /// Every file's module components, by file index — what a `crate::`
    /// path has to be anchored against.
    file_comps: Vec<Vec<Box<str>>>,
    /// Directory component vectors (Go packages; representative file).
    dirs: HashMap<Box<str>, Vec<CompEntry>>,
    basenames: HashMap<Box<str>, usize>,
    /// Rust crate roots (lib.rs/main.rs) by their directory components.
    crate_roots: HashMap<Vec<Box<str>>, usize>,
    /// Rust symbol -> defining files, for `use crate::Symbol` edges:
    /// attributing root re-exports to lib.rs would manufacture hub
    /// cycles the code does not have.
    rust_exports: HashMap<Box<str>, Vec<usize>>,
    /// Each file's nearest enclosing crate root, precomputed.
    file_crate_root: Vec<Option<usize>>,
}

/// What separates one component of an import target from the next.
fn component_separators(lang: Lang) -> &'static [char] {
    match lang {
        Lang::Lua => &['.', '/'],
        Lang::Ruby | Lang::Solidity => &['/'],
        Lang::Php => &['\\'],
        Lang::Perl => &[':'],
        _ => &['.'],
    }
}

impl Index {
    fn build(files: &[GraphFacts]) -> Index {
        let mut idx = Index {
            paths: HashMap::new(),
            modules: HashMap::new(),
            by_last: HashMap::new(),
            dirs: HashMap::new(),
            basenames: HashMap::new(),
            crate_roots: HashMap::new(),
            rust_exports: HashMap::new(),
            file_crate_root: Vec::new(),
            file_comps: Vec::new(),
        };
        for (i, f) in files.iter().enumerate() {
            idx.paths.entry(f.path.clone()).or_insert(i);
            let comps = module_components(f);
            idx.file_comps.push(comps.clone());
            if let Some(last) = comps.last() {
                idx.by_last
                    .entry(last.clone())
                    .or_default()
                    .push((comps.clone(), i));
            }
            if f.lang == Lang::Rust {
                if matches!(
                    f.path.file_stem().and_then(|s| s.to_str()),
                    Some("lib" | "main")
                ) {
                    idx.crate_roots.entry(comps.clone()).or_insert(i);
                }
                for sym in &f.exports {
                    idx.rust_exports.entry(sym.clone()).or_default().push(i);
                }
            }
            idx.modules.entry(comps).or_insert(i);
            let dir: Vec<Box<str>> = components(f.path.parent().unwrap_or(Path::new("")));
            if let Some(last) = dir.last() {
                idx.dirs.entry(last.clone()).or_default().push((dir, i));
            }
            if let Some(base) = f.path.file_name().and_then(|b| b.to_str()) {
                idx.basenames.entry(base.into()).or_insert(i);
            }
        }
        // Second pass: crate roots are only complete now.
        idx.file_crate_root = files
            .iter()
            .map(|f| {
                let comps = module_components(f);
                (0..=comps.len())
                    .rev()
                    .find_map(|n| idx.crate_roots.get(&comps[..n]).copied())
            })
            .collect();
        idx
    }

    fn classify(&self, i: usize, from: &GraphFacts, target: &str) -> Class {
        match from.lang {
            Lang::Python => self.python(from, target),
            Lang::Rust => self.rust(i, from, target),
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => self.web(from, target),
            Lang::Go => self.go(target),
            Lang::Zig => self.zig(from, target),
            // C++ includes resolve exactly as C's do: a quoted path is
            // relative to the including file, an angled one is a
            // system header and definitionally external.
            Lang::C | Lang::Cpp | Lang::Cuda => self.c(from, target),
            // A sourced path is relative to the script — or assembled
            // at run time from a variable, which resolves to nothing
            // and lands in the honesty bucket where it belongs.
            Lang::Shell => self.shell(from, target),
            // Everything below resolves the same way: split the target
            // on whatever this language uses to separate namespace or
            // path components, then match the tail against the scanned
            // files. Only the separators differ.
            //
            //   OCaml     `open Core` names a module, by its own name
            //   Lua       `require "a.b.c"` walks package.path
            //   Ruby      `require_relative "a/b"` is a path
            //   Perl      `use Foo::Bar` mirrors the namespace
            //   PHP       `use Foo\\Bar` likewise
            //   Solidity  an import names a FILE path
            //   the rest  a package or namespace names a directory
            lang => {
                let parts: Vec<&str> = target
                    .split(component_separators(lang))
                    .filter(|p| !p.is_empty())
                    .collect();
                match self.suffix(&parts) {
                    Some(i) => Class::Internal(i),
                    None => Class::External,
                }
            }
        }
    }

    fn python(&self, from: &GraphFacts, target: &str) -> Class {
        if !target.starts_with('.') {
            // Absolute: internal if the dotted path is a module suffix
            // here; otherwise a dependency.
            let segs: Vec<&str> = target.split('.').collect();
            return match self.suffix(&segs) {
                Some(i) => Class::Internal(i),
                None => Class::External,
            };
        }
        // Relative: each dot beyond the first climbs one package.
        let dots = target.chars().take_while(|c| *c == '.').count();
        let mut base = from.path.parent().unwrap_or(Path::new("")).to_path_buf();
        for _ in 1..dots {
            base = base.parent().unwrap_or(Path::new("")).to_path_buf();
        }
        for seg in target[dots..].split('.').filter(|s| !s.is_empty()) {
            base.push(seg);
        }
        self.paths
            .get(&base.with_extension("py"))
            .or_else(|| self.paths.get(&base.join("__init__.py")))
            .map_or(Class::Unresolved, |&i| Class::Internal(i))
    }

    fn rust(&self, i: usize, from: &GraphFacts, target: &str) -> Class {
        let segs: Vec<&str> = target
            .split("::")
            .filter(|s| !s.is_empty() && *s != "*")
            .collect();
        let Some((first, rest)) = segs.split_first() else {
            return Class::Unresolved;
        };
        match *first {
            "crate" => self.rust_crate(i, rest),
            "super" | "self" => self.rust_relative(from, first, rest),
            // Bare roots: a sibling top-level module (2015-style) or an
            // external crate.
            _ => match self
                .suffix(&segs)
                .or_else(|| self.suffix(&segs[..segs.len() - 1]))
            {
                Some(i) => Class::Internal(i),
                None => Class::External,
            },
        }
    }

    /// `crate::...`: the leaf may be a symbol, not a module — try both;
    /// a bare symbol resolves to the module that DEFINES it (never the
    /// re-exporting root, which would manufacture hub cycles), then to
    /// the crate root.
    fn rust_crate(&self, i: usize, rest: &[&str]) -> Class {
        // `crate::a::b` names exactly <crate root>/a/b. Suffix matching
        // it instead picks whichever file happens to end in the same
        // segment, and this repository holds two called `metrics` —
        // src/metrics/mod.rs and src/graph/metrics.rs — so every
        // `crate::metrics::` edge landed on the wrong one and the real
        // module read as an orphan.
        if let Some(root) = self.file_crate_root[i]
            && let Some(root_comps) = self.file_comps.get(root)
        {
            for probe in [rest, &rest[..rest.len().saturating_sub(1)]] {
                let mut candidate = root_comps.clone();
                candidate.extend(probe.iter().map(|s| Box::<str>::from(*s)));
                if let Some(&m) = self.modules.get(&candidate) {
                    return Class::Internal(m);
                }
            }
        }
        let minus_leaf = &rest[..rest.len().saturating_sub(1)];
        if let Some(m) = self.suffix(rest).or_else(|| self.suffix(minus_leaf)) {
            return Class::Internal(m);
        }
        if rest.len() != 1 {
            return Class::Unresolved;
        }
        let root = self.file_crate_root[i];
        let defined = self.rust_exports.get(rest[0]).and_then(|files| {
            files
                .iter()
                .find(|&&d| self.file_crate_root[d] == root)
                .copied()
        });
        match defined.or(root) {
            Some(d) => Class::Internal(d),
            None => Class::Unresolved,
        }
    }

    /// `super::`/`self::` paths, walked from the importer's own module.
    fn rust_relative(&self, from: &GraphFacts, first: &str, rest: &[&str]) -> Class {
        let mut comps = module_components(from);
        if first == "super" {
            comps.pop();
        }
        let mut segs = rest;
        while segs.first() == Some(&"super") {
            comps.pop();
            segs = &segs[1..];
        }
        for probe in [segs, &segs[..segs.len().saturating_sub(1)]] {
            let mut candidate = comps.clone();
            candidate.extend(probe.iter().map(|s| Box::<str>::from(*s)));
            if let Some(&i) = self.modules.get(&candidate) {
                return Class::Internal(i);
            }
        }
        Class::Unresolved
    }

    fn web(&self, from: &GraphFacts, target: &str) -> Class {
        if !target.starts_with('.') {
            return Class::External; // package imports, path aliases
        }
        let joined = normalize(from.path.parent().unwrap_or(Path::new("")), target);
        const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];
        if let Some(&i) = self.paths.get(&joined) {
            return Class::Internal(i);
        }
        for ext in EXTS {
            if let Some(&i) = self
                .paths
                .get(&joined.with_extension(ext))
                .or_else(|| self.paths.get(&joined.join("index").with_extension(ext)))
            {
                return Class::Internal(i);
            }
        }
        // nodenext style: `./x.js` on disk as x.ts.
        if joined.extension().is_some_and(|e| e == "js" || e == "jsx")
            && let Some(i) = EXTS
                .iter()
                .find_map(|e| self.paths.get(&joined.with_extension(e)))
        {
            return Class::Internal(*i);
        }
        Class::Unresolved
    }

    fn go(&self, target: &str) -> Class {
        // Module paths are domain-qualified (github.com/...); a bare
        // first segment is the standard library — `import "runtime"`
        // must never suffix-match an internal/runtime directory.
        if !target.split('/').next().unwrap_or("").contains('.') {
            return Class::External;
        }
        // The tail of the module path maps onto package directories;
        // unmatched paths are dependencies.
        let segs: Vec<&str> = target.split('/').collect();
        for take in (1..=segs.len().min(4)).rev() {
            let tail = &segs[segs.len() - take..];
            let Some(cands) = self.dirs.get(tail[take - 1]) else {
                continue;
            };
            if let Some((_, i)) = cands.iter().find(|(c, _)| ends_with(c, tail)) {
                return Class::Internal(*i);
            }
        }
        Class::External
    }

    /// `source ./lib/common.sh`. Interpolated paths (`. "$DIR/x.sh"`)
    /// cannot be resolved without running the script, so they resolve
    /// by BASENAME when one matches and stay unresolved otherwise.
    fn shell(&self, from: &GraphFacts, target: &str) -> Class {
        let joined = normalize(from.path.parent().unwrap_or(Path::new("")), target);
        if let Some(&i) = self.paths.get(&joined) {
            return Class::Internal(i);
        }
        let base = target.rsplit('/').next().unwrap_or(target);
        match self.basenames.get(base) {
            Some(&i) => Class::Internal(i),
            None => Class::Unresolved,
        }
    }

    fn zig(&self, from: &GraphFacts, target: &str) -> Class {
        if !target.ends_with(".zig") {
            return Class::External; // std, builtin, build packages
        }
        let joined = normalize(from.path.parent().unwrap_or(Path::new("")), target);
        match self.paths.get(&joined) {
            Some(&i) => Class::Internal(i),
            None => Class::Unresolved,
        }
    }

    fn c(&self, from: &GraphFacts, target: &str) -> Class {
        if target.starts_with('<') {
            return Class::External;
        }
        let joined = normalize(from.path.parent().unwrap_or(Path::new("")), target);
        if let Some(&i) = self.paths.get(&joined) {
            return Class::Internal(i);
        }
        // Include paths are unknowable; a matching basename is evidence
        // enough.
        let base = target.rsplit('/').next().unwrap_or(target);
        match self.basenames.get(base) {
            Some(&i) => Class::Internal(i),
            None => Class::Unresolved,
        }
    }

    /// First module whose component vector the segments suffix-match
    /// (files arrive path-sorted, so "first" is deterministic).
    fn suffix(&self, segs: &[&str]) -> Option<usize> {
        let last = segs.last()?;
        self.by_last
            .get(*last)?
            .iter()
            .find(|(c, _)| ends_with(c, segs))
            .map(|(_, i)| *i)
    }
}

fn ends_with(comps: &[Box<str>], segs: &[&str]) -> bool {
    comps.len() >= segs.len()
        && comps[comps.len() - segs.len()..]
            .iter()
            .zip(segs)
            .all(|(a, b)| &**a == *b)
}

/// A file's language-true module components: extension dropped, and the
/// filename that means "the directory is the module" dropped too.
fn module_components(f: &GraphFacts) -> Vec<Box<str>> {
    let mut comps = components(&f.path.with_extension(""));
    let dir_marker = match f.lang {
        Lang::Python => "__init__",
        Lang::Rust => "mod",
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => "index",
        _ => "",
    };
    let crate_root =
        f.lang == Lang::Rust && matches!(comps.last().map(|c| &**c), Some("lib" | "main"));
    if comps.last().is_some_and(|c| &**c == dir_marker) || crate_root {
        comps.pop();
    }
    comps
}

fn components(path: &Path) -> Vec<Box<str>> {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .map(Into::into)
        .collect()
}

/// Join and fold `.`/`..` without touching the filesystem.
fn normalize(base: &Path, rel: &str) -> PathBuf {
    let mut out: Vec<&std::ffi::OsStr> = base.components().map(|c| c.as_os_str()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(std::ffi::OsStr::new(seg)),
        }
    }
    out.iter().collect()
}

#[cfg(test)]
pub(super) fn fixture(lang: Lang, path: &str, imports: &[&str]) -> GraphFacts {
    GraphFacts {
        path: PathBuf::from(path),
        lang,
        is_test: false,
        imports: imports
            .iter()
            .map(|i| crate::facts::ImportFact {
                target: (*i).into(),
                names: Vec::new(),
            })
            .collect(),
        exports: Vec::new(),
        mass: 0,
        surface_cost: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(lang: Lang, path: &str, imports: &[&str]) -> GraphFacts {
        fixture(lang, path, imports)
    }

    #[test]
    fn python_relative_and_absolute_imports_resolve() {
        let files = [
            file(
                Lang::Python,
                "src/attr/validators.py",
                &["attr.converters", "..attr", ".converters", "os"],
            ),
            file(Lang::Python, "src/attr/converters.py", &[]),
            file(Lang::Python, "src/attr/__init__.py", &["typing"]),
        ];
        let r = resolve(&files);
        // attr.converters (suffix), ..attr (package __init__), .converters
        // (sibling) internal; os and typing external.
        assert_eq!(
            r,
            Resolution {
                internal: 3,
                external: 2,
                unresolved: 0
            }
        );
    }

    #[test]
    fn rust_mod_tree_resolves_crate_super_and_bare_roots() {
        let files = [
            file(
                Lang::Rust,
                "src/lib.rs",
                &["crate::facts::extract", "serde::Serialize"],
            ),
            file(Lang::Rust, "src/facts/mod.rs", &["super::lang::Pack"]),
            file(
                Lang::Rust,
                "src/facts/extract.rs",
                &["self::helpers", "crate::lang"],
            ),
            file(Lang::Rust, "src/lang.rs", &[]),
        ];
        let r = resolve(&files);
        // crate::facts::extract, super::lang (leaf Pack a symbol),
        // self::helpers (inline mod -> importer), crate::lang internal;
        // serde external.
        assert_eq!(
            r,
            Resolution {
                internal: 4,
                external: 1,
                unresolved: 0
            }
        );
    }

    #[test]
    fn a_crate_path_lands_on_the_module_it_names_not_a_namesake() {
        // Two modules can share a last segment — this repository holds
        // src/metrics/mod.rs and src/graph/metrics.rs — and matching a
        // `crate::` path by its last segment picked whichever was
        // scanned first. Every `crate::metrics::` edge went to the
        // wrong file, and the real module read as an orphan with no
        // importers at all.
        let files = [
            file(Lang::Rust, "src/main.rs", &["self::metrics", "self::graph"]),
            file(Lang::Rust, "src/metrics/mod.rs", &[]),
            file(Lang::Rust, "src/graph/mod.rs", &["self::metrics"]),
            file(Lang::Rust, "src/graph/metrics.rs", &[]),
            file(Lang::Rust, "src/rollup.rs", &["crate::metrics::METRICS"]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        assert_eq!(res.unresolved, 0, "every edge resolves");

        // `mod metrics;` in main.rs is src/metrics, and the same
        // declaration inside src/graph/mod.rs is src/graph/metrics.
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[0][0], Some(idx("src/metrics/mod.rs")));
        assert_eq!(targets[2][0], Some(idx("src/graph/metrics.rs")));
        // And a crate path from a third file anchors at the crate root,
        // so it reaches the module rather than the namesake.
        assert_eq!(targets[4][0], Some(idx("src/metrics/mod.rs")));
    }

    #[test]
    fn crate_root_symbols_resolve_to_the_root() {
        let files = [
            file(Lang::Rust, "src/lib.rs", &[]),
            file(Lang::Rust, "src/search.rs", &["crate::Match"]),
        ];
        let (r, e) = edges(&files);
        assert_eq!(r.internal, 1);
        assert_eq!(e, [(1, 0)], "edge points at the crate root");
    }

    #[test]
    fn web_relative_imports_honor_extensions_and_index() {
        let files = [
            file(
                Lang::TypeScript,
                "web/src/app.ts",
                &["./util", "../lib", "./missing", "react"],
            ),
            file(Lang::TypeScript, "web/src/util.ts", &[]),
            file(Lang::TypeScript, "web/lib/index.ts", &[]),
        ];
        let r = resolve(&files);
        assert_eq!(
            r,
            Resolution {
                internal: 2,
                external: 1,
                unresolved: 1
            }
        );
    }

    #[test]
    fn go_zig_and_c_targets_classify_by_their_conventions() {
        let files = [
            file(
                Lang::Go,
                "esbuild/internal/parser/parser.go",
                &["github.com/evanw/esbuild/internal/lexer", "fmt"],
            ),
            file(Lang::Go, "esbuild/internal/lexer/lexer.go", &[]),
            file(Lang::Zig, "src/vsr/replica.zig", &["../stdx.zig", "std"]),
            file(Lang::Zig, "src/stdx.zig", &[]),
            file(Lang::C, "src/server.c", &["server.h", "<stdio.h>"]),
            file(Lang::C, "src/server.h", &[]),
        ];
        let r = resolve(&files);
        assert_eq!(
            r,
            Resolution {
                internal: 3,
                external: 3,
                unresolved: 0
            }
        );
    }
}
