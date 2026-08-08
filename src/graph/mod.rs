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
    /// Members reached through a receiver rather than through a type
    /// name. See `FileFacts::receiver_units`.
    pub receiver_units: Vec<Box<str>>,
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
    /// Record one classified import, and hand back the module it names.
    fn count(&mut self, class: Class) -> Option<usize> {
        match class {
            Class::Internal(j) => {
                self.internal += 1;
                Some(j)
            }
            Class::External => {
                self.external += 1;
                None
            }
            Class::Unresolved => {
                self.unresolved += 1;
                None
            }
        }
    }

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
        let mut row: Vec<Option<usize>> = Vec::with_capacity(f.imports.len());
        // A require assembled at run time still states the DIRECTORY it
        // looks in, and every file under that directory is a candidate
        // the source cannot narrow further. The first stands in the
        // import's own row and the rest ride PAST it -- every reader
        // zips against `imports` and stops there -- so they add edges
        // without disturbing the alignment.
        let mut extra: Vec<Option<usize>> = Vec::new();
        for imp in &f.imports {
            let mut named = index.named_modules(f, imp).into_iter();
            // A prefix that names modules of this project is not a
            // third-party dependency, whatever the rest of its name
            // turns out to be at run time.
            let class = named
                .next()
                .map_or_else(|| index.classify(i, f, imp), Class::Internal);
            // A mention states no dependency, so it reaches the graph
            // without reaching the tally.
            let target = match imp.reach {
                crate::lang::Reach::Mention | crate::lang::Reach::Member => class.module(),
                _ => r.count(class),
            };
            extra.extend(named.map(Some));
            extra.extend(index.submodules(target, imp, f.lang).into_iter().map(Some));
            row.push(target);
        }
        row.extend(extra);
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

#[derive(Clone, Copy)]
enum Class {
    Internal(usize),
    External,
    Unresolved,
}

impl Class {
    /// The module of this project it names, if it names one.
    fn module(self) -> Option<usize> {
        match self {
            Class::Internal(j) => Some(j),
            _ => None,
        }
    }
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
    /// Every file's path components, keyed by file name — what a
    /// path-shaped specifier (`cutlass/gemm/gemm.h`) matches against.
    basenames: HashMap<Box<str>, Vec<CompEntry>>,
    /// Rust crate roots (lib.rs/main.rs) by their directory components.
    crate_roots: HashMap<Vec<Box<str>>, usize>,
    /// Exported symbol -> the files declaring it, keyed by language so a
    /// C# `Policy` cannot bind an OCaml one.
    ///
    /// Rust needs it because attributing a root re-export to lib.rs
    /// would manufacture hub cycles the code does not have. C# needs it
    /// because the declaration is the ONLY statement about where a type
    /// lives: a `partial` class is spread over `SqlMapper.cs`,
    /// `SqlMapper.TypeHandler.cs` and eleven more, and 87 of the gold
    /// corpus's 102 dotted-stem files are one.
    declared: HashMap<(Lang, Box<str>), Vec<usize>>,
    /// Each file's nearest enclosing crate root, precomputed.
    file_crate_root: Vec<Option<usize>>,
    /// A module name -> the directory holding it, where a MANIFEST says
    /// so rather than the tree. Covers a monorepo's own npm packages
    /// (3843 tsx specifiers name one) and a SwiftPM target whose
    /// `path:` moves it off the Sources/<name> convention.
    workspaces: HashMap<Box<str>, PathBuf>,
    /// Member name -> the files declaring it as an extension method.
    /// Separate from `declared` so a call on a value can never land on a
    /// TYPE of the same name. See `Reach::Member`.
    members: HashMap<Box<str>, Vec<usize>>,
    /// Each file's language. A module name binds a file of the language
    /// that named it: the OCaml corpus ships util.h, config.h and
    /// sha256.c beside OCaml modules of the same stem.
    langs: Vec<Lang>,
    /// The two dune stanzas that widen where a bare OCaml module name
    /// answers from. See `DuneScopes`.
    dune: DuneScopes,
}

/// A dune library is a directory, and two stanzas say it is more than
/// that.
///
/// `(include_subdirs unqualified)` folds every SUBDIRECTORY of the
/// library into one flat module namespace, so `dune_rules/gen_rules.ml`
/// writing `Cram_rules.rules` means `dune_rules/cram/cram_rules.ml`.
/// `src/dune_lang/dune`, `src/dune_rules/dune` and `src/dune_tui/dune`
/// all declare it.
///
/// `(wrapped false)` drops the `Libname__` prefix, which makes every one
/// of the library's modules a top-level compilation unit its CONSUMERS
/// name bare: containers' `src/data/dune` declares it, and
/// `tests/data/t_bv.ml:3` says `open CCBV` with no qualification at all.
#[derive(Default)]
struct DuneScopes {
    flat: Vec<Vec<Box<str>>>,
    unwrapped: HashSet<Vec<Box<str>>>,
}

/// What separates one component of an import target from the next.
fn component_separators(lang: Lang) -> &'static [char] {
    match lang {
        Lang::Lua => &['.', '/'],
        Lang::Ruby => &['/'],
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
            file_crate_root: Vec::new(),
            file_comps: Vec::new(),
            langs: files.iter().map(|f| f.lang).collect(),
            workspaces: {
                let mut m = workspace_packages(files);
                m.extend(swift_targets(files));
                m
            },
            declared: declaring_files(files),
            members: receiver_members(files),
            dune: dune_scopes(files),
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
            if f.lang == Lang::Rust
                && matches!(
                    f.path.file_stem().and_then(|s| s.to_str()),
                    Some("lib" | "main")
                )
            {
                idx.crate_roots.entry(comps.clone()).or_insert(i);
            }
            idx.modules.entry(comps).or_insert(i);
            // A package's representative file must be one the
            // production graph keeps. toml's root package sorts
            // bench_test.go first, and standing for the package with a
            // test file dropped every edge into it.
            let dir: Vec<Box<str>> = components(f.path.parent().unwrap_or(Path::new("")));
            if let (Some(last), false) = (dir.last(), f.is_test) {
                idx.dirs.entry(last.clone()).or_default().push((dir, i));
            }
            let full = components(&f.path);
            if let Some(base) = full.last() {
                idx.basenames
                    .entry(base.clone())
                    .or_default()
                    .push((full.clone(), i));
            }
        }
        // Second pass: crate roots are only complete now.
        idx.file_crate_root = nearest_crate_roots(files, &idx.crate_roots);
        idx
    }

    fn classify(&self, i: usize, from: &GraphFacts, imp: &crate::facts::ImportFact) -> Class {
        // A member name is not a module specifier: `named_modules` has
        // already asked the only index that can answer for one, and the
        // component matcher would read `ToString` as a path.
        if imp.reach == crate::lang::Reach::Member {
            return Class::External;
        }
        match self.by_language(i, from, imp) {
            Some(class) => class,
            // Everything else resolves the same way: split the target on
            // whatever this language uses to separate namespace or path
            // components, then match the tail against the scanned files.
            // Only the separators differ.
            //
            //   Lua       `require "a.b.c"` walks package.path
            //   Ruby      `require` searches $LOAD_PATH
            //   Perl      `use Foo::Bar` mirrors the namespace
            //   PHP       `use Foo\\Bar` likewise
            //   the rest  a package or namespace names a directory
            None => self.by_components(from.lang, imp),
        }
    }

    /// The resolver a language states its own dependencies with, where
    /// it has one. `None` hands the target to the component matcher.
    fn by_language(
        &self,
        i: usize,
        from: &GraphFacts,
        imp: &crate::facts::ImportFact,
    ) -> Option<Class> {
        self.by_path(from, imp)
            .or_else(|| self.by_name(i, from, &imp.target))
    }

    /// Languages whose specifier is a PATH, resolved against the file
    /// it names rather than against a namespace.
    fn by_path(&self, from: &GraphFacts, imp: &crate::facts::ImportFact) -> Option<Class> {
        let target = &*imp.target;
        Some(match from.lang {
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => self.web(from, target),
            // An import names a FILE, and the generic arm dropped the
            // extension on one side of the comparison only.
            Lang::Solidity => self.solidity(from, imp),
            Lang::Zig => self.zig(from, target),
            // C++ includes resolve exactly as C's do: a quoted path is
            // relative to the including file, an angled one is a
            // system header and definitionally external.
            Lang::C | Lang::Cpp | Lang::Cuda => self.c(from, target),
            // A sourced path is relative to the script — or assembled
            // at run time from a variable, which resolves to nothing
            // and lands in the honesty bucket where it belongs.
            Lang::Shell => self.shell(from, target),
            // `require_relative` is a path from the requiring file, so
            // `..` climbs a directory rather than naming a namespace,
            // and a sibling means the sibling. Plain `require` searches
            // $LOAD_PATH and falls through.
            Lang::Ruby if imp.reach == crate::lang::Reach::Project => {
                self.ruby_relative(from, target)
            }
            // Resolving a preloaded name by basename bound 37 edges into
            // lua-language-server/meta/template/*.lua — LuaCATS
            // declaration stubs for those very libraries — 25 of them
            // from `require 'ffi'` in kong, and made
            // meta/template/debug.lua the corpus's 4th most load-bearing
            // module at 316 dependents.
            Lang::Lua if crate::lang::preloaded(target) => Class::External,
            _ => return None,
        })
    }

    /// Languages whose specifier is a NAME, resolved against what the
    /// language calls a module rather than against a path.
    fn by_name(&self, i: usize, from: &GraphFacts, target: &str) -> Option<Class> {
        Some(match from.lang {
            Lang::Python => self.python(from, target),
            Lang::Rust => self.rust(i, from, target),
            Lang::Go => self.go(target),
            Lang::Swift => self.swift(target),
            // A module name is CamelCase and its path is snake_case,
            // and nothing bridged the two.
            Lang::Elixir => self.elixir(target),
            // A module IS a file, and the file is not capitalised.
            Lang::OCaml => self.ocaml(i, target),
            // The packs cut a JVM import back to the TYPE, whose file it
            // is. What is left uncut names a PACKAGE — `import a.b.*`,
            // `import cats.data.{X, Y}` — and a package is a directory
            // exactly as a Go package is.
            Lang::Java | Lang::Scala => self.jvm(target),
            _ => return None,
        })
    }

    /// Split the target on whatever separates this language's namespace
    /// or path components, and match the tail against the scanned files.
    fn by_components(&self, lang: Lang, imp: &crate::facts::ImportFact) -> Class {
        let parts: Vec<&str> = imp
            .target
            .split(component_separators(lang))
            .filter(|p| !p.is_empty())
            .collect();
        match self.suffix(&parts) {
            Some(i) => Class::Internal(i),
            // A specifier that named a path, or a member of a namespace
            // this project declares, was SUPPOSED to be here. Calling
            // that a third-party dependency is how a corpus that
            // resolved nothing reported itself fully resolved.
            None => match imp.reach {
                crate::lang::Reach::Project => Class::Unresolved,
                _ => Class::External,
            },
        }
    }

    /// Could an absolute import of `segs` segments have named this file?
    /// Only if the directory `segs` levels above its module path is on
    /// sys.path, and a directory holding `__init__.py` never is: it is
    /// the inside of a package, and code inside a package still spells
    /// an absolute import from the root.
    ///
    /// Fails open on PEP 420 namespace packages, which is the safe
    /// direction.
    fn import_root(&self, i: usize, segs: usize) -> bool {
        let comps = &self.file_comps[i];
        let anchor: PathBuf = comps[..comps.len().saturating_sub(segs)]
            .iter()
            .map(|c| &**c)
            .collect();
        !self.paths.contains_key(&anchor.join("__init__.py"))
    }

    fn python(&self, from: &GraphFacts, target: &str) -> Class {
        if !target.starts_with('.') {
            // Absolute: internal if the dotted path is a module suffix
            // here AND the match is anchored at an import root. A
            // top-level name is ONE segment long, so a bare suffix match
            // reads any same-named file as the target: `import types`
            // landed on starlette/starlette/types.py, `from abc import
            // ABC` at rich/rich/console.py:4 on rich/rich/abc.py. 77 of
            // the gold corpus's 1760 internal Python imports were a
            // stdlib module resolving to project code, and the false
            // attrs -> rich edges merged two unrelated repositories into
            // one 73-module cycle.
            let segs: Vec<&str> = target.split('.').collect();
            let hit = self
                .suffix(&segs)
                .filter(|&i| self.import_root(i, segs.len()));
            return match hit {
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

    /// `require_relative "../utils/replace"`. The extension is normally
    /// left off and is normally `.rb`, but the explicit spelling is
    /// legal too, so both are tried.
    fn ruby_relative(&self, from: &GraphFacts, target: &str) -> Class {
        let joined = normalize(from.path.parent().unwrap_or(Path::new("")), target);
        let mut with_rb = joined.clone().into_os_string();
        with_rb.push(".rb");
        match self
            .paths
            .get(Path::new(&with_rb))
            .or_else(|| self.paths.get(&joined))
        {
            Some(&i) => Class::Internal(i),
            None => Class::Unresolved,
        }
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
            // The standard library is never this project. The bare-root
            // arm drops the leaf and suffix-matches what is left, so
            // `use core::cmp` asked for a module called `core` — which
            // ripgrep's crates/core answers to, giving it 37 dependents
            // in rayon and regex that no Cargo.toml declares.
            "std" | "core" | "alloc" => Class::External,
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
        let key = (Lang::Rust, Box::<str>::from(rest[0]));
        let defined = self.declared.get(&key).and_then(|files: &Vec<usize>| {
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
            return self.workspace(target);
        }
        let joined = normalize(from.path.parent().unwrap_or(Path::new("")), target);
        const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];
        if let Class::Internal(i) = self.web_file(&joined) {
            return Class::Internal(i);
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
        let segs: Vec<&str> = target.split('/').collect();
        if let Class::Internal(i) = self.go_dir(&segs) {
            return Class::Internal(i);
        }
        // A module at major version 2 or above carries `/vN` in its
        // path, and that element names no directory on disk. 35 of chi's
        // 52 self-imports are the bare `github.com/go-chi/chi/v5`, whose
        // last segment is the version: all 35 read as an outside
        // dependency and chi's five root files as depended on by nothing.
        // Stripping is a SECOND attempt, never the first, because
        // `_examples/versions/presenter/v2` is a real directory that the
        // path as written already finds.
        let bare: Vec<&str> = segs.iter().copied().filter(|s| !major_version(s)).collect();
        if bare.len() < segs.len() {
            return self.go_dir(&bare);
        }
        Class::External
    }

    /// The tail of a Go module path maps onto package directories;
    /// unmatched paths are dependencies.
    fn go_dir(&self, segs: &[&str]) -> Class {
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

    /// A Swift module is a TARGET DIRECTORY (`Sources/NIOCore/`) and
    /// never a file, so it resolves the way a Go package does. Matching
    /// a file STEM sent vapor's `import HTTPTypes` — Apple's
    /// swift-http-types, declared in vapor's own Package.swift — to
    /// swift-nio's Sources/NIOHTTP1/HTTPTypes.swift in another
    /// repository, and put its 84 fabricated dependents at the top of
    /// the report. 140 of the 170 stem matches were wrong that way.
    fn swift(&self, target: &str) -> Class {
        // A `path:` in Package.swift beats the convention. Alamofire
        // puts its target in `Source/`, so the only directory named
        // Alamofire is the REPOSITORY ROOT -- and resolving there made
        // every one of its 43 files inherit a dependent from one bogus
        // edge, whitewashing the repository wholesale.
        if let Some(dir) = self.workspaces.get(target) {
            return self.first_under(dir);
        }
        let dir = self
            .dirs
            .get(target)
            .and_then(|c| c.iter().min_by_key(|(comps, _)| comps.len()));
        match dir {
            Some(&(_, i)) => Class::Internal(i),
            None => Class::External,
        }
    }

    /// A representative file of a directory: the first in path order,
    /// which is how a Go package already answers for itself.
    fn first_under(&self, dir: &Path) -> Class {
        match self.paths.iter().filter(|(p, _)| p.starts_with(dir)).min() {
            Some((_, &i)) => Class::Internal(i),
            None => Class::External,
        }
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
        match self.basenames.get(base).and_then(|c| c.first()) {
            Some(&(_, i)) => Class::Internal(i),
            None => Class::Unresolved,
        }
    }

    /// A target is a path from the importing file — except in the build
    /// DSL, where it is a path from the BUILD ROOT: the directory
    /// holding the `build.zig` that owns this file. ghostty spells every
    /// one of its executables that way from `src/build/Ghostty*.zig`,
    /// three directories below its root, and 42 of the corpus's 113
    /// `b.path` targets miss on the file-relative reading alone.
    /// A C header is routed to C's own resolver, because that is what
    /// `@cInclude` names and the include roots `addIncludePath` puts
    /// there are the ones a quoted include already searches. ghostty's
    /// `pkg/freetype/c.zig:2` says `@cInclude("freetype-zig.h")` of the
    /// file beside it and `src/stb/main.zig` the same of two more.
    ///
    /// The routing keys on the TARGET's extension and not on the node,
    /// so the seven `b.path("….h")` sites in ghostty's build files go
    /// the same way — which is what reaches `include/ghostty.h`, not the
    /// `include/` exemption.
    ///
    /// A miss is EXTERNAL, never unresolved. Zig has no angled spelling
    /// to mark a system header with, and 22 of the corpus's 25
    /// `@cInclude` sites name one (`unistd.h`, `errno.h`, `stdlib.h`);
    /// routing a miss through C's quoted arm instead put all 22 into the
    /// honesty bucket, taking the zig corpus from 57 unresolved to 79
    /// for no gain at all.
    fn zig(&self, from: &GraphFacts, target: &str) -> Class {
        if crate::lang::c_header(target) {
            return match self.c(from, target) {
                Class::Unresolved => Class::External,
                bound => bound,
            };
        }
        if !target.ends_with(".zig") {
            return Class::External; // std, builtin, build packages
        }
        let dir = from.path.parent().unwrap_or(Path::new(""));
        if let Some(&i) = self.paths.get(&normalize(dir, target)) {
            return Class::Internal(i);
        }
        let root = dir
            .ancestors()
            .find(|a| self.paths.contains_key(&a.join("build.zig")));
        match root.and_then(|r| self.paths.get(&normalize(r, target))) {
            Some(&i) => Class::Internal(i),
            None => Class::Unresolved,
        }
    }

    /// A quoted include is a path from the including file. An angled
    /// one names a system header — except that a header-only library
    /// includes its OWN headers that way, and flux spells every
    /// internal one `<flux/adaptor/filter.hpp>`.
    ///
    /// An angled include must name two components before it may match
    /// the tree. At one, `<cuda_runtime.h>` binds to transformer-engine's
    /// own util/cuda_runtime.h and `<unistd.h>` to a Windows shim in
    /// llm.c. At two, all 455 distinct targets that fire across the
    /// three corpora name the file they meant.
    fn c(&self, from: &GraphFacts, target: &str) -> Class {
        let angled = target.starts_with('<');
        let path = target.trim_matches(['<', '>']);
        if !angled {
            let joined = normalize(from.path.parent().unwrap_or(Path::new("")), path);
            if let Some(&i) = self.paths.get(&joined) {
                return Class::Internal(i);
            }
        }
        // The include directory is unknowable, so match the whole
        // specifier against file paths instead of just its last segment.
        let segs = segments(path);
        let hit = match (angled, segs.len()) {
            (_, 0) => None,
            (true, 1) => self.include_root_suffix(&segs),
            _ => self.path_suffix(&segs),
        };
        match hit {
            Some(i) => Class::Internal(i),
            None if angled => Class::External,
            None => Class::Unresolved,
        }
    }

    /// Every file an include names where its tail matches several, and
    /// the search path deciding which is not in the source.
    ///
    /// musl's Makefile compiles with `-Iarch/$(ARCH)`, so
    /// `#include <bits/fcntl.h>` means one of eleven real files and the
    /// source cannot say which. 456 of musl's 655 headers had no
    /// includer for that reason -- a claim about the build system
    /// stated as a claim about the code, since every one of them IS
    /// depended on, each in its own configuration.
    ///
    /// So the nearest candidate answers where there IS one: 295 musl
    /// sources write `#include "syscall.h"` and mean the
    /// `src/internal/syscall.h` beside them, not the public
    /// `include/sys/syscall.h`. Where several tie -- every arch is
    /// equally far from `include/fcntl.h` -- they are all candidates,
    /// which manufactures no external dependency because only real
    /// files are ever named. Picking one of a tie would be arbitrary,
    /// and `arch/or1k/crt_arch.h` is what arbitrary looks like.
    ///
    /// Outside musl this is nearly inert: across c, cpp and cuda it
    /// moves three files, all of them correctly.
    fn nearest_includes(&self, from: &GraphFacts, target: &str) -> Vec<usize> {
        let angled = target.starts_with('<');
        let path = target.trim_matches(['<', '>']);
        let dir = from.path.parent().unwrap_or(Path::new(""));
        if !angled && self.paths.contains_key(&normalize(dir, path)) {
            return Vec::new();
        }
        let segs = segments(path);
        if segs.is_empty() {
            return Vec::new();
        }
        let mine = components(&from.path);
        // A bare `<name.h>` may still only mean a file the include
        // convention puts on the search path.
        let rooted = |c: &[Box<str>]| c.len() >= 2 && &*c[c.len() - 2] == "include";
        let hits: Vec<(usize, usize)> = self
            .suffix_matches(&segs)
            .filter(|(c, _)| !(angled && segs.len() == 1) || rooted(c))
            .map(|(c, i)| (shared(&mine, c), *i))
            .collect();
        let Some(&near) = hits.iter().map(|(n, _)| n).max().filter(|_| hits.len() > 1) else {
            return Vec::new();
        };
        hits.iter()
            .filter(|(n, _)| *n == near)
            .map(|&(_, i)| i)
            .collect()
    }

    /// `path_suffix`, narrowed to a file sitting DIRECTLY in a directory
    /// named `include` — C's convention for "this is on the -I path",
    /// and the only evidence in the tree that a bare `<name.h>` could
    /// mean a file of this project. 84 musl sources write
    /// `#include <stdio.h>` and musl/include/stdio.h is right there.
    ///
    /// Across c, cpp and cuda the anchor adds 732 edges over 52 targets
    /// and every one is right: musl's 48 public headers and the umbrella
    /// headers of ctre, flux and immer. Dropping the anchor and admitting
    /// any one-component match makes 131 of them wrong -- transformer-
    /// engine's `<cuda_runtime.h>`, `<math.h>` and `<cudnn.h>` bind to
    /// its own util/ headers, llm.c's `<unistd.h>` to a Windows shim.
    fn include_root_suffix(&self, segs: &[&str]) -> Option<usize> {
        let mut hits = self
            .basenames
            .get(*segs.last()?)?
            .iter()
            .filter(|(c, _)| ends_with(c, segs));
        let (comps, i) = hits.next()?;
        (hits.next().is_none() && comps.len() >= 2 && &*comps[comps.len() - 2] == "include")
            .then_some(*i)
    }

    /// `alias Plug.Conn` names a module, and a module's file is its
    /// name run through Elixir's own `Macro.underscore`:
    /// `Plug.CSRFProtection` lives at `plug/csrf_protection.ex`.
    ///
    /// Comparing the CamelCase name against snake_case path components
    /// missed all 2740 module references in the corpus, so Elixir
    /// reported 31 internal edges and every one of them was JavaScript
    /// under phoenix/assets/js.
    fn elixir(&self, target: &str) -> Class {
        let segs: Vec<String> = target
            .split('.')
            .filter(|s| !s.is_empty())
            .map(crate::lang::underscore)
            .collect();
        let parts: Vec<&str> = segs.iter().map(String::as_str).collect();
        match self.suffix(&parts) {
            Some(i) => Class::Internal(i),
            None => Class::External,
        }
    }

    /// An OCaml module IS a compilation unit, and its name is the file
    /// stem with the first letter capitalised: module `Path` is
    /// `path.ml`, module `CCParse` is `CCParse.ml`. The generic arm
    /// compared `Stdune` against the path component `stdune` and could
    /// not match either spelling — all 2444 OCaml files in the corpus
    /// produced 8 internal edges, and base on its own produced none at
    /// all, so its report carried no architecture section.
    ///
    /// Only the HEAD of a dotted path names a unit: `Memo.O` is the
    /// submodule O inside memo.ml, and `Stdune.Path` reaches stdune's
    /// path.ml through the library's wrapper module, which is the file
    /// the reference actually names. dune spells that same reference
    /// `Stdune__Path`, where the unit is the LAST component instead.
    ///
    /// Which `list.ml` a bare `List.map` means is settled by the dune
    /// library the importing file belongs to, and a dune library is a
    /// directory — so the candidate sharing the longest path prefix
    /// with the importer wins. Taking the shallowest instead sent all
    /// 398 of containers' `List` references to base/src/list.ml, a
    /// different repository.
    fn ocaml(&self, i: usize, target: &str) -> Class {
        // A functor application names its functor (`F(X).t`), and a
        // wrapped-library alias names the module after the separator —
        // both are the build system's spelling, not the language's.
        let head = target
            .split('.')
            .next()
            .unwrap_or_default()
            .split('(')
            .next()
            .unwrap_or_default()
            .trim();
        let Some(unit) = head.rsplit("__").next().filter(|u| !u.is_empty()) else {
            return Class::External;
        };
        let mut rest = unit.chars();
        let lowered: String = match rest.next() {
            Some(first) => first.to_lowercase().chain(rest).collect(),
            None => return Class::External,
        };
        let here = &self.file_comps[i];
        let best = [unit, lowered.as_str()]
            .iter()
            .filter_map(|stem| self.by_last.get(*stem))
            .flatten()
            .filter(|(c, j)| self.langs[*j] == Lang::OCaml && self.dune.visible(c, here))
            .min_by_key(|(c, _)| (std::cmp::Reverse(shared(c, here)), c.len()));
        match best {
            // Ties keep path order, which puts `foo.ml` before
            // `foo.mli`: an edge lands on the implementation.
            Some(&(_, j)) => Class::Internal(j),
            None => Class::External,
        }
    }

    /// A Solidity import names a FILE. `./x.sol` and `../utils/x.sol`
    /// are paths from the importing file, so a miss there is a miss. A
    /// bare specifier is a remapping, and openzeppelin-contracts remaps
    /// `@openzeppelin/contracts/...` onto itself — the table that says
    /// so lives in foundry.toml, so the longest tail naming exactly one
    /// file is taken instead, never below two components.
    fn solidity(&self, from: &GraphFacts, imp: &crate::facts::ImportFact) -> Class {
        let target = &*imp.target;
        let joined = normalize(from.path.parent().unwrap_or(Path::new("")), target);
        if let Some(&i) = self.paths.get(&joined) {
            return Class::Internal(i);
        }
        if imp.reach == crate::lang::Reach::Project {
            return Class::Unresolved;
        }
        let segs = segments(target);
        (2..=segs.len())
            .rev()
            .find_map(|take| self.path_suffix(&segs[segs.len() - take..]))
            .map_or(Class::External, Class::Internal)
    }

    /// A dotted JVM name: the file it names, or the package directory it
    /// names when no file answers to it. 770 Java and 1018 Scala targets
    /// in the gold corpus name a package and no file — an on-demand
    /// import and a selector list both state one.
    fn jvm(&self, target: &str) -> Class {
        let parts: Vec<&str> = target.split('.').filter(|p| !p.is_empty()).collect();
        match self.suffix(&parts).or_else(|| self.dir_suffix(&parts)) {
            Some(i) => Class::Internal(i),
            None => Class::External,
        }
    }

    /// The shallowest package directory whose components these segments
    /// suffix-match, answered by a representative file of it.
    fn dir_suffix(&self, segs: &[&str]) -> Option<usize> {
        self.dirs
            .get(*segs.last()?)?
            .iter()
            .filter(|(c, _)| ends_with(c, segs))
            .min_by_key(|(c, _)| c.len())
            .map(|(_, i)| *i)
    }

    /// A web path with the extension left off, as every specifier in
    /// these languages leaves it: the file itself, then each extension,
    /// then the directory's index.
    ///
    /// The extension is APPENDED, not substituted. `with_extension`
    /// replaces whatever follows the last dot, so
    /// `./authentication.contribution` asked for `authentication.ts` --
    /// a file nobody has -- and vscode's 86 contribution modules read as
    /// depended on by nothing while being imported by name. At least 500
    /// specifiers across the corpus carry a dotted stem: `.gen` 353,
    /// `.test` 54, `.contribution` 14, and `.constants`, `.util`,
    /// `.types` behind them.
    ///
    /// The `.js`-to-`.ts` rewrite nodenext requires is a different
    /// question and genuinely does substitute; it lives in `web`.
    fn web_file(&self, base: &Path) -> Class {
        const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];
        if let Some(&i) = self.paths.get(base) {
            return Class::Internal(i);
        }
        for ext in EXTS {
            let mut named = base.as_os_str().to_os_string();
            named.push(".");
            named.push(ext);
            if let Some(&i) = self
                .paths
                .get(Path::new(&named))
                .or_else(|| self.paths.get(&base.join("index").with_extension(ext)))
            {
                return Class::Internal(i);
            }
        }
        Class::External
    }

    /// A bare specifier naming a package this repository declares.
    /// ariakit writes 1500+ `@ariakit/*` and excalidraw 567
    /// `@excalidraw/element`; all of them read as third-party because a
    /// specifier that is not a path was External by definition.
    ///
    /// The probe is `<dir>/src/<sub>` then `<dir>/<sub>`, and the
    /// `exports` map is deliberately not read: the plain probe finds
    /// 3819 of the 3824 specifiers the full exports lookup finds, and a
    /// resolver-grade manifest parser is not worth five.
    fn workspace(&self, target: &str) -> Class {
        let (name, sub) = split_package(target);
        let Some(dir) = self.workspaces.get(name) else {
            return Class::External;
        };
        let roots = [dir.join("src"), dir.clone()];
        for root in roots {
            let base = sub.iter().fold(root, |p, s| p.join(s));
            if let Class::Internal(i) = self.web_file(&base) {
                return Class::Internal(i);
            }
        }
        Class::External
    }

    /// The submodules an import's bound names denote. `from . import
    /// errors, themes` binds two MODULES, not two symbols, and the
    /// dependency that matters is on themes.py rather than on the
    /// package's __init__.py -- rich's console.py:36 is that line and
    /// themes.py had no other importer in the corpus. attrs names five
    /// submodules in one statement, which is why the first name alone
    /// is not enough.
    fn submodules(
        &self,
        resolved: Option<usize>,
        imp: &crate::facts::ImportFact,
        lang: Lang,
    ) -> Vec<usize> {
        let Some(m) = resolved.filter(|_| lang == Lang::Python) else {
            return Vec::new();
        };
        let base: PathBuf = self.file_comps[m].iter().map(|c| &**c).collect();
        imp.names
            .iter()
            .filter_map(|n| {
                let child = base.join(&**n);
                self.paths
                    .get(&child.with_extension("py"))
                    .or_else(|| self.paths.get(&child.join("__init__.py")))
                    .copied()
            })
            .collect()
    }

    /// Every module one specifier names, where it names more than one:
    /// a directory whose contents are chosen at run time, or a name
    /// whose declaration the language lets you split across files.
    fn named_modules(&self, from: &GraphFacts, imp: &crate::facts::ImportFact) -> Vec<usize> {
        match from.lang {
            // A name a Lua table LISTS is a registry entry, and the one
            // place a bare name written as data can mean anything is
            // beside the file listing it.
            Lang::Lua if imp.reach == crate::lang::Reach::Mention && !imp.target.contains('*') => {
                self.beside(from, &imp.target).into_iter().collect()
            }
            Lang::Lua | Lang::Ruby | Lang::Python => self.glob_modules(from, imp),
            Lang::C | Lang::Cpp | Lang::Cuda => self.nearest_includes(from, &imp.target),
            // A member name answers from the extension-method index
            // alone; see `Reach::Member`.
            // A member name answers from the extension-method index
            // alone; see `Reach::Member`. Deliberately WITHOUT the
            // proximity tie-break `nearest_declarers` makes for a type:
            // C# spreads one extension method's overloads across sibling
            // files and each calls the others, so the calling file is
            // its own nearest declarer and the real edge would go with
            // the self-edge. `CircuitBreakerTResultSyntax.cs:28` calls
            // the `CircuitBreaker` that `CircuitBreakerSyntax.cs:26`
            // declares while declaring a `CircuitBreaker<TResult>` of
            // its own. Measured: the tie-break here costs 63 C# edges
            // and buys 0 orphans in any of the 22 corpora.
            _ if imp.reach == crate::lang::Reach::Member => {
                self.members.get(&imp.target).cloned().unwrap_or_default()
            }
            _ if imp.reach == crate::lang::Reach::Mention => {
                let hits = self
                    .declared
                    .get(&(from.lang, imp.target.clone()))
                    .cloned()
                    .unwrap_or_default();
                self.nearest_declarers(from, hits)
            }
            _ => Vec::new(),
        }
    }

    /// Of the files declaring one name, the ones NEAREST the file that
    /// wrote it — the tie-break `nearest_includes` already makes for a
    /// C header, for the same reason.
    ///
    /// A C# type reference is scoped to an ASSEMBLY, and a scan of the
    /// gold corpus is six unrelated assemblies in one directory. Ten of
    /// its non-test file stems are declared in more than one repository
    /// — `Extensions` in AutoMapper, Dapper, FluentValidation and
    /// Newtonsoft.Json, `Program`, `TypeExtensions`,
    /// `ServiceCollectionExtensions` and four of the .NET polyfill
    /// attributes — and with no tie-break the bare word edged to all of
    /// them. That is 558 of 7823 C# edges, 7.1%, every one naming a
    /// project the referencing file cannot even link against.
    ///
    /// Three files stop being reachable when the fabricated edges go,
    /// and two are then reached TRULY by rules in the same commit:
    /// Dapper's `Extensions.cs` through the generic `CastResult<...>()`
    /// that `called_member` now unwraps, and Newtonsoft's
    /// `VersionConverter.cs` once a test's own nested copy stops
    /// answering. The third is a real finding: Polly's
    /// `DynamicallyAccessedMembersAttribute.cs` was held up entirely by
    /// Newtonsoft.Json, because Polly writes all seven of its own
    /// `[DynamicallyAccessedMembers]` uses on a TYPE PARAMETER and
    /// `sealed()` does not enter a `type_parameter_list`.
    fn nearest_declarers(&self, from: &GraphFacts, hits: Vec<usize>) -> Vec<usize> {
        if from.lang != Lang::CSharp || hits.len() < 2 {
            return hits;
        }
        let mine = components(&from.path);
        let near = |&i: &usize| shared(&mine, &self.file_comps[i]);
        let Some(nearest) = hits.iter().map(near).max() else {
            return hits;
        };
        hits.iter()
            .filter(|i| near(i) == nearest)
            .copied()
            .collect()
    }

    /// The module of this name sitting beside `from`, if one does.
    fn beside(&self, from: &GraphFacts, name: &str) -> Option<usize> {
        let dir = from.path.parent().unwrap_or(Path::new(""));
        let file = dir.join(name);
        self.paths
            .get(&file.with_extension("lua"))
            .or_else(|| self.paths.get(&file.join("init.lua")))
            .copied()
    }

    /// Every module a specifier with a run-time component names.
    ///
    /// `kong/db/schema/plugin_loader.lua:17` writes
    /// `require("kong.plugins." .. plugin .. ".schema")`, and roda
    /// writes the same thing as `require "roda/plugins/#{name}"`. What
    /// the name builds cannot be read from the source, but everything
    /// around it can, so the pack hands over `kong.plugins.*.schema`
    /// and `*` matches exactly one component.
    ///
    /// The part AFTER the star is what makes the Lua case work at all:
    /// nothing sits directly under `kong/plugins`, because each of its
    /// 150 plugins is a directory, so the prefix alone reaches nothing
    /// while `kong.plugins.*.handler` names 35 real files. 417 of gold
    /// Ruby's 490 orphans and 276 of Lua's 342 sit under one of these.
    /// Only real files are ever named, so unlike completing the literal
    /// this manufactures no external dependency.
    fn glob_modules(&self, from: &GraphFacts, imp: &crate::facts::ImportFact) -> Vec<usize> {
        if !matches!(from.lang, Lang::Lua | Lang::Ruby | Lang::Python) {
            return Vec::new();
        }
        let segs: Vec<&str> = imp
            .target
            .split(component_separators(from.lang))
            .filter(|s| !s.is_empty())
            .collect();
        let Some(star) = segs.iter().position(|s| *s == "*") else {
            return Vec::new();
        };
        let (before, after) = (&segs[..star], &segs[star + 1..]);
        if before.is_empty() {
            return Vec::new();
        }
        let rooted = self.glob_root(from, imp, before);
        // One unknown component, so the prefix ends at a known offset
        // from the end and there is exactly one place to look.
        let fits = |c: &[Box<str>]| {
            let Some(k) = c.len().checked_sub(1 + after.len()) else {
                return false;
            };
            rooted(&c[..k]) && c[k + 1..].iter().zip(after).all(|(a, b)| &**a == *b)
        };
        self.file_comps
            .iter()
            .enumerate()
            .filter(|(_, c)| fits(c))
            .map(|(i, _)| i)
            .collect()
    }

    /// The directories such a prefix names, which is a question about
    /// the language's load path.
    ///
    /// Lua's `package.path` holds whole path templates, so a prefix is
    /// matched wherever it appears. Rubygems puts exactly one directory
    /// of a gem on `$LOAD_PATH`, `lib`, so a `require` prefix names a
    /// directory sitting directly under one -- and that is what tells
    /// sequel's own `lib/sequel/adapters/jdbc` apart from what
    /// `require "jdbc/#{name}"` actually loads, which its own line
    /// calls "the necessary JDBC support via a gem". A
    /// `require_relative` prefix is a directory beside the requiring
    /// file, and is not on the load path at all.
    fn glob_root<'a>(
        &self,
        from: &GraphFacts,
        imp: &crate::facts::ImportFact,
        before: &'a [&str],
    ) -> impl Fn(&[Box<str>]) -> bool + use<'a> {
        let lang = from.lang;
        let beside = (lang == Lang::Ruby && imp.reach == crate::lang::Reach::Project).then(|| {
            let base = from.path.parent().unwrap_or(Path::new(""));
            components(&normalize(base, &before.join("/")))
        });
        move |dir: &[Box<str>]| match &beside {
            Some(base) => dir == base.as_slice(),
            None if !ends_with(dir, before) => false,
            // Rubygems puts exactly one directory of a gem on
            // $LOAD_PATH, so a `require` prefix names a directory
            // directly under one.
            None => {
                lang != Lang::Ruby
                    || dir[..dir.len() - before.len()]
                        .last()
                        .is_none_or(|d| &**d == "lib")
            }
        }
    }

    /// The one file whose path ends with these segments. `None` when
    /// none does, and equally when several do: which of them a
    /// specifier means is decided by a search path or a remapping
    /// table that is not in the source.
    fn path_suffix(&self, segs: &[&str]) -> Option<usize> {
        let mut hits = self.suffix_matches(segs);
        let &(_, i) = hits.next()?;
        hits.next().is_none().then_some(i)
    }

    /// Every file whose path ends with these segments, with its own
    /// path components — what says how near it is to the includer.
    fn suffix_matches<'a>(&'a self, segs: &'a [&str]) -> impl Iterator<Item = &'a CompEntry> + 'a {
        segs.last()
            .and_then(|last| self.basenames.get(*last))
            .into_iter()
            .flatten()
            .filter(move |(c, _)| ends_with(c, segs))
    }

    /// The shallowest module whose component vector the segments
    /// suffix-match: a search path finds `log` before `vendor/log`.
    fn suffix(&self, segs: &[&str]) -> Option<usize> {
        let last = segs.last()?;
        self.by_last
            .get(*last)?
            .iter()
            .filter(|(c, _)| ends_with(c, segs))
            .min_by_key(|(c, _)| c.len())
            .map(|(_, i)| *i)
    }
}

/// Which files declare each exported symbol, for the two languages
/// that resolve a name by its declaration rather than by where it sits.
fn declaring_files(files: &[GraphFacts]) -> HashMap<(Lang, Box<str>), Vec<usize>> {
    let mut out: HashMap<(Lang, Box<str>), Vec<usize>> = HashMap::new();
    let named = |f: &GraphFacts| matches!(f.lang, Lang::Rust | Lang::CSharp);
    // A TEST file's declaration must not answer for a production name,
    // for the reason `dirs` already skips one: production code cannot
    // depend on a test, so the edge lands on a file the production
    // graph then drops and the real target keeps its zero. Polly
    // declares `public static class Constants` in
    // `test/Polly.Specs/Helpers` and `internal static class Constants`
    // in `src/Polly.Core/Utils`.
    //
    // Load-bearing only once `nearest_declarers` is in: while every
    // declarer answered, the production one answered too and the filter
    // was worth nothing. With the proximity tie-break a test's copy can
    // WIN — it is 12 orphans and 7276 C# edges with this filter and 13
    // and 7265 without.
    for (i, f) in files
        .iter()
        .enumerate()
        .filter(|(_, f)| named(f) && !f.is_test)
    {
        let stem = csharp_stem(f).filter(|s| !f.exports.contains(s));
        for sym in f.exports.iter().chain(stem.iter()) {
            out.entry((f.lang, sym.clone())).or_default().push(i);
        }
    }
    out
}

/// Which files declare each extension method, so a call written on a
/// VALUE can find them. A test's declaration is skipped for the reason
/// `declaring_files` skips one.
fn receiver_members(files: &[GraphFacts]) -> HashMap<Box<str>, Vec<usize>> {
    let mut out: HashMap<Box<str>, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate().filter(|(_, f)| !f.is_test) {
        for name in &f.receiver_units {
            out.entry(name.clone()).or_default().push(i);
        }
    }
    out
}

/// The type a C# file's NAME declares, which its exports need not.
///
/// `internal` is assembly-wide, so an internal type is referenced from
/// other files exactly as a public one is — and `exports` holds the
/// public surface, by design, so nothing in the index answered for
/// `Polly.Utils.Constants` or `Newtonsoft.Json.Utilities.
/// DynamicallyAccessedMemberTypes`. The file name is the statement that
/// survives the accessibility modifier, and it is the rule the pack's
/// own resolver docstring states: 907 of the gold corpus's 962
/// production C# files are named after the type they declare.
///
/// The FIRST dot-segment, because that is where a `partial` type's
/// continuation files put it: `JsonReader.Async.cs` continues
/// `JsonReader`, and its class header carries a `#if` in the base-list
/// position that costs the declaration its name in the parse.
fn csharp_stem(f: &GraphFacts) -> Option<Box<str>> {
    if f.lang != Lang::CSharp {
        return None;
    }
    let name = f.path.file_name()?.to_str()?;
    let stem = name.split('.').next()?;
    stem.starts_with(char::is_uppercase).then(|| stem.into())
}

/// Leading components two files share — how near one is to the other.
fn shared(a: &[Box<str>], b: &[Box<str>]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

impl DuneScopes {
    /// Can a module name written bare reach this file? A dune library is
    /// a DIRECTORY, so its modules see each other by name; every other
    /// library is reached through its root module, the file dune names
    /// after the library and its users spell `open Stdune`.
    ///
    /// A name that answers from neither is the standard library wearing
    /// a name this project also uses, or another library's private
    /// module. otherlibs/dyn/dyn.ml says `Float.to_string` and means
    /// Stdlib's; binding it to otherlibs/stdune/src/float.ml put 697 of
    /// dune's 886 modules into a single cycle, and OCaml has no such
    /// thing — a cycle between compilation units does not compile, and
    /// dune refuses to build one.
    ///
    /// The two stanzas widen exactly that, and only where the build file
    /// says so. A flat library is ONE directory spelled over several:
    /// 19 orphans and 627 edges, and the file-level cycle mass and
    /// largest cycle do not move at all — the directory cycle it does
    /// create has all six members inside `dune/src/dune_rules`, which is
    /// the single library `(include_subdirs unqualified)` flattens. An
    /// unwrapped library publishes every module under its own bare name:
    /// no orphan moves for it, but it earns 64 edges and reclassifies
    /// 1050 imports from external to internal, and every unwrapped
    /// library in gold uses a prefixed namespace (`CC*`, `cmdliner_*`,
    /// `opam*`, `lwd*`, `sha*`, `notty*`) so the "a common stem captures
    /// an unrelated bare reference" hazard has no instance here.
    fn visible(&self, cand: &[Box<str>], here: &[Box<str>]) -> bool {
        let (Some((stem, dir)), Some((_, own))) = (cand.split_last(), here.split_last()) else {
            return false;
        };
        // `<lib>/<lib>.ml` and `<lib>/src/<lib>.ml` are both how a
        // library names its root module.
        dir == own
            || dir.iter().rev().take(2).any(|d| d == stem)
            || self.unwrapped.contains(dir)
            || self
                .flat
                .iter()
                .any(|root| dir.starts_with(root) && own.starts_with(root))
    }
}

/// Which directories the `dune` files beside the scanned OCaml modules
/// declare flat or unwrapped. Only the ancestors of OCaml files are
/// probed, so a repository without any pays nothing.
fn dune_scopes(files: &[GraphFacts]) -> DuneScopes {
    let mut seen: HashSet<&Path> = HashSet::new();
    let mut out = DuneScopes::default();
    for f in files.iter().filter(|f| f.lang == Lang::OCaml) {
        for dir in f.path.ancestors().skip(1) {
            if !seen.insert(dir) {
                break;
            }
            let Ok(text) = std::fs::read_to_string(dir.join("dune")) else {
                continue;
            };
            let comps = components(dir);
            if text.contains("(include_subdirs unqualified)") {
                out.flat.push(comps.clone());
            }
            if text.contains("(wrapped false)") {
                out.unwrapped.insert(comps);
            }
        }
    }
    out
}

/// Go writes a major version into the module path from v2 on, so `v1`
/// and `v0` are never spelled and a `v1` element is an ordinary
/// directory — chi has three of them under `_examples/versions/`.
fn major_version(seg: &str) -> bool {
    seg.strip_prefix('v')
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| n >= 2)
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
        // Penlight writes the rule down at run.lua:37 —
        // `package.path = "lua/?.lua;lua/?/init.lua"`.
        Lang::Lua => "init",
        _ => "",
    };
    let crate_root =
        f.lang == Lang::Rust && matches!(comps.last().map(|c| &**c), Some("lib" | "main"));
    if comps.last().is_some_and(|c| &**c == dir_marker) || crate_root {
        comps.pop();
    }
    comps
}

/// Each file's nearest enclosing crate root: the longest prefix of its
/// module components that a `lib.rs` or `main.rs` answers to.
fn nearest_crate_roots(
    files: &[GraphFacts],
    roots: &HashMap<Vec<Box<str>>, usize>,
) -> Vec<Option<usize>> {
    files
        .iter()
        .map(|f| {
            let comps = module_components(f);
            (0..=comps.len())
                .rev()
                .find_map(|n| roots.get(&comps[..n]).copied())
        })
        .collect()
}

/// `@scope/name/sub/path` -> ("@scope/name", ["sub", "path"]); an
/// unscoped `name/sub` -> ("name", ["sub"]).
fn split_package(target: &str) -> (&str, Vec<&str>) {
    // A scoped package spends two segments on its name, an unscoped one.
    let take = if target.starts_with('@') { 2 } else { 1 };
    let mut cut = target.len();
    let mut seen = 0;
    for (at, c) in target.char_indices() {
        seen += usize::from(c == '/');
        if seen == take {
            cut = at;
            break;
        }
    }
    let rest = target.get(cut + 1..).unwrap_or("");
    let sub = rest.split('/').filter(|s| !s.is_empty()).collect();
    (&target[..cut], sub)
}

/// Every SwiftPM target whose manifest moves it off the
/// `Sources/<name>` convention, mapped to the directory it names.
/// Alamofire declares `.target(name: "Alamofire", path: "Source")`, and
/// swift-nio seven more.
///
/// Read textually rather than parsed: a manifest is Swift, and the two
/// fields wanted are adjacent literals inside one `.target(` call.
fn swift_targets(files: &[GraphFacts]) -> HashMap<Box<str>, PathBuf> {
    let mut seen: HashSet<&Path> = HashSet::new();
    let mut out = HashMap::new();
    for f in files.iter().filter(|f| f.lang == Lang::Swift) {
        for dir in f.path.ancestors().skip(1) {
            if !seen.insert(dir) {
                break;
            }
            let Ok(text) = std::fs::read_to_string(dir.join("Package.swift")) else {
                continue;
            };
            for call in text.split(".target(").skip(1) {
                let Some((name, path)) = target_path(call) else {
                    continue;
                };
                out.insert(name.into(), dir.join(path));
            }
        }
    }
    out
}

/// The `name:` and `path:` literals of one target declaration, when it
/// carries both and neither is separated from it by the next `)`.
fn target_path(call: &str) -> Option<(&str, &str)> {
    let head = call.split_once(')').map_or(call, |(h, _)| h);
    Some((quoted_after(head, "name:")?, quoted_after(head, "path:")?))
}

/// The string literal following `field` in a manifest fragment.
fn quoted_after<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let rest = text.split_once(field)?.1;
    rest.split_once('"')?.1.split_once('"').map(|(v, _)| v)
}

/// Every package name a `package.json` beside the scanned files
/// declares, mapped to the directory holding it. Only the ancestors of
/// web-language files are probed, so a repository without any pays
/// nothing.
fn workspace_packages(files: &[GraphFacts]) -> HashMap<Box<str>, PathBuf> {
    let mut seen: HashSet<&Path> = HashSet::new();
    let mut out = HashMap::new();
    for f in files {
        if !matches!(f.lang, Lang::TypeScript | Lang::Tsx | Lang::JavaScript) {
            continue;
        }
        for dir in f.path.ancestors().skip(1) {
            if !seen.insert(dir) {
                break;
            }
            let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
                continue;
            };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if let Some(name) = json.get("name").and_then(|n| n.as_str()) {
                out.entry(Box::<str>::from(name))
                    .or_insert_with(|| dir.to_path_buf());
            }
            for (name, dir) in subpath_imports(&json, dir) {
                out.entry(name).or_insert(dir);
            }
        }
    }
    out
}

/// Node's own private-name mechanism: a `package.json` may map `#app/*`
/// onto `./src/*`, and a specifier starting `#` is resolvable ONLY
/// through that map. ariakit writes 313 of them, every one of which
/// read as a third-party package.
///
/// The name and the directory are exactly what a workspace package
/// contributes, so a subpath entry joins the same map: `#app` is a
/// package whose root happens to be `src`.
fn subpath_imports(json: &serde_json::Value, dir: &Path) -> Vec<(Box<str>, PathBuf)> {
    let Some(map) = json.get("imports").and_then(|i| i.as_object()) else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(key, value)| {
            let root = key.strip_suffix("/*")?;
            let target = value.as_str()?.strip_suffix("/*")?;
            Some((root.into(), normalize(dir, target)))
        })
        .collect()
}

/// A path specifier's components, with the segments that name nothing
/// dropped.
fn segments(target: &str) -> Vec<&str> {
    target
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
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
                reach: crate::lang::Reach::Anywhere,
            })
            .collect(),
        exports: Vec::new(),
        receiver_units: Vec::new(),
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
    fn a_specifier_that_named_this_project_reports_a_miss_as_a_miss() {
        // A corpus that resolved NOTHING used to report itself 100%
        // resolved: the generic arm could only answer Internal or
        // External, so every in-repo `require_relative` that failed to
        // land read as a third-party dependency. roda showed 41
        // internal / 296 external / 0 unresolved while 161 of those 296
        // named files inside itself.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str, reach| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach,
        };
        let mut f = file(Lang::Ruby, "lib/app/core.rb", &[]);
        f.imports = vec![
            imp("nowhere/at/all", Reach::Project),
            imp("nowhere/at/all", Reach::Anywhere),
        ];
        let files = [f, file(Lang::Ruby, "lib/app/other.rb", &[])];
        let (res, _) = super::resolve_imports(&files);
        assert_eq!(
            (res.internal, res.external, res.unresolved),
            (0, 1, 1),
            "a path that named this project is unresolved; a package name is external"
        );
        assert!(
            res.rate() < 1.0,
            "a run that resolved nothing cannot report success"
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
    fn an_include_lands_on_the_header_its_path_names() {
        // cutlass holds seven files called gemm.h. Matching an include
        // by its last segment sent `cutlass/gemm/gemm.h` to whichever
        // came first in path order — the one under device/ — so the
        // header the include actually names had no fan-in at all.
        let files = [
            file(Lang::Cuda, "include/cutlass/gemm/device/gemm.h", &[]),
            file(Lang::Cuda, "include/cutlass/gemm/gemm.h", &[]),
            file(Lang::Cuda, "include/cutlass/gemm/kernel/gemm.h", &[]),
            file(Lang::Cuda, "main.cu", &["cutlass/gemm/gemm.h"]),
        ];
        let (_, e) = edges(&files);
        assert_eq!(e, [(3, 1)]);
    }

    #[test]
    fn a_package_is_represented_by_a_file_the_graph_keeps() {
        // A Go package's edges land on a representative file of it, and
        // the representative was whichever sorted first. toml's root
        // package sorts bench_test.go first, so the production graph
        // dropped the representative and every edge into the package
        // with it: 16 of toml's 22 modules read as orphans.
        let mut bench = file(Lang::Go, "toml/bench_test.go", &[]);
        bench.is_test = true;
        let files = [
            bench,
            file(
                Lang::Go,
                "toml/cmd/tomlv/main.go",
                &["github.com/BurntSushi/toml"],
            ),
            file(Lang::Go, "toml/decode.go", &[]),
        ];
        let (_, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[1][0], Some(idx("toml/decode.go")));
    }

    #[test]
    fn a_swift_import_names_a_target_directory_not_a_file() {
        // vapor's `import HTTPTypes` is Apple's swift-http-types, which
        // vapor's own Package.swift declares. Matching a file STEM sent
        // it to swift-nio's Sources/NIOHTTP1/HTTPTypes.swift — another
        // repository entirely — and made those 84 fabricated dependents
        // the top line of the Swift report. 140 of the 170 stem matches
        // were wrong the same way.
        let files = [
            file(Lang::Swift, "swift-nio/Sources/NIOCore/Channel.swift", &[]),
            file(
                Lang::Swift,
                "swift-nio/Sources/NIOHTTP1/HTTPTypes.swift",
                &["NIOCore"],
            ),
            file(
                Lang::Swift,
                "vapor/Sources/Vapor/Request.swift",
                &["HTTPTypes"],
            ),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(
            targets[1][0],
            Some(idx("Sources/NIOCore/Channel.swift")),
            "a module is the directory that holds it"
        );
        assert_eq!(targets[2][0], None, "and never a file that shares its name");
        assert_eq!(res.external, 1);
    }

    #[test]
    fn an_angled_include_can_name_the_projects_own_header() {
        // A header-only library includes its own headers the angled
        // way: flux spells every one `<flux/core.hpp>`, so 93 headers
        // read as 149 modules with 32 edges and 97% deletable.
        //
        // One component has to stay external. `<cuda_runtime.h>` is the
        // toolkit header 109 times over, and admitting it would bind to
        // transformer-engine's own util/cuda_runtime.h — a different
        // file that happens to share a name.
        let files = [
            file(
                Lang::Cpp,
                "include/flux/adaptor/filter.hpp",
                &["<flux/core.hpp>", "<cassert>"],
            ),
            file(Lang::Cpp, "include/flux/core.hpp", &[]),
            file(Lang::Cuda, "util/cuda_runtime.h", &[]),
            file(Lang::Cuda, "util/kernel.cu", &["<cuda_runtime.h>"]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[0][0], Some(idx("include/flux/core.hpp")));
        assert_eq!(targets[0][1], None, "<cassert> is a system header");
        assert_eq!(targets[3][0], None, "one component means the toolkit");
        assert_eq!(res.external, 2);
    }

    #[test]
    fn an_include_several_headers_answer_to_names_the_nearest_or_all_of_them() {
        // musl carries eighteen copies of syscall_arch.h, one per
        // architecture, and compiles with `-Iarch/$(ARCH)`. WHICH one a
        // build sees is still nowhere in the source — but reporting
        // that as a failure to resolve left 456 of musl's 655 headers
        // with no includer, which states a fact about the build system
        // as though it were one about the code. Every one of them IS
        // depended on, each in its own configuration.
        //
        // So a tie names them all, and manufactures no external
        // dependency because only real files are ever named. Where one
        // candidate is strictly nearer it answers alone: 295 musl
        // sources write `#include "syscall.h"` meaning the
        // src/internal/syscall.h beside them, not the public
        // include/sys/syscall.h. Picking one of a TIE would be
        // arbitrary, and `arch/or1k/crt_arch.h` is what arbitrary looks
        // like.
        let files = [
            file(Lang::C, "arch/aarch64/syscall_arch.h", &[]),
            file(Lang::C, "arch/x86_64/syscall_arch.h", &[]),
            file(Lang::C, "include/sys/syscall.h", &[]),
            file(Lang::C, "src/internal/syscall.h", &[]),
            file(
                Lang::C,
                "src/unistd/read.c",
                &["syscall_arch.h", "syscall.h"],
            ),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[4][1], Some(idx("src/internal/syscall.h")));
        let arches = [
            idx("arch/aarch64/syscall_arch.h"),
            idx("arch/x86_64/syscall_arch.h"),
        ];
        for a in arches {
            assert!(targets[4].contains(&Some(a)), "every arch is depended on");
        }
        // Two includes, two dependencies: the extra arch rides past the
        // row without being tallied twice.
        assert_eq!((res.internal, res.external, res.unresolved), (2, 0, 0));
    }

    #[test]
    fn a_zig_binding_names_the_c_header_it_wraps() {
        // `@cInclude` is Zig's import statement for a C header, and a
        // binding module is where it names the header beside it:
        // ghostty's pkg/freetype/c.zig:2 says
        // `@cInclude("freetype-zig.h")`. A MISS is external and never
        // unresolved — Zig has no angled spelling to mark a system
        // header with, and 22 of the corpus's 25 @cInclude sites name
        // one, so routing a miss through C's quoted arm put all 22 into
        // the honesty bucket for no gain.
        let files = [
            file(
                Lang::Zig,
                "pkg/freetype/c.zig",
                &["freetype-zig.h", "stdio.h"],
            ),
            file(Lang::C, "pkg/freetype/freetype-zig.h", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        assert_eq!(targets[0][0], Some(1), "the header beside it");
        assert_eq!(targets[0][1], None);
        assert_eq!(
            (res.internal, res.external, res.unresolved),
            (1, 1, 0),
            "a system header is external, not a failure to resolve"
        );
    }

    #[test]
    fn a_dune_stanza_widens_where_a_bare_module_name_answers() {
        // `(include_subdirs unqualified)` folds every SUBDIRECTORY of a
        // library into one flat namespace, so dune_rules/gen_rules.ml
        // writing `Cram_rules.rules` means dune_rules/cram/cram_rules.ml.
        // `(wrapped false)` publishes each module under its own bare
        // name, which is why containers/tests/data/t_bv.ml can say
        // `open CCBV` with no qualification at all. 49 orphans and 691
        // edges between them, and the file-level cycle mass does not
        // move: an illegal compilation-unit cycle is the failure mode
        // `visible` exists to prevent.
        let comps = |p: &str| -> Vec<Box<str>> { p.split('/').map(Box::from).collect() };
        let mut scopes = DuneScopes::default();
        scopes.flat.push(comps("dune/src/dune_rules"));
        scopes.unwrapped.insert(comps("containers/src/data"));
        let here = comps("dune/src/dune_rules/gen_rules");
        let cand = comps("dune/src/dune_rules/cram/cram_rules");
        assert!(scopes.visible(&cand, &here), "one flattened library");
        let bv = comps("containers/src/data/CCBV");
        let test = comps("containers/tests/data/t_bv");
        assert!(scopes.visible(&bv, &test), "an unwrapped module is global");
        // Neither stanza reaches a directory that declares neither, and
        // the flat scope does not leave the library that declared it.
        let float = comps("dune/otherlibs/stdune/src/float");
        let dyn_ml = comps("dune/otherlibs/dyn/dyn");
        assert!(
            !scopes.visible(&float, &dyn_ml),
            "Float.to_string means Stdlib's"
        );
        assert!(
            !DuneScopes::default().visible(&cand, &here),
            "without the stanza the subdirectory is another library"
        );
    }

    #[test]
    fn an_elixir_module_name_is_its_path_underscored() {
        // `defmodule Plug.Conn` lives at lib/plug/conn.ex. Comparing
        // the CamelCase name against snake_case path components missed
        // all 2740 module references in the corpus, which reported 31
        // internal edges — every one of them JavaScript under
        // phoenix/assets/js.
        //
        // Absinthe holds KnownDirectives twice, under document and
        // under schema, so the full segment vector has to decide which;
        // the last segment alone cannot.
        let files = [
            file(
                Lang::Elixir,
                "lib/absinthe/phase/document/validation/known_directives.ex",
                &[],
            ),
            file(
                Lang::Elixir,
                "lib/absinthe/phase/schema/validation/known_directives.ex",
                &[],
            ),
            file(Lang::Elixir, "lib/plug/conn.ex", &[]),
            file(
                Lang::Elixir,
                "lib/plug/csrf_protection.ex",
                &[
                    "Plug.Conn",
                    "Absinthe.Phase.Schema.Validation.KnownDirectives",
                    "Ecto.Query",
                ],
            ),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[3][0], Some(idx("lib/plug/conn.ex")));
        assert_eq!(
            targets[3][1],
            Some(idx("schema/validation/known_directives.ex"))
        );
        assert_eq!(targets[3][2], None, "Ecto is a dependency here");
        assert_eq!(res.external, 1);
    }

    #[test]
    fn a_solidity_import_names_a_file_extension_and_all() {
        // The generic arm split "../utils/Context.sol" into segments
        // and matched them against module components, which have the
        // extension stripped — present on one side, absent on the
        // other, and `..` folded on neither. A guaranteed miss:
        // openzeppelin's 367 contracts produced zero internal edges and
        // no architecture section at all.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str, reach| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach,
        };
        let mut ownable = file(Lang::Solidity, "contracts/access/Ownable.sol", &[]);
        ownable.imports = vec![
            imp("../utils/Context.sol", Reach::Project),
            imp(
                "@openzeppelin/contracts/utils/math/Math.sol",
                Reach::Anywhere,
            ),
            imp("forge-std/Test.sol", Reach::Anywhere),
            imp("../utils/Gone.sol", Reach::Project),
        ];
        let files = [
            ownable,
            file(Lang::Solidity, "contracts/utils/Context.sol", &[]),
            file(Lang::Solidity, "contracts/utils/math/Math.sol", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[0][0], Some(idx("contracts/utils/Context.sol")));
        // A remapping onto the project itself, found by its longest
        // unique tail, since foundry.toml is not in the source.
        assert_eq!(targets[0][1], Some(idx("contracts/utils/math/Math.sol")));
        assert_eq!(targets[0][2], None, "forge-std is a real dependency");
        assert_eq!(targets[0][3], None, "a path that names nothing is a miss");
        assert_eq!((res.internal, res.external, res.unresolved), (2, 1, 1));
    }

    #[test]
    fn a_ruby_relative_require_is_a_path_from_the_requiring_file() {
        // `require_relative` was suffix-matched on '/', so `..` — never
        // a component of a namespace — could not resolve at all, and a
        // bare sibling landed on a namesake: sequel's lib/sequel/core.rb
        // reached lib/sequel/dataset/sql.rb where Ruby loads
        // lib/sequel/sql.rb.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let rel = |target: &str| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach: Reach::Project,
        };
        let mut sqlite = file(Lang::Ruby, "lib/sequel/adapters/shared/sqlite.rb", &[]);
        sqlite.imports = vec![rel("../utils/replace")];
        let mut core = file(Lang::Ruby, "lib/sequel/core.rb", &[]);
        core.imports = vec![rel("sql"), rel("dataset/sql.rb")];
        let files = [
            sqlite,
            file(Lang::Ruby, "lib/sequel/adapters/utils/replace.rb", &[]),
            core,
            file(Lang::Ruby, "lib/sequel/dataset/sql.rb", &[]),
            file(Lang::Ruby, "lib/sequel/sql.rb", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        assert_eq!(res.unresolved, 0, "all three name files in this tree");
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[0][0], Some(idx("adapters/utils/replace.rb")));
        assert_eq!(targets[2][0], Some(idx("lib/sequel/sql.rb")));
        // The extension is normally left off, and legal when written.
        assert_eq!(targets[2][1], Some(idx("dataset/sql.rb")));
    }

    #[test]
    fn a_php_class_named_inline_is_reached_without_being_counted() {
        // `use` covers only the classes of ANOTHER namespace. One in the
        // file's own namespace is named bare, and one BELOW it is
        // written out: PHP-Parser spells `Comment\Doc`, `Lexer\Emulative`
        // and `Builder\Class_` that way and imports none of them, which
        // is why 181 of its 274 modules read as orphans while only 4 are
        // never named by another production file.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str, reach| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach,
        };
        let mut parser = file(Lang::Php, "lib/PhpParser/Parser.php", &[]);
        parser.imports = vec![
            imp("Node\\Stmt", Reach::Project),
            imp("Comment\\Doc", Reach::Mention),
            imp("Runtime\\Missing", Reach::Mention),
        ];
        let files = [
            parser,
            file(Lang::Php, "lib/PhpParser/Node/Stmt.php", &[]),
            file(Lang::Php, "lib/PhpParser/Comment/Doc.php", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[0][0], Some(idx("Node/Stmt.php")));
        assert_eq!(targets[0][1], Some(idx("Comment/Doc.php")));
        // Only the `use` statement is a dependency the file declares.
        // The name it merely writes down is neither internal nor
        // external, and the one naming nothing here is not a failure to
        // resolve — an inline name is not a promise that the file is
        // present.
        assert_eq!((res.internal, res.external, res.unresolved), (1, 0, 0));
    }

    #[test]
    fn a_csharp_type_reference_reaches_what_declares_it_and_states_no_dependency() {
        // A `using` opens a namespace and binds no file: it makes short
        // names visible and nothing more, and a type in the file's own
        // namespace needs none at all. The corpus's 7032 directives
        // resolved 23 edges, and 2025 of 2032 modules read as orphans.
        //
        // The TYPE is what names another file, and where it lives is
        // said by the declaration and by nothing else — a `partial`
        // class is spread over `SqlMapper.cs`, `SqlMapper.TypeHandler.cs`
        // and eleven more, and 87 of the corpus's 102 dotted-stem files
        // are one.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str, reach| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach,
        };
        let mut execute = file(Lang::CSharp, "src/Dapper/Execute.cs", &[]);
        execute.imports = vec![
            imp("System.Threading", Reach::Anywhere),
            imp("SqlMapper", Reach::Mention),
            imp("Task", Reach::Mention),
        ];
        let mut whole = file(Lang::CSharp, "src/Dapper/SqlMapper.cs", &[]);
        whole.exports = vec!["SqlMapper".into()];
        let mut part = file(Lang::CSharp, "src/Dapper/SqlMapper.TypeHandler.cs", &[]);
        part.exports = vec!["SqlMapper".into()];
        let files = [execute, whole, part];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert!(targets[0].contains(&Some(idx("SqlMapper.cs"))));
        assert!(
            targets[0].contains(&Some(idx("SqlMapper.TypeHandler.cs"))),
            "a stem match cannot reach the other half of a partial type"
        );
        // Neither the reference that resolved nor the framework name
        // that did not touches the tally: the one using directive is
        // the only dependency this file STATES, so `imports_external`
        // keeps counting modules from outside rather than every type
        // name the compiler found somewhere else.
        assert_eq!((res.internal, res.external, res.unresolved), (0, 1, 0));
    }

    #[test]
    fn an_extension_method_is_reached_through_the_value_it_is_called_on() {
        // `policyBuilder.CircuitBreaker(n, t)` names no type at all: the
        // declaring class is spelled nowhere in the call, and the member
        // is the only handle on the file. 40 of gold C#'s 86 orphans
        // declare nothing but extension methods, all eight of Polly's
        // `*Syntax.cs` among them.
        //
        // It answers from its OWN index, so a call on a value can never
        // land on a type of the same name, and a test's declaration is
        // skipped for the reason `declaring_files` skips one.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str, reach| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach,
        };
        let mut caller = file(Lang::CSharp, "src/Polly/Retry.cs", &[]);
        caller.imports = vec![
            imp("CircuitBreaker", Reach::Member),
            imp("ToString", Reach::Member),
            imp("IgnoreAttribute", Reach::Mention),
        ];
        let mut syntax = file(Lang::CSharp, "src/Polly/CircuitBreakerSyntax.cs", &[]);
        syntax.receiver_units = vec!["CircuitBreaker".into()];
        // A namesake TYPE is not what a member call reaches.
        let mut namesake = file(Lang::CSharp, "src/Polly/CircuitBreaker.cs", &[]);
        namesake.exports = vec!["CircuitBreaker".into()];
        // `[Ignore]` is the short form the compiler completes.
        let mut attr = file(Lang::CSharp, "src/Polly/IgnoreAttribute.cs", &[]);
        attr.exports = vec!["IgnoreAttribute".into()];
        let files = [caller, syntax, namesake, attr];
        let (_, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert!(targets[0].contains(&Some(idx("CircuitBreakerSyntax.cs"))));
        assert!(
            !targets[0].contains(&Some(idx("CircuitBreaker.cs"))),
            "a member call must not land on a type of the same name"
        );
        assert!(targets[0].contains(&Some(idx("IgnoreAttribute.cs"))));
        // A member nothing declares as an extension resolves to nothing
        // rather than to a path that happens to end that way.
        assert_eq!(targets[0].iter().filter(|t| t.is_some()).count(), 2);
    }

    #[test]
    fn a_csharp_file_name_declares_a_type_and_the_nearest_assembly_answers() {
        // `internal` is assembly-wide, so an internal type is referenced
        // by name exactly as a public one is — and `exports` is the
        // PUBLIC surface by design, so nothing answered for
        // `Polly.Utils.Constants` or `Newtonsoft.Json.Utilities.
        // DynamicallyAccessedMemberTypes`. The file NAME is the
        // statement that survives the accessibility modifier: 907 of the
        // gold corpus's 962 production C# files carry the name of the
        // type they declare.
        //
        // A scan is several assemblies, and 13 of the corpus's stems are
        // declared in more than one repository — `Extensions` in
        // AutoMapper, Dapper, FluentValidation and Newtonsoft.Json alike.
        // With no tie-break the bare word edged to all four, 661 of 7823
        // C# edges naming a project the caller cannot link against.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let mention = |target: &str| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach: Reach::Mention,
        };
        let mut caller = file(Lang::CSharp, "Dapper/src/SqlMapper.cs", &[]);
        caller.imports = vec![mention("JsonReader"), mention("Extensions")];
        // `JsonReader.Async.cs` continues `public abstract partial class
        // JsonReader` and carries a `#if` in the base-list position that
        // costs the declaration its name in the parse. Only the FIRST
        // dot-segment answers: no path component spells `JsonReader`,
        // because the basename is `JsonReader.Async`, so the component
        // matcher cannot reach it and 49 production files mention it.
        let partial = file(Lang::CSharp, "Dapper/src/JsonReader.Async.cs", &[]);
        // Neither declarer states its name in `exports`: one is
        // `internal`, the other a `partial` continuation.
        let near = file(Lang::CSharp, "Dapper/src/Extensions.cs", &[]);
        let far = file(Lang::CSharp, "AutoMapper/src/Extensions.cs", &[]);
        // And a TEST's declaration must not answer even when it IS the
        // nearest: Newtonsoft declares `VersionConverter` once in
        // `Src/Newtonsoft.Json/Converters/` and once, nested in a
        // documentation sample, under `Src/Newtonsoft.Json.Tests/`.
        // `VersionConverterTests.cs` sits beside the second, so the
        // proximity tie-break hands it the sample and the production
        // converter keeps its zero — 12 orphans with this filter and 13
        // without.
        let mut suite = file(
            Lang::CSharp,
            "Json/Tests/Converters/VersionConverterTests.cs",
            &[],
        );
        suite.is_test = true;
        suite.imports = vec![mention("VersionConverter")];
        let mut sample = file(Lang::CSharp, "Json/Tests/Samples/CustomConverter.cs", &[]);
        sample.is_test = true;
        sample.exports = vec!["VersionConverter".into()];
        let prod = file(Lang::CSharp, "Json/Src/Converters/VersionConverter.cs", &[]);
        let files = [caller, partial, near, far, suite, sample, prod];
        let (_, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        let hit = |row: usize, p: &str| targets[row].contains(&Some(idx(p)));
        assert!(hit(0, "Dapper/src/JsonReader.Async.cs"));
        assert!(hit(0, "Dapper/src/Extensions.cs"));
        assert!(
            !hit(0, "AutoMapper/src/Extensions.cs"),
            "an assembly the caller cannot link against"
        );
        let tests = idx("Json/Tests/Converters/VersionConverterTests.cs");
        assert!(hit(tests, "Json/Src/Converters/VersionConverter.cs"));
        assert!(
            !hit(tests, "Json/Tests/Samples/CustomConverter.cs"),
            "a test's declaration must not answer for a name"
        );
    }

    #[test]
    fn a_package_named_where_the_module_is_computed_names_every_module_in_it() {
        // rich writes `import_module(f".unicode{version}",
        // "rich._unicode_data")`. The module cannot be read and the
        // package can, so every module under it is a candidate. 22 of
        // Python's 28 orphans sat in that one package.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let mut loader = file(Lang::Python, "rich/_unicode_data/__init__.py", &[]);
        loader.imports = vec![ImportFact {
            target: "rich._unicode_data.*".into(),
            names: Vec::new(),
            reach: Reach::Mention,
        }];
        let files = [
            loader,
            file(Lang::Python, "rich/_unicode_data/unicode14.py", &[]),
            file(Lang::Python, "rich/_unicode_data/unicode15.py", &[]),
            // One level deeper is not in the package.
            file(Lang::Python, "rich/_unicode_data/old/unicode9.py", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        for m in ["unicode14.py", "unicode15.py"] {
            assert!(targets[0].contains(&Some(idx(m))), "{m} is a candidate");
        }
        assert!(!targets[0].contains(&Some(idx("old/unicode9.py"))));
        // The file states a package, not a module, so nothing is
        // tallied: which one it loads is settled at run time.
        assert_eq!((res.internal, res.external, res.unresolved), (0, 0, 0));
    }

    #[test]
    fn a_table_that_lists_names_is_a_registry_of_what_sits_beside_it() {
        // `kong/db/migrations/core/init.lua` returns
        // `{"000_base", "003_100_to_110", ...}` and a migration runner
        // requires each beside it. 91 of Lua's 223 remaining orphans
        // were named that way and no other.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach: Reach::Mention,
        };
        let mut index = file(Lang::Lua, "kong/db/migrations/core/init.lua", &[]);
        index.imports = vec![imp("000_base"), imp("003_100_to_110"), imp("nope")];
        let files = [
            index,
            file(Lang::Lua, "kong/db/migrations/core/000_base.lua", &[]),
            file(Lang::Lua, "kong/db/migrations/core/003_100_to_110.lua", &[]),
            // Same name, a different directory: a registry lists what is
            // beside it, and reaching further would let any word in a
            // table claim a module somewhere else in the tree.
            file(Lang::Lua, "kong/plugins/acl/migrations/000_base.lua", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[0][0], Some(idx("core/000_base.lua")));
        assert_eq!(targets[0][1], Some(idx("core/003_100_to_110.lua")));
        assert!(!targets[0].contains(&Some(idx("acl/migrations/000_base.lua"))));
        // A registry states no dependency: it writes names, and whether
        // each is a module is settled by whether one is there.
        assert_eq!((res.internal, res.external, res.unresolved), (0, 0, 0));
    }

    #[test]
    fn a_built_module_name_is_read_on_both_sides_of_what_it_cannot_read() {
        // kong writes `local plugin_handler = "kong.plugins." .. plugin
        // .. ".handler"` and requires the local three lines later. The
        // prefix alone reaches NOTHING — each of kong's 150 plugins is
        // a directory, so no file sits directly under kong/plugins —
        // while the suffix names one real file per plugin. 174 of gold
        // Lua's 342 orphans are inside that directory.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str, reach| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach,
        };
        let mut loader = file(Lang::Lua, "kong/db/dao/plugins.lua", &[]);
        loader.imports = vec![
            imp("kong.plugins.*.handler", Reach::Mention),
            imp("kong.plugins.*", Reach::Anywhere),
        ];
        let files = [
            loader,
            file(Lang::Lua, "kong/plugins/acl/handler.lua", &[]),
            file(Lang::Lua, "kong/plugins/acl/schema.lua", &[]),
            file(Lang::Lua, "kong/plugins/acme/handler.lua", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        for h in ["acl/handler.lua", "acme/handler.lua"] {
            assert!(targets[0].contains(&Some(idx(h))), "{h} is loaded that way");
        }
        assert!(
            !targets[0].contains(&Some(idx("acl/schema.lua"))),
            "the suffix is what tells the files of a plugin apart"
        );
        // The bare prefix names a plugin DIRECTORY and no file is
        // directly inside one, so it resolves to nothing — and the
        // built name that did resolve is not tallied, because writing a
        // string is not stating a dependency.
        assert_eq!((res.internal, res.external, res.unresolved), (0, 1, 0));
    }

    #[test]
    fn a_ruby_directory_prefix_is_anchored_at_a_load_path_root() {
        // `require "roda/plugins/#{name}"` names a directory the way
        // Lua's concatenated require does, and 417 of gold Ruby's 490
        // orphans sat under six such prefixes. What tells a real prefix
        // from a gem is the load path: rubygems puts `lib` on it and
        // nothing else, so sequel's own lib/sequel/adapters/jdbc is not
        // what `require "jdbc/#{name}"` reaches — that line loads "the
        // necessary JDBC support via a gem", as its own comment says.
        use crate::facts::ImportFact;
        use crate::lang::Reach;
        let imp = |target: &str, reach| ImportFact {
            target: target.into(),
            names: Vec::new(),
            reach,
        };
        let mut plugins = file(Lang::Ruby, "roda/lib/roda/plugins.rb", &[]);
        plugins.imports = vec![imp("roda/plugins/*", Reach::Anywhere)];
        let mut rodauth = file(Lang::Ruby, "rodauth/lib/rodauth.rb", &[]);
        rodauth.imports = vec![imp("rodauth/features/*", Reach::Anywhere)];
        let mut jdbc = file(Lang::Ruby, "sequel/lib/sequel/adapters/jdbc.rb", &[]);
        jdbc.imports = vec![imp("jdbc/*", Reach::Anywhere)];
        let mut pool = file(Lang::Ruby, "sequel/lib/sequel/connection_pool.rb", &[]);
        pool.imports = vec![imp("connection_pool/*", Reach::Project)];
        let files = [
            plugins,
            rodauth,
            jdbc,
            pool,
            file(Lang::Ruby, "roda/lib/roda/plugins/render.rb", &[]),
            file(Lang::Ruby, "roda/lib/roda/plugins/assets/css.rb", &[]),
            file(Lang::Ruby, "rodauth/lib/rodauth/features/login.rb", &[]),
            file(Lang::Ruby, "sequel/lib/sequel/adapters/jdbc/mysql.rb", &[]),
            file(
                Lang::Ruby,
                "sequel/lib/sequel/connection_pool/thread.rb",
                &[],
            ),
        ];
        let (res, targets) = super::resolve_imports(&files);
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        // Directly under the prefix, and only directly: a plugin that
        // brings a directory of its own keeps its own entry point.
        assert_eq!(targets[0], [Some(idx("plugins/render.rb"))]);
        assert_eq!(targets[1], [Some(idx("features/login.rb"))]);
        assert!(
            !targets[2].contains(&Some(idx("jdbc/mysql.rb"))),
            "jdbc/ names a gem, and sequel's own adapters are not under a lib/"
        );
        // `require_relative` is not on the load path at all; it names a
        // directory beside the requiring file.
        assert_eq!(targets[3], [Some(idx("connection_pool/thread.rb"))]);
        // A prefix that names modules of this project is not a
        // third-party dependency, and the one that names a gem is
        // exactly that.
        assert_eq!((res.internal, res.external, res.unresolved), (3, 1, 0));
    }

    #[test]
    fn a_lua_directory_entry_point_answers_to_its_directory() {
        // `package.path` is `?.lua;?/init.lua`, so `require 'vm'` names
        // vm/init.lua and reaches vm/vm.lua only in its absence.
        // Reading init as an ordinary module sent all 71 of
        // lua-language-server's `require 'vm'` to the submodule and
        // left the entry point with no importers at all.
        let files = [
            file(Lang::Lua, "script/luarocks/cmd.lua", &[]),
            file(Lang::Lua, "script/luarocks/cmd/init.lua", &[]),
            file(Lang::Lua, "script/main.lua", &["vm", "luarocks.cmd"]),
            file(Lang::Lua, "script/vm/init.lua", &[]),
            file(Lang::Lua, "script/vm/vm.lua", &[]),
        ];
        let (res, targets) = super::resolve_imports(&files);
        assert_eq!(res.internal, 2, "both requires name files in this tree");
        let idx = |p: &str| files.iter().position(|f| f.path.ends_with(p)).unwrap();
        assert_eq!(targets[2][0], Some(idx("script/vm/init.lua")));
        // And `?.lua` is searched first, so the plain file still wins
        // where a directory of the same name also has an entry point.
        assert_eq!(targets[2][1], Some(idx("script/luarocks/cmd.lua")));
    }

    #[test]
    fn a_module_search_finds_the_shallowest_match() {
        // `require 'pl.utils'` walks package.path, which reaches the
        // project's own tree before anything vendored under it. Taking
        // the path-first match instead handed every edge to the copy
        // that happened to sort earliest.
        let files = [
            file(Lang::Lua, "a/deep/vendor/pl/utils.lua", &[]),
            file(Lang::Lua, "app/main.lua", &["pl.utils"]),
            file(Lang::Lua, "z/pl/utils.lua", &[]),
        ];
        let (_, e) = edges(&files);
        assert_eq!(e, [(1, 2)]);
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
