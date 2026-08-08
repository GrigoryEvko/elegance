//! Rung-5 architecture metrics over the resolved module graph. These are
//! distributional REPORTS, never CI gates: they describe the shape of the
//! dependency structure so a human can judge it.
//!
//! Parnas 1979: a correct uses-hierarchy is acyclic and subsettable —
//! cycle mass is the single best size-free architecture-health ratio.
//! Deletability is the underrated half of evolvability: code you can
//! delete never complected itself into its neighbors.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::{GraphFacts, Resolution};

pub struct Architecture {
    pub resolution: Resolution,
    pub modules: u32,
    pub edges: u32,
    /// Share of modules living inside a dependency cycle (SCC size >= 2).
    /// File-level cycles within one directory are common idiom (Rust
    /// sibling modules exchange types); read this next to the directory
    /// figure.
    pub cycle_mass_pct: f64,
    /// Members of the largest cycle, capped for display.
    pub largest_cycle: Vec<String>,
    pub largest_cycle_size: u32,
    /// Share of directories inside a directory-level cycle — the Parnas
    /// violation proper: package layers that cannot be built, tested, or
    /// understood independently.
    pub dir_cycle_mass_pct: f64,
    pub largest_dir_cycle: Vec<String>,
    pub largest_dir_cycle_size: u32,
    /// Dependency depth (longest chain in the condensation) percentiles.
    pub depth_p50: u32,
    pub depth_p90: u32,
    pub depth_max: u32,
    /// Modules the two dependency questions below are asked of:
    /// `modules` less the files whose fan-in the language fixes at
    /// zero. See `Lang::is_sink`.
    pub judged_modules: u32,
    /// Share of JUDGED modules nothing depends on (safe to delete
    /// outright).
    pub deletable_pct: f64,
    /// Modules with the widest blast radius: (path, transitive dependents).
    pub load_bearing: Vec<(String, u32)>,
    /// Non-test modules with no importers and no entry-point name:
    /// total count and a capped sample.
    pub orphan_count: u32,
    pub orphans: Vec<String>,
    /// Judged modules no production file imports but a TEST does. Not
    /// orphans: a library exercised only by its own suite is a finding
    /// of its own, and calling it unreferenced is a false positive on
    /// public API.
    pub tested_only: u32,
    /// Orphans and judged modules per language, worst rate first, for
    /// trees holding more than one. A pooled figure over a polyglot
    /// directory says nothing about any language in it: gold `zig/`
    /// carries 190 Swift and 92 C files against 1138 Zig, and reads 23%
    /// pooled where Zig alone reads 7%.
    pub by_language: Vec<(&'static str, u32, u32)>,
    pub interfaces: Interfaces,
}

/// Parnas information hiding, measured: a module is good when it hides
/// a lot behind a little.
pub struct Interfaces {
    /// Median mass-per-surface-unit over exporting modules. Depth alone
    /// selects for god-modules with narrow facades — shallow listings
    /// carry mass so a 5000-line two-export module reads as suspicious.
    pub median_depth: u32,
    /// Wide-but-shallow modules: (label, depth, mass, surface cost).
    pub shallow: Vec<(String, u32, u32, u32)>,
    /// Fat, under-used surfaces: (label, exports ever imported, exports).
    /// Name-level approximation: bound import names, not call sites.
    pub fat: Vec<(String, u32, u32)>,
    /// Underscore symbols imported across package boundaries — content
    /// coupling (Constantine), Python convention only for now.
    pub leak_count: u32,
    pub leaks: Vec<String>,
    /// Exported symbols no other file mentions: surface with no consumer,
    /// and the cheapest deletion in any codebase. Count plus a sample.
    pub dead_count: u32,
    pub dead: Vec<String>,
}

/// How many files mention each identifier, keyed by name.
pub type Mentions = HashMap<Box<str>, u32>;

const CYCLE_SHOW: usize = 8;
const LOAD_SHOW: usize = 5;
const ORPHAN_SHOW: usize = 8;

/// Entry-point stems that legitimately have no importers.
const ENTRY_STEMS: &[&str] = &[
    "main", "lib", "index", "__init__", "__main__", "app", "cli", "build", "Package", "mix",
];

pub fn analyze(all: &[GraphFacts], mentions: &Mentions) -> Option<Architecture> {
    let (resolution, files, edge_list, targets, mut from_tests) = production_view(all)?;
    let n = files.len();
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut fan_in = vec![0u32; n];
    for &(a, b) in &edge_list {
        succ[a as usize].push(b);
        fan_in[b as usize] += 1;
    }

    let sccs = Sccs::of(&succ);
    let file_labels: Vec<String> = files.iter().map(path_label).collect();
    let cycles = CycleStats::of(&file_labels, &sccs);

    let (dir_labels, dir_succ) = dir_graph(&files, &edge_list);
    let dir_cycles = CycleStats::of(&dir_labels, &Sccs::of(&dir_succ));

    let comp_succ = condense(&succ, &sccs);
    let module_depths = module_depths(&sccs, &comp_succ, n);
    let q = |p: f64| depth_at(&module_depths, p);

    let mut blasts = blast_radii(&sccs, &comp_succ, n);
    fold_over_modules(&files, &edge_list, &mut fan_in, &mut blasts);
    fold_over_packages(&files, &edge_list, &mut fan_in, &mut blasts);
    fold_test_reach(&files, all, &mut from_tests);
    fold_cfg_test_modules(&files, &fan_in, &mut from_tests);
    let judged = judgeable(&files, &fan_in);
    let (judged_modules, deletable_pct) = deletability(&blasts, &judged);
    let load = load_bearing(&file_labels, &blasts);
    let (orphan_count, orphans, tested_only) = find_orphans(&files, &fan_in, &judged, &from_tests);
    let interfaces = interfaces(&files, &targets, &file_labels, mentions);

    Some(Architecture {
        resolution,
        modules: n as u32,
        edges: edge_list.len() as u32,
        cycle_mass_pct: cycles.mass_pct,
        largest_cycle_size: cycles.largest_size,
        largest_cycle: cycles.largest_members,
        dir_cycle_mass_pct: dir_cycles.mass_pct,
        largest_dir_cycle_size: dir_cycles.largest_size,
        largest_dir_cycle: dir_cycles.largest_members,
        depth_p50: q(crate::report::P50),
        depth_p90: q(crate::report::P90),
        depth_max: *module_depths.last().expect("n >= 2"),
        judged_modules,
        deletable_pct,
        load_bearing: load,
        orphan_count,
        orphans,
        tested_only,
        by_language: orphans_by_language(&files, &fan_in, &judged, &from_tests),
        interfaces,
    })
}

/// The production graph: tests are scaffolding, and (Go proves it) test
/// files may legally close cycles production code cannot have. Returns
/// None when there is nothing to analyze. The last element aligns a
/// resolved internal target with every import of every kept file.
type ProdView<'a> = (
    Resolution,
    Vec<&'a GraphFacts>,
    Vec<(u32, u32)>,
    Vec<Vec<Option<usize>>>,
    // Last element: per kept file, how many TEST files import it. A
    // test importing production code is not production coupling, so it
    // earns no edge — but it is not nothing either, and a file reached
    // only that way is a different finding from one reached by nobody.
    Vec<u32>,
);

fn production_view(all: &[GraphFacts]) -> Option<ProdView<'_>> {
    let (resolution, all_targets) = super::resolve_imports(all);
    let keep: Vec<usize> = (0..all.len()).filter(|&m| !all[m].is_test).collect();
    let renum: HashMap<usize, usize> = keep
        .iter()
        .enumerate()
        .map(|(new, &old)| (old, new))
        .collect();
    let files: Vec<&GraphFacts> = keep.iter().map(|&m| &all[m]).collect();
    let targets: Vec<Vec<Option<usize>>> = keep
        .iter()
        .map(|&old| {
            all_targets[old]
                .iter()
                .map(|t| t.and_then(|j| renum.get(&j).copied()))
                .collect()
        })
        .collect();
    let mut seen = HashSet::new();
    let mut edge_list: Vec<(u32, u32)> = Vec::new();
    for (i, row) in targets.iter().enumerate() {
        for &j in row.iter().flatten() {
            if i != j && seen.insert((i, j)) {
                edge_list.push((i as u32, j as u32));
            }
        }
    }
    let mut from_tests = vec![0u32; files.len()];
    for (old, facts) in all.iter().enumerate() {
        if !facts.is_test {
            continue;
        }
        for &j in all_targets[old].iter().flatten() {
            if let Some(&new) = renum.get(&j) {
                from_tests[new] += 1;
            }
        }
    }
    (files.len() >= 2 && !edge_list.is_empty())
        .then_some((resolution, files, edge_list, targets, from_tests))
}

/// Strongly connected components with per-component sizes.
struct Sccs {
    comp: Vec<usize>,
    size: Vec<u32>,
}

impl Sccs {
    fn of(succ: &[Vec<u32>]) -> Sccs {
        let comp = Tarjan::run(succ);
        let ncomp = comp.iter().map(|&c| c + 1).max().unwrap_or(0);
        let mut size = vec![0u32; ncomp];
        for &c in &comp {
            size[c] += 1;
        }
        Sccs { comp, size }
    }

    fn in_cycle(&self, node: usize) -> bool {
        self.size[self.comp[node]] >= 2
    }
}

/// Cycle mass and the largest cycle's labeled members.
struct CycleStats {
    mass_pct: f64,
    largest_size: u32,
    largest_members: Vec<String>,
}

impl CycleStats {
    fn of(labels: &[String], sccs: &Sccs) -> CycleStats {
        let n = labels.len();
        let cyclic = (0..n).filter(|&m| sccs.in_cycle(m)).count();
        let largest = (0..sccs.size.len())
            .max_by_key(|&c| sccs.size[c])
            .unwrap_or(0);
        let mut members: Vec<String> = (0..n)
            .filter(|&m| sccs.comp[m] == largest && sccs.size[largest] >= 2)
            .map(|m| labels[m].clone())
            .collect();
        members.sort_unstable();
        members.truncate(CYCLE_SHOW);
        CycleStats {
            mass_pct: 100.0 * cyclic as f64 / n.max(1) as f64,
            largest_size: sccs.size.get(largest).copied().unwrap_or(0).max(1),
            largest_members: members,
        }
    }
}

/// Directory-granularity graph. Ancestor-descendant edges are containment
/// (a Rust crate's mod.rs and its children), not layering — only
/// unrelated-directory edges count.
fn dir_graph(files: &[&GraphFacts], edge_list: &[(u32, u32)]) -> (Vec<String>, Vec<Vec<u32>>) {
    let mut ids: HashMap<&Path, u32> = HashMap::new();
    let mut names: Vec<&Path> = Vec::new();
    let dir_of: Vec<u32> = files
        .iter()
        .map(|f| {
            let d = f.path.parent().unwrap_or(Path::new(""));
            *ids.entry(d).or_insert_with(|| {
                names.push(d);
                names.len() as u32 - 1
            })
        })
        .collect();
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); names.len()];
    let mut seen = HashSet::new();
    for &(a, b) in edge_list {
        let (da, db) = (dir_of[a as usize], dir_of[b as usize]);
        let nested = names[da as usize].starts_with(names[db as usize])
            || names[db as usize].starts_with(names[da as usize]);
        if da != db && !nested && seen.insert((da, db)) {
            succ[da as usize].push(db);
        }
    }
    let labels = names.iter().map(|p| p.display().to_string()).collect();
    (labels, succ)
}

/// Component-level edges, deduplicated. Tarjan emits components in
/// reverse topological order — successors always have lower ids.
fn condense(succ: &[Vec<u32>], sccs: &Sccs) -> Vec<Vec<u32>> {
    let mut comp_succ: Vec<Vec<u32>> = vec![Vec::new(); sccs.size.len()];
    let mut seen = HashSet::new();
    for (a, succs) in succ.iter().enumerate() {
        for &b in succs {
            let (ca, cb) = (sccs.comp[a], sccs.comp[b as usize]);
            if ca != cb && seen.insert((ca, cb)) {
                comp_succ[ca].push(cb as u32);
            }
        }
    }
    comp_succ
}

fn depth_at(sorted: &[u32], p: f64) -> u32 {
    let idx = (sorted.len() - 1) as f64 * p;
    sorted[idx.round() as usize]
}

fn path_label(f: &&GraphFacts) -> String {
    f.path.display().to_string()
}

/// Longest dependency chain below each module, counted in components,
/// sorted ascending. Reverse-topo ids make this a single pass.
fn module_depths(sccs: &Sccs, comp_succ: &[Vec<u32>], n: usize) -> Vec<u32> {
    let mut depth = vec![1u32; comp_succ.len()];
    for c in 0..comp_succ.len() {
        for &s in &comp_succ[c] {
            depth[c] = depth[c].max(depth[s as usize] + 1);
        }
    }
    let mut out: Vec<u32> = (0..n).map(|m| depth[sccs.comp[m]]).collect();
    out.sort_unstable();
    out
}

/// Bitset word width for the ancestor sets.
const WORD: usize = 64;

/// Transitive dependents per module, via ancestor bitsets over the
/// condensation: topological order (reversed emission), each component
/// pushing its ancestor set plus itself onto its successors.
///
/// Blocked 64 ancestors at a time. The whole ncomp x ncomp bit matrix is
/// the obvious encoding and costs ncomp^2/8 bytes — 312 MB at 50k
/// modules, which is what actually stood between this tool and the
/// 10M-line codebases it claims to handle. One word per component per
/// pass is the same algorithm at O(ncomp) memory, and iterating set bits
/// rather than all 64 keeps the work proportional to the closure that
/// actually exists.
fn blast_radii(sccs: &Sccs, comp_succ: &[Vec<u32>], n: usize) -> Vec<u32> {
    let ncomp = comp_succ.len();
    let mut mass = vec![0u32; ncomp];
    let mut anc = vec![0u64; ncomp];
    for lo in (0..ncomp).step_by(WORD) {
        anc.fill(0);
        propagate(comp_succ, &mut anc, lo);
        for (c, slot) in mass.iter_mut().enumerate() {
            *slot += ancestor_mass(anc[c], lo, c, sccs);
        }
    }
    (0..n)
        .map(|m| {
            let c = sccs.comp[m];
            mass[c] + sccs.size[c] - 1
        })
        .collect()
}

/// Push each component's ancestors-within-this-block onto its successors.
/// Tarjan emits components in reverse topological order, so one downward
/// pass reaches every descendant.
fn propagate(comp_succ: &[Vec<u32>], anc: &mut [u64], lo: usize) {
    for c in (0..comp_succ.len()).rev() {
        let own = c
            .checked_sub(lo)
            .filter(|d| *d < WORD)
            .map_or(0, |d| 1 << d);
        let bits = anc[c] | own;
        for &s in &comp_succ[c] {
            anc[s as usize] |= bits;
        }
    }
}

/// Total module count behind the ancestors this block's bits name.
/// Iterating set bits rather than all WORD of them keeps the work
/// proportional to the closure that actually exists.
fn ancestor_mass(mut bits: u64, lo: usize, of: usize, sccs: &Sccs) -> u32 {
    let mut total = 0;
    while bits != 0 {
        let c = lo + bits.trailing_zeros() as usize;
        bits &= bits - 1;
        // A component is not its own ancestor; cycle partners are added
        // once by the caller.
        if c != of {
            total += sccs.size[c];
        }
    }
    total
}

/// The widest blast radii, worst first, capped for display.
fn load_bearing(labels: &[String], blasts: &[u32]) -> Vec<(String, u32)> {
    let mut load: Vec<(String, u32)> = labels
        .iter()
        .zip(blasts)
        .filter(|(_, b)| **b > 0)
        .map(|(l, b)| (l.clone(), *b))
        .collect();
    load.sort_by(widest_first);
    load.truncate(LOAD_SHOW);
    load
}

fn widest_first(a: &(String, u32), b: &(String, u32)) -> std::cmp::Ordering {
    b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0))
}

/// Surface size at which a module is wide enough to judge.
const WIDE_SURFACE: u32 = 8;

/// Interface metrics: depth (hidden mass per surface unit), utilization
/// (share of exports anyone ever imports by name), and cross-package
/// underscore leaks.
fn interfaces(
    files: &[&GraphFacts],
    targets: &[Vec<Option<usize>>],
    labels: &[String],
    mentions: &Mentions,
) -> Interfaces {
    let mut depths = Vec::new();
    let mut shallow = Vec::new();
    for (m, f) in files.iter().enumerate() {
        if f.surface_cost > 0 {
            depths.push(depth(f));
        }
        if f.surface_cost >= WIDE_SURFACE {
            shallow.push((labels[m].clone(), depth(f), f.mass, f.surface_cost));
        }
    }
    depths.sort_unstable();
    let median_depth = depths.get(depths.len() / 2).copied().unwrap_or(0);
    shallow.sort_by(shallowest_first);
    shallow.truncate(LOAD_SHOW);

    let used = surface_use(files, targets);
    let mut fat = Vec::new();
    for (m, f) in files.iter().enumerate() {
        if f.exports.len() as u32 >= WIDE_SURFACE {
            let hits = surface_hits(f, &used[m]);
            fat.push((labels[m].clone(), hits, f.exports.len() as u32));
        }
    }
    fat.sort_by(least_used_first);
    fat.truncate(LOAD_SHOW);

    let mut leaks = find_leaks(files, targets, labels);
    leaks.sort_unstable();
    leaks.dedup();
    let leak_count = leaks.len() as u32;
    leaks.truncate(ORPHAN_SHOW);
    let (dead_count, dead) = find_dead_exports(files, labels, mentions);
    Interfaces {
        median_depth,
        shallow,
        fat,
        leak_count,
        leaks,
        dead_count,
        dead,
    }
}

/// Exports nothing outside their own file ever names. Import lists alone
/// cannot answer this — Go and C never import names — so the evidence is
/// how many FILES mention the identifier: one means only its definition.
///
/// Deliberately a report, never a gate. A library's public API is
/// legitimately unreferenced inside its own repository, which is why
/// declared API surfaces (lib.rs, __init__.py, index.ts, mod.rs) are
/// exempt outright. Names shared with an unrelated symbol elsewhere read
/// as alive: the error runs toward silence, as it should.
///
/// The rate is a property of the language as much as the codebase. Gold
/// reads go 4.0%, rust 6.0%, ts 9.0%, zig 9.9%, py 18.4% — Python is
/// highest because it has no visibility modifier at all, so every helper
/// without a leading underscore counts as surface. In an inferring
/// language a named type can also flow through call sites that never
/// spell it, so "nobody names this" is narrower than "nobody uses this";
/// it still says the name is in no other file's vocabulary.
fn find_dead_exports(
    files: &[&GraphFacts],
    labels: &[String],
    mentions: &Mentions,
) -> (u32, Vec<String>) {
    let mut dead: Vec<String> = Vec::new();
    for (m, f) in files.iter().enumerate() {
        let api_surface = f
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| ENTRY_STEMS.contains(&s));
        if api_surface {
            continue;
        }
        for sym in &f.exports {
            if mentions.get(sym).copied().unwrap_or(0) <= 1 {
                dead.push(format!("{}.{sym}", labels[m]));
            }
        }
    }
    dead.sort_unstable();
    dead.dedup();
    let count = dead.len() as u32;
    dead.truncate(ORPHAN_SHOW);
    (count, dead)
}

/// Hidden mass per unit of surface — Ousterhout's deep-vs-shallow.
fn depth(f: &GraphFacts) -> u32 {
    f.mass / f.surface_cost.max(1)
}

type Shallow = (String, u32, u32, u32);

fn shallowest_first(a: &Shallow, b: &Shallow) -> std::cmp::Ordering {
    a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0))
}

/// How many of a module's exports its importers ever bind by name.
fn surface_hits(f: &GraphFacts, used: &HashSet<&str>) -> u32 {
    let mut hits = 0;
    for e in &f.exports {
        hits += used.contains(&**e) as u32;
    }
    hits
}

/// Least-used surface first: ratio ascending by cross-multiplication.
fn least_used_first(a: &(String, u32, u32), b: &(String, u32, u32)) -> std::cmp::Ordering {
    (a.1 as u64 * b.2 as u64)
        .cmp(&(b.1 as u64 * a.2 as u64))
        .then_with(|| a.0.cmp(&b.0))
}

/// Which exported names each module's importers actually bind.
fn surface_use<'a>(
    files: &[&'a GraphFacts],
    targets: &[Vec<Option<usize>>],
) -> Vec<HashSet<&'a str>> {
    let mut used: Vec<HashSet<&str>> = vec![HashSet::new(); files.len()];
    for (i, f) in files.iter().enumerate() {
        for (imp, tgt) in f.imports.iter().zip(&targets[i]) {
            let Some(j) = *tgt else { continue };
            for name in &imp.names {
                used[j].insert(name);
            }
        }
    }
    used
}

/// Underscore symbols imported from a DIFFERENT package — content
/// coupling. Same-package private sharing is idiomatic Python and
/// exempt; dunders are protocol, not privacy.
fn find_leaks(
    files: &[&GraphFacts],
    targets: &[Vec<Option<usize>>],
    labels: &[String],
) -> Vec<String> {
    let mut leaks = Vec::new();
    for (i, f) in files.iter().enumerate() {
        if f.lang != crate::lang::Lang::Python {
            continue;
        }
        for (imp, tgt) in f.imports.iter().zip(&targets[i]) {
            let Some(j) = *tgt else { continue };
            if files[i].path.parent() != files[j].path.parent() {
                leak_names(imp, &labels[i], &labels[j], &mut leaks);
            }
        }
    }
    leaks
}

fn leak_names(imp: &crate::facts::ImportFact, from: &str, to: &str, leaks: &mut Vec<String>) {
    for name in &imp.names {
        if name.starts_with('_') && !name.starts_with("__") {
            leaks.push(format!("{from} <- {to}.{name}"));
        }
    }
}

/// Swift and Go state a dependency on a MODULE, which is a directory,
/// and the edge lands on a representative file of it — so every OTHER
/// file of that module reads as depended on by nothing.
///
/// There is no import to fix. Alamofire's Source/Core/AFError.swift
/// declares `public enum AFError` and 24 files under Source reference
/// it; Source/Core/Session.swift contains exactly one import, and it is
/// `import Foundation`. Swift has no syntax that would say more. At
/// file granularity 98% of Swift is orphaned by construction, and Go
/// sits at 52% for the same reason one tier milder.
///
/// So the answer is folded over the module: a file inherits whatever
/// depends on the directory it belongs to, and a module nobody imports
/// keeps its zero. What survives is the answer worth reading — example
/// executables, benchmark targets, demo servers.
///
/// A Go package is exactly its directory. A Swift target is an
/// ancestor, since `Sources/NIOCore/Channel/` is still NIOCore, so the
/// nearest depended ancestor is the one that counts.
fn fold_over_modules(
    files: &[&GraphFacts],
    edges: &[(u32, u32)],
    fan_in: &mut [u32],
    blasts: &mut [u32],
) {
    use crate::lang::Lang;
    let folded = |f: &GraphFacts| matches!(f.lang, Lang::Swift | Lang::Go);
    if !files.iter().any(|f| folded(f)) {
        return;
    }
    // Keyed by language as well as path: a directory only vouches for
    // the files of the language whose import named it.
    let mut depended: HashSet<(usize, &Path)> = HashSet::new();
    for &(_, b) in edges {
        let target = files[b as usize];
        if let (true, Some(dir)) = (folded(target), target.path.parent()) {
            depended.insert((target.lang as usize, dir));
        }
    }
    // A module that HOLDS an entry file is itself the entry point. Go
    // spreads one `package main` over as many files as it likes and a
    // Swift executable target does the same, and none of them import
    // each other: chi's `_examples/todos-resource/` is main.go plus
    // todos.go and users.go, swift-nio's `Sources/NIOPerformanceTester/`
    // is main.swift plus 23 more. 27 files in gold read as depended on
    // by nothing because only the file literally named `main` was exempt.
    let entries: HashSet<&Path> = files
        .iter()
        .filter(|f| folded(f) && entryish(&f.path))
        .filter_map(|f| f.path.parent())
        .collect();
    for (i, f) in files.iter().enumerate() {
        if !folded(f) {
            continue;
        }
        let holds = |d| depended.contains(&(f.lang as usize, d));
        let module = match f.lang {
            Lang::Go => f.path.parent().is_some_and(holds),
            _ => f.path.ancestors().skip(1).any(holds),
        } || f.path.parent().is_some_and(|d| entries.contains(d));
        fan_in[i] = u32::from(module);
        blasts[i] = u32::from(module);
    }
}

/// The same fold, for the OTHER thing an import can credit.
///
/// `fan_in` is folded over the module for every language whose imports
/// name one, and `from_tests` never was — so a test importing a Go
/// package credited only the file standing for it, and the package's
/// other files read as reached by nobody. Every one of go-cmp's
/// `internal/teststructs/project1..4.go` is that: `compare_test.go`
/// imports the package, and the four files the representative did not
/// stand for were orphans. 11 of Go's 15 remaining orphans were this
/// one gap, and it reached Java, Scala and Swift the same way.
///
/// A zero is filled and a count is never overwritten: the question is
/// only whether a test reaches the module at all.
fn fold_test_reach(files: &[&GraphFacts], all: &[GraphFacts], from_tests: &mut [u32]) {
    let mut reached: HashSet<(usize, String)> = files
        .iter()
        .zip(from_tests.iter())
        .filter(|&(_, &n)| n > 0)
        .filter_map(|(f, _)| module_key(f))
        .collect();
    // A test that BELONGS to the module writes no import into it, for
    // the same reason a production sibling does not: `src/test/java/io/
    // netty/handler/pcap/PcapWriteHandlerTest.java` declares package
    // `io.netty.handler.pcap` and names `PcapWriteHandler` bare, and a
    // Go `_test.go` sits in the package it exercises. Fourteen of gold
    // Java's twenty remaining orphans, 58 of Scala's 83 and all three of
    // tigerbeetle's Go client files have a same-package test and no
    // other reader — which is `tested_only`, a finding this metric
    // already separates from being reached by nobody. All 75 change
    // BUCKET and none changes a judged count.
    reached.extend(all.iter().filter(|f| f.is_test).filter_map(module_key));
    for (i, f) in files.iter().enumerate() {
        let held = module_key(f).is_some_and(|k| reached.contains(&k));
        from_tests[i] = from_tests[i].max(u32::from(held));
    }
}

/// A Rust module its own parent declares behind `#[cfg(test)]`.
///
/// The pack DECLINES to emit that declaration as an edge — `src/lang/
/// rust.rs` requires `!preceding_attr_contains(node, src, "cfg(test")`
/// — and it is right to: a module compiled only under `cfg(test)` is
/// not production coupling. But refusing the edge left the file reached
/// by nothing at all, which says something stronger and false. rayon
/// writes `#[cfg(test)]` then `mod test;` five times and ripgrep's
/// searcher once for `src/testutil.rs`, and all six read as orphans
/// where `tested_only` is the bucket that describes them.
///
/// Routed to `from_tests` rather than to `fan_in`, so the tool still
/// says nothing in production depends on the file. Read from disk only
/// where the file would otherwise BE an orphan, so a corpus without the
/// shape opens nothing. 6 orphans, the last Rust ones in gold.
fn fold_cfg_test_modules(files: &[&GraphFacts], fan_in: &[u32], from_tests: &mut [u32]) {
    use crate::lang::Lang;
    for (i, f) in files.iter().enumerate() {
        if f.lang != Lang::Rust || fan_in[i] > 0 || from_tests[i] > 0 {
            continue;
        }
        // `x.rs` is module `x` of the directory holding it; `x/mod.rs`
        // is module `x` of the directory ABOVE.
        let (stem, dir) = match f.path.file_name().and_then(|n| n.to_str()) {
            Some("mod.rs") => (f.path.parent(), f.path.parent().and_then(Path::parent)),
            _ => (Some(f.path.as_path()), f.path.parent()),
        };
        let (Some(stem), Some(dir)) = (stem.and_then(|p| p.file_stem()?.to_str()), dir) else {
            continue;
        };
        let parents = ["mod.rs", "lib.rs", "main.rs"].map(|n| dir.join(n));
        let sibling = dir.with_extension("rs");
        from_tests[i] =
            u32::from(parents.iter().chain([&sibling]).any(|p| {
                std::fs::read_to_string(p).is_ok_and(|text| declared_cfg_test(&text, stem))
            }));
    }
}

/// Does this module text declare `mod <stem>;` behind a `#[cfg(test)]`?
/// The attribute must be the token immediately before it, so a
/// production `mod testutil;` further down the file does not match one
/// higher up.
fn declared_cfg_test(text: &str, stem: &str) -> bool {
    let decl = format!("mod {stem};");
    text.match_indices(&decl).any(|(at, _)| {
        let head = text[..at].trim_end();
        let head = head.strip_suffix("pub").unwrap_or(head).trim_end();
        head.ends_with("#[cfg(test)]")
    })
}

/// What a module means for the language, where its imports name one: a
/// directory for Go and Swift, a declared package for Java and Scala.
fn module_key(f: &GraphFacts) -> Option<(usize, String)> {
    use crate::lang::Lang;
    let name = match f.lang {
        Lang::Go | Lang::Swift => f.path.parent()?.display().to_string(),
        Lang::Java | Lang::Scala => package_of(&f.path)?,
        _ => return None,
    };
    Some((f.lang as usize, name))
}

/// A Java or Scala PACKAGE is the unit of visibility — package-private
/// is Java's default access — and a reference between two files of one
/// package needs no import at all. Nobody writes the redundant one: 11
/// of the gold corpus's 71978 Java imports name the importer's own
/// package, and 1961 of Java's 2939 orphans are named by a same-package
/// sibling. There is no edge to resolve better.
///
/// Where the import IS written it names the file, so unlike a Go package
/// the count is kept and only a zero is filled: a file nothing imports
/// inherits whatever depends on its package, and a package nobody
/// imports keeps its zero.
fn fold_over_packages(
    files: &[&GraphFacts],
    edges: &[(u32, u32)],
    fan_in: &mut [u32],
    blasts: &mut [u32],
) {
    use crate::lang::Lang;
    let jvm = |f: &GraphFacts| matches!(f.lang, Lang::Java | Lang::Scala);
    if !files.iter().any(|f| jvm(f)) {
        return;
    }
    let key = |f: &GraphFacts| Some((f.lang as usize, package_of(&f.path)?));
    let depended: HashSet<(usize, String)> = edges
        .iter()
        .map(|&(_, b)| files[b as usize])
        .filter(|f| jvm(f))
        .filter_map(key)
        .collect();
    // A package that HOLDS an entry point is itself reached, for the
    // same reason a Go directory holding a `main` is. See `jvm_entry`.
    let entries: HashSet<(usize, String)> = files
        .iter()
        .filter(|f| jvm(f) && jvm_entry(f))
        .filter_map(|f| key(f))
        .collect();
    for (i, f) in files.iter().enumerate() {
        if !jvm(f) {
            continue;
        }
        // The launcher names the FILE. Its package is credited too where
        // it declares one, but the entry point itself is reached whether
        // it does or not: tigerbeetle's four `samples/*/src/main/java/
        // Main.java` declare no package at all.
        let held =
            jvm_entry(f) || key(f).is_some_and(|k| depended.contains(&k) || entries.contains(&k));
        fan_in[i] = fan_in[i].max(u32::from(held));
        blasts[i] = blasts[i].max(u32::from(held));
    }
}

/// A file the JVM launcher itself names: `java Foo` runs `Foo.main`, so
/// the class holding it is an entry point and nothing in the tree needs
/// to reference it.
///
/// Read from the file's own declared surface rather than from its path.
/// 94 of the gold Java corpus's non-test files write `static void main`
/// and not ONE of them is named `Main.java` — netty writes
/// `AutobahnServer`, `Http2Server` and `DnsNativeClient`, gson writes
/// `ParseBenchmark`, junit5 writes `ConsoleLauncher` — so the
/// path-based `ENTRY_STEMS` could never see them.
///
/// Worth 19 of gold Java's 65 orphans and one more in the zig corpus's
/// java client, every one a netty `testsuite-*` demo server or the
/// handler and initializer sitting in its package. It raises `judged`
/// by 154 as well, but circularly: those are files the zero-fan-in
/// gates had dropped and that this rule itself makes unorphanable, so
/// the denominator growth is not extra coverage.
fn jvm_entry(f: &GraphFacts) -> bool {
    entryish(&f.path) || f.exports.iter().any(|e| &**e == "main")
}

/// The package a JVM source file declares, read off its path: whatever
/// lies below the source root.
///
/// A package is not a directory. sbt cross-building spreads one over
/// `shared/`, `js/`, `scala-2/` and `scala-3/` source roots and Gradle
/// spreads it over `src/main/java` and `src/testFixtures/java`; keying
/// on the directory left 496 of Scala's modules orphaned where the
/// package leaves 290.
///
/// `None` for a file sitting directly ON a source root: that is the
/// UNNAMED package, which groups nothing. Every such file in a scan
/// shares the empty key whatever repository it came from, so crediting
/// it makes one project answer for another — zio's `zio-docs/src/main/
/// scala/utils.scala` was credited by a test in `streams-tests`, a
/// different sbt project, and tigerbeetle's four `samples/*/src/main/
/// java/Main.java` credited the java client's `module-info.java`. 32
/// files share the empty key in the gold Java corpus alone, across
/// gson, netty, junit5 and jackson-databind.
fn package_of(path: &Path) -> Option<String> {
    let comps: Vec<&str> = path
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let root = |c: &str| {
        matches!(c, "java" | "scala" | "kotlin")
            || c.starts_with("scala-")
            || c.starts_with("java-")
    };
    // `src/<set>/<language>/<package>` is what Maven, Gradle and sbt all
    // write. `java` is a package name too — netty declares
    // java.lang.invoke — so the root is found by the `src` above it
    // rather than by its name alone.
    let below = comps
        .windows(3)
        .rposition(|w| w[0] == "src" && root(w[2]))
        .map(|k| k + 3)
        .or_else(|| comps.iter().rposition(|c| root(c)).map(|k| k + 1))
        .unwrap_or(0);
    // Below the root a dotted directory is a package written flat: zio
    // files its Scala-2 stream sources under `scala-2/zio.stream/`, and
    // reading that as one component put them in a package of their own.
    // Only below the root — `scala-2.13+` is a SOURCE SET and splitting
    // it took 47 Scala modules out of the packages they belong to.
    let name = comps[below..].join("/").replace('.', "/");
    (!name.is_empty()).then_some(name)
}

/// A file whose stem declares it an entry point or an API surface.
///
/// The FIRST dot-segment, not `file_stem`: ariakit writes
/// `index.react.tsx`, whose stem is `index.react`, and no entry name
/// ever matched. Reading the first segment exempts 360 tsx files and
/// exactly none in ts or js.
fn entryish(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let stem = name.split('.').next().unwrap_or("");
    ENTRY_STEMS.contains(&stem)
        || reserved(name)
        || routed(path, name, stem)
        || dune_root(path, name, stem)
        || mix_task(path)
        || header_the_build_names(path, name)
        || xcode_app(path)
        || swift_leaf_target(path)
        || cargo_target(path, name)
        || gem_entry(path, name)
        || manifest_entry(path)
        || tool_driver(path)
        || built_by_name(path, name)
}

/// A header whose only caller is outside this repository, because the
/// build DESCRIPTION is what reaches it and not any source in the tree.
///
/// Three separate readings, kept as three functions because they are
/// three different claims: `include/` is a toolchain constant, an
/// `addIncludePath` is a per-package declaration, and a `-I` with a
/// variable in it is a per-repository configuration. Only the extension
/// test is shared.
fn header_the_build_names(path: &Path, name: &str) -> bool {
    const HEADER: &[&str] = &["h", "hpp", "hh", "hxx", "h++", "cuh", "inc", "inl"];
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| HEADER.contains(&ext))
        && (published_header(path) || declared_include_root(path) || configured_header(path))
}

/// A header inside an `include/` tree: the directory a build puts on the
/// CONSUMER's search path, so the specifier naming one is written
/// relative to it and whoever writes that specifier is outside this
/// repository.
///
/// Every repository in gold that ships such a tree says so itself.
/// musl's Makefile installs the whole of it —
/// `$(DESTDIR)$(includedir)/%: $(srcdir)/include/%` at :210, reached
/// from `install-headers` at :218 — curl's CMakeLists.txt:2366 writes
/// `install(DIRECTORY "${PROJECT_SOURCE_DIR}/include/curl" …)`,
/// magic_enum's :100 `target_include_directories(…
/// $<BUILD_INTERFACE:${PROJECT_SOURCE_DIR}/include>)` and cutlass's :718
/// globs `include/cutlass/*.h`. Their own sources write the specifier
/// the same way: musl's syslog.c says `#include <stdio.h>` for
/// `include/stdio.h`, fmt's os.cc `#include "fmt/os.h"` for
/// `include/fmt/os.h`.
///
/// An entry point and not a sink, because the tree is mostly NOT
/// unreferenced. Gold holds 1374 headers under an `include/` directory
/// and 66 of them have no includer here; musl/include alone ships 183 of
/// which 127 are included in the tree, so a sink would drop the lot from
/// the judged population to describe a twentieth of it. 66 orphans
/// across six corpora.
fn published_header(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "include")
}

/// A header under an include root the build CHOOSES BETWEEN.
///
/// musl's Makefile:51 writes
/// `CFLAGS_ALL += -I$(srcdir)/arch/$(ARCH) -I$(srcdir)/arch/generic …`,
/// so `#include "ksigaction.h"` names one of seven real files and which
/// one is settled by `$(ARCH)`. The generic copy is what the nearest
/// match answers with — see `Index::nearest_includes`, which says so —
/// and the six per-architecture copies read as included by nobody while
/// each is included in its own configuration, exactly as a translation
/// unit is linked in its own.
///
/// A make VARIABLE BELOW the first component is what marks the root, and
/// it is the only shape in gold that does: of the 44 `-I` flags in the
/// corpus's makefiles that mention a variable at all, 43 spell it only
/// as a `$(srcdir)`-style prefix, which selects nothing. Deliberately
/// entryish and not an edge: handing all seven copies of ksigaction.h an
/// edge would manufacture six dependencies where the build takes one.
/// 9 orphans, all musl's.
fn configured_header(path: &Path) -> bool {
    path.ancestors().skip(1).any(|dir| {
        let Ok(rel) = path.strip_prefix(dir) else {
            return false;
        };
        ["Makefile", "makefile"].iter().any(|mk| {
            std::fs::read_to_string(dir.join(mk)).is_ok_and(|text| {
                let mut flags = text.split_whitespace().filter_map(|t| t.strip_prefix("-I"));
                flags.any(|root| under_variant(rel, root))
            })
        })
    })
}

/// A file a `package.json` above it NAMES as an entry point of its
/// package. Nothing inside the repository imports one, because the
/// consumer is outside it: radix publishes 36 re-export barrels through
/// `"./*": "./src/*.ts"`, preact eight `compat/*` shims through
/// `exports`, and every vscode extension its activation module through
/// `main` and `browser`.
///
/// The declared path is normally a BUILD OUTPUT — vscode writes
/// `"main": "./out/npmMain"` and its tsconfig `"rootDir": "./src"`,
/// `"outDir": "./out"` — so a value that names no file on disk is
/// matched by NAME and DEPTH within the package that declared it. The
/// directory the compiler writes to is a build detail; the name the
/// toolchain loads, and how far down it sits, are the claim.
///
/// The depth is what keeps a stem from claiming a tree. `"main"` and
/// `"exports"` name a compiled artefact, and matching a bare stem
/// anywhere below the package exempted 479 gold files to win about a
/// hundred: hono's fourteen subpath exports claimed 155 files, copilot's
/// single `"./dist/extension"` claimed nine (three of them ordinary
/// modules), vscode's root `"./out/main.js"` twelve. Requiring the
/// candidate to sit as many components deep as the declared path does —
/// `"./dist/request.js"` is two, so `src/request.ts` answers and
/// `src/utils/request.ts` does not — cuts that blanket to 384 and costs
/// exactly one orphan across all 22 corpora. Two stricter forms were
/// measured and are worse buys: "a unique stem within the package"
/// costs 18, and "the declared path minus its first component must
/// equal the candidate's" costs 10 by losing the real two-level outputs
/// like `"./client/dist/browser/cssClientMain"`.
///
/// A wildcard must narrow: `"./*": "./src/*.ts"` says which files are
/// exports, `"./*": "./*"` says only that the package is a directory.
/// ariakit's website and guide declare the second, and honouring it
/// would have exempted 95 of that repository's ordinary modules.
fn manifest_entry(path: &Path) -> bool {
    let Some(stem) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let stem = stem.split('.').next().unwrap_or("");
    path.ancestors().skip(1).any(|dir| {
        package_entries(dir).is_some_and(|e| {
            let depth = path.strip_prefix(dir).map_or(0, |r| r.components().count());
            e.stems.iter().any(|(s, d)| &**s == stem && *d == depth)
                || e.exact.iter().any(|p| p == path)
                || e.scripted.iter().any(|p| p == path)
                || e.globs.iter().any(|(head, tail)| {
                    path.starts_with(head)
                        && path.to_str().is_some_and(|p| {
                            p.len() >= head.as_os_str().len() + tail.len() && p.ends_with(&**tail)
                        })
                })
        })
    })
}

/// Does the path lie under `arch/<anything>` where the makefile wrote
/// `$(srcdir)/arch/$(ARCH)`? The leading source-tree variable is dropped
/// and each remaining one matches a single component.
fn under_variant(rel: &Path, root: &str) -> bool {
    fn chosen(component: &str) -> bool {
        component.starts_with("$(") || component.starts_with("${")
    }
    let pattern: Vec<&str> = root.split('/').skip_while(|c| chosen(c)).collect();
    if !pattern.iter().any(|c| chosen(c)) {
        return false;
    }
    let mut here = rel.components().filter_map(|c| c.as_os_str().to_str());
    let matched = pattern
        .iter()
        .all(|want| here.next().is_some_and(|got| chosen(want) || got == *want));
    matched && here.next().is_some()
}

/// A header under a directory a `build.zig` DECLARES as an include path
/// under some name other than `include`.
///
/// ghostty vendors seven C libraries under `pkg/`, each with its own
/// `build.zig`, and each names the directory holding the headers it
/// substitutes for upstream's: `libxml2/build.zig` writes
/// `addIncludePath(b.path("override/config/posix"))`, glslang's
/// `b.path("override")`, and libintl's and freetype's `b.path("")` for
/// the package directory itself. The sources those headers answer are
/// FETCHED rather than vendored, so nothing in this repository includes
/// one and the declaration is the only statement there is. 5 orphans.
fn declared_include_root(path: &Path) -> bool {
    const DECL: &str = "addIncludePath(b.path(\"";
    path.ancestors().skip(1).any(|dir| {
        let Ok(rel) = path.strip_prefix(dir) else {
            return false;
        };
        std::fs::read_to_string(dir.join("build.zig")).is_ok_and(|text| {
            text.split(DECL)
                .skip(1)
                .filter_map(|call| call.split('"').next())
                .any(|root| rel.starts_with(root))
        })
    })
}

/// A source of a SwiftPM target that produces something nothing can
/// import: an executable, a compiler-plugin macro, or a build plugin.
///
/// A library target is reached by `import <name>` and the module fold
/// already credits every file in it. An executable has no such name —
/// `main.swift` used to be the marker and `@main` replaced it, so
/// swift-nio's echo and websocket samples, vapor's `Development` and the
/// nine files of its `.macro(name: "VaporMacrosPlugin")` at Package.swift
/// :141 all read as unreferenced. swift-nio's manifest declares 15
/// `.executableTarget(`, its `dev/stackdiff` one more. 21 orphans.
fn swift_leaf_target(path: &Path) -> bool {
    const LEAF: &[&str] = &[".executableTarget(", ".macro(", ".plugin("];
    if path.extension().and_then(|e| e.to_str()) != Some("swift") {
        return false;
    }
    path.ancestors().skip(1).any(|dir| {
        let Ok(text) = std::fs::read_to_string(dir.join("Package.swift")) else {
            return false;
        };
        LEAF.iter().any(|kind| {
            text.split(kind)
                .skip(1)
                .filter_map(target_name)
                .any(|target| path.starts_with(dir.join("Sources").join(target)))
        })
    })
}

/// The `name:` a target stanza declares, read at ARGUMENT level.
///
/// `.executableTarget(name: "X", dependencies: [.product(name: "Y", …)])`
/// carries two of them and only the outer one names a directory. Cutting
/// the call at its first `)` — which the nested `.product(` closes —
/// reads the right one for all 22 leaf stanzas in gold only because
/// every one writes `name:` on the line after the paren; tracking the
/// nesting instead does not depend on the field order, and a manifest
/// that reordered would yield nothing rather than the wrong directory.
fn target_name(call: &str) -> Option<&str> {
    let mut depth = 0i32;
    for (at, b) in call.bytes().enumerate() {
        match b {
            b'(' | b'[' => depth += 1,
            b')' | b']' if depth == 0 => return None, // the stanza closed
            b')' | b']' => depth -= 1,
            b'n' if depth == 0 && call[at..].starts_with("name:") => {
                return super::quoted_after(&call[at..], "name:");
            }
            _ => {}
        }
    }
    None
}

/// A Swift source Xcode compiles into an APPLICATION.
///
/// An app bundle is a leaf: no module can import one, and inside a
/// module Swift has no import to write, so a file that ends up in an
/// application target is unreferenced by construction. ghostty's
/// `TerminalController` is declared once and named in 24 files, and not
/// one of them imports anything.
///
/// The PRODUCT TYPE is the discriminator, not the presence of a project:
/// `Ghostty.xcodeproj` declares
/// `productType = "com.apple.product-type.application"` against a
/// `PBXFileSystemSynchronizedRootGroup` whose `path = Sources` at
/// project.pbxproj:274, which is Xcode 16's way of saying "compile that
/// whole directory" with no per-file listing — 139 orphans. Alamofire's
/// project declares `com.apple.product-type.framework` for five targets
/// and no application at all, its 94 library files ARE importable, and
/// reading any `.xcodeproj` ancestor as an app would have claimed them.
///
/// The product type is matched as a PREFIX, so `.application.watchapp2`,
/// `.application.watchapp2-container` and `.application-extension` are
/// accepted too. That is deliberate — a watch app and an app extension
/// are leaves for the same reason — and it is inert in gold: Alamofire's
/// `watchOS Example` project declares two of the three, and contains no
/// `PBXFileSystemSynchronizedRootGroup` at all, so it lists its files
/// one by one and this rule reads nothing from it.
fn xcode_app(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("swift") {
        return false;
    }
    for dir in path.ancestors().skip(1) {
        let mut project = None;
        let mut repo_root = false;
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            match entry.file_name().to_str() {
                Some(n) if n.ends_with(".xcodeproj") => project = Some(entry.path()),
                Some(".git") => repo_root = true,
                _ => {}
            }
        }
        if let Some(proj) = project {
            let Ok(text) = std::fs::read_to_string(proj.join("project.pbxproj")) else {
                return false;
            };
            return app_target_dirs(&text)
                .iter()
                .any(|group| path.starts_with(dir.join(group)));
        }
        // The scan never leaves the repository: a `.xcodeproj` above it
        // belongs to another project and says nothing about this file.
        if repo_root {
            return false;
        }
    }
    false
}

/// The directories an Xcode project hands wholesale to an application
/// target, read from `project.pbxproj`.
///
/// A `PBXNativeTarget` writes its fields alphabetically and `productType`
/// is the last of them, so the text between the marker and it is exactly
/// one target. A synchronized root group declares its `path` on a single
/// line, which is how the identifier is turned back into a directory.
fn app_target_dirs(text: &str) -> Vec<&str> {
    let wanted = app_group_ids(text);
    text.lines()
        .filter(|l| l.contains("isa = PBXFileSystemSynchronizedRootGroup;"))
        .filter_map(|l| group_path(l, &wanted))
        .collect()
}

/// The synchronized root groups every APPLICATION target claims.
fn app_group_ids(text: &str) -> Vec<&str> {
    const APP: &str = "com.apple.product-type.application";
    let mut wanted: Vec<&str> = Vec::new();
    for block in text.split("isa = PBXNativeTarget;").skip(1) {
        let Some((body, kind)) = block.split_once("productType = \"") else {
            continue;
        };
        let ids = body
            .split_once("fileSystemSynchronizedGroups = (")
            .and_then(|(_, rest)| rest.split_once(')'));
        if let (true, Some((ids, _))) = (kind.starts_with(APP), ids) {
            wanted.extend(ids.split(',').filter_map(|e| e.split_whitespace().next()));
        }
    }
    wanted
}

/// The directory one root-group line declares, when the group is wanted.
fn group_path<'a>(line: &'a str, wanted: &[&str]) -> Option<&'a str> {
    let id = line.split_whitespace().next()?;
    if !wanted.contains(&id) {
        return None;
    }
    let (_, rest) = line.split_once("path = ")?;
    let declared = rest.split(';').next()?;
    Some(declared.trim().trim_matches('"'))
}

/// A source file a Cargo manifest names outright.
///
/// What Cargo builds from a `[[bin]]`, `[[bench]]`, `[[example]]` or
/// `[[test]]` is a TARGET: an artefact nothing links against, reached by
/// a name only the manifest writes. regex's `fuzz/Cargo.toml` declares
/// eight — `[[bin]] name = "fuzz_regex_match"` with
/// `path = "fuzz_targets/fuzz_regex_match.rs"` — and ripgrep's fuzz
/// manifest a ninth. All nine read as unreferenced. 9 orphans.
fn cargo_target(path: &Path, name: &str) -> bool {
    if !name.ends_with(".rs") {
        return false;
    }
    path.ancestors().skip(1).any(|dir| {
        let Ok(rel) = path.strip_prefix(dir) else {
            return false;
        };
        let rel = rel.display().to_string().replace('\\', "/");
        std::fs::read_to_string(dir.join("Cargo.toml"))
            .is_ok_and(|text| declares_target(&text, &rel))
    })
}

/// Is this path the `path` of a TARGET table?
///
/// The enclosing header is what separates the artefact from the crate:
/// `[lib] path = "src/lib.rs"` names the root every dependent `use`s,
/// the exact opposite of something nothing links against, and three
/// manifests in gold write one (git's two libgit crates and a toml
/// testdata fixture). A dependency's `path` points ABOVE the manifest so
/// it can never spell a file under it, but it is excluded here anyway by
/// sitting under `[dependencies]`.
fn declares_target(text: &str, rel: &str) -> bool {
    const TARGET: &[&str] = &["[[bin]]", "[[bench]]", "[[example]]", "[[test]]"];
    let mut here = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            here = TARGET.contains(&line);
        } else if here
            && line
                .split_once('=')
                .is_some_and(|(k, v)| k.trim() == "path" && v.trim().trim_matches('"') == rel)
        {
            return true;
        }
    }
    false
}

/// The build descriptions that name a FILE rather than a library, and
/// are read only for a file sitting in the same directory as one.
const BUILD_FILES: &[&str] = &[
    "Makefile",
    "Makefile.am",
    "Makefile.in",
    "GNUmakefile",
    "CMakeLists.txt",
    "meson.build",
    "Rakefile",
    "build.zig",
];

/// A file the build description in its OWN directory spells by name.
///
/// `dune_root` already makes this argument for `(name containers)`;
/// this is the same argument for the build systems that name a file
/// instead of a library. curl's lib/Makefile.am:182 runs
/// `@PERL@ $(srcdir)/optiontable.pl` over `include/curl/curl.h` into
/// `easyoptions.c`, git's Makefile:2812 builds git-instaweb from
/// `unimplemented.sh`, and curl's src/CMakeLists.txt names mkhelp.pl
/// and mk-file-embed.pl. A generator nothing imports is not
/// unreferenced; it is invoked.
///
/// OWN DIRECTORY, never an ancestor. The widening was tried and
/// rejected on one case: fmt's CMakeLists.txt lists every header as a
/// source, so `include/fmt/core.h` would be exempted — and core.h IS
/// included by format.h, so the exemption would HIDE a resolver defect
/// rather than report it. The own-directory restriction is what keeps
/// an install manifest from being read as an entry-point claim across a
/// whole repository.
///
/// Two filters, both of which fire in gold and both of which cost
/// nothing that is not a false positive.
///
/// A COMMENTED line names nothing. ghostty's pkg/libintl/build.zig
/// opens with `//! ... I generated the config.h on my own machine (a
/// Mac) and then copied it here`, and that prose was the whole reason
/// config.h read as built. Its identical sibling libgnuintl.h, same
/// directory and same status, was not exempted — because the prose does
/// not happen to mention it. A rule that can be moved by an anecdote is
/// not reading the build.
///
/// A SHIPPING MANIFEST is not an invocation. `EXTRA_DIST` says "put
/// this in the tarball" and `*_HEADERS` / `*_DATA` say "install this";
/// none of the three says anything runs. curl's projects/vms/Makefile.am
/// lists vms_eco_level.h under EXTRA_DIST at line 59, and that file is
/// genuinely dead — make_pcsi_curl_kit_name.com:106-108 opens it as a
/// TEXT file to read a version stamp out of it. Same shape for curl's
/// Dockerfile and docs/libcurl/symbols.pl.
///
/// The manifest is followed THROUGH ITS VARIABLES, because that is
/// where the corpus's largest instance hides: curl's
/// tests/Makefile.am:26 opens `TESTSCRIPTS = \` with eighteen
/// `test*.pl` under it, and TESTSCRIPTS is referenced exactly once, at
/// line 85, as the last entry of EXTRA_DIST. Reading only the literal
/// left-hand side exempted all eighteen — a third of this rule's whole
/// gain — on a claim no stronger than the one that was rejected for
/// vms_eco_level.h.
fn built_by_name(path: &Path, name: &str) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    BUILD_FILES.iter().any(|build| {
        std::fs::read_to_string(dir.join(build)).is_ok_and(|text| names_whole(&text, name))
    })
}

/// Does this build text spell the filename, on a line that claims
/// something is done with it?
///
/// A make assignment continues across a trailing backslash, so the
/// variable a line belongs to is carried forward: curl's
/// projects/vms/Makefile.am opens `EXTRA_DIST = \` at line 24 and
/// reaches vms_eco_level.h at line 59.
fn names_whole(text: &str, name: &str) -> bool {
    const COMMENT: &[&str] = &["#", "//", "--"];
    let ship = shipping_vars(text);
    let mut lhs: Option<&str> = None;
    let mut continued = false;
    for line in text.lines() {
        if !continued {
            lhs = assigned(line);
        }
        continued = line.trim_end().ends_with('\\');
        let head = line.trim_start();
        let inert =
            COMMENT.iter().any(|c| head.starts_with(c)) || lhs.is_some_and(|v| ship.contains(&v));
        if !inert && spells(line, name) {
            return true;
        }
    }
    false
}

/// What one `package.json` declares: files that exist as written,
/// wildcard (prefix, suffix) pairs, and the (name, depth) of the rest.
#[derive(Default)]
struct Entries {
    exact: Vec<PathBuf>,
    /// Files a `scripts` command RUNS, kept apart from the entry fields
    /// so the two claims can be counted separately. See `scripted_files`.
    scripted: Vec<PathBuf>,
    globs: Vec<(PathBuf, Box<str>)>,
    stems: Vec<(Box<str>, usize)>,
    /// Package names this manifest declares — what says a file named
    /// after a tool is that tool's driver. See `tool_driver`.
    dependencies: Vec<Box<str>>,
}

/// The fields a package manifest loads a file BY NAME through. `types`
/// is deliberately absent: it points at a `.d.ts`, which is already a
/// sink, and `files`/`workspaces` list directories rather than entries.
const ENTRY_FIELDS: &[&str] = &[
    "main",
    "module",
    "browser",
    "bin",
    "exports",
    "source",
    "unpkg",
    "react-native",
    "svelte",
];

const WEB_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Parsed once per directory: `entryish` is asked about every file and
/// a package.json sits above thousands of them.
fn package_entries(dir: &Path) -> Option<std::rc::Rc<Entries>> {
    thread_local! {
        static CACHE: std::cell::RefCell<HashMap<PathBuf, Option<std::rc::Rc<Entries>>>> =
            std::cell::RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        if let Some(hit) = c.borrow().get(dir) {
            return hit.clone();
        }
        let parsed = read_package_entries(dir).map(std::rc::Rc::new);
        c.borrow_mut().insert(dir.to_path_buf(), parsed.clone());
        parsed
    })
}

fn read_package_entries(dir: &Path) -> Option<Entries> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut out = Entries {
        scripted: scripted_files(&json, dir),
        dependencies: declared_dependencies(&json),
        ..Entries::default()
    };
    let mut values = Vec::new();
    for field in ENTRY_FIELDS.iter().filter_map(|f| json.get(f)) {
        collect_strings(field, &mut values);
    }
    for value in values.iter().filter_map(|v| v.strip_prefix("./")) {
        record_entry(&mut out, dir, value);
    }
    Some(out)
}

/// One entry-field value, classified: a wildcard that narrows, a file
/// that exists as written, or a (name, depth) pair to match by, because
/// the manifest gave the BUILD OUTPUT's path.
fn record_entry(out: &mut Entries, dir: &Path, body: &str) {
    if let Some((head, tail)) = body.split_once('*') {
        // The head must name a directory inside the package, or the
        // pattern claims the package's whole tree.
        if head.contains('/') {
            out.globs.push((dir.join(head), tail.into()));
        }
        return;
    }
    let base = dir.join(body);
    let named = std::iter::once(base.clone())
        .chain(WEB_EXTS.iter().map(|e| with_suffix(&base, e)))
        .chain(WEB_EXTS.iter().map(|e| with_suffix(&base.join("index"), e)))
        .find(|p| p.is_file());
    match named {
        // A build output states its own depth: `./out/npmMain` is two
        // components below the package, and only a source file two
        // components below it can be what the compiler wrote it from.
        None => {
            let depth = body.split('/').filter(|s| !s.is_empty()).count();
            out.stems.extend(first_segment(&base).map(|s| (s, depth)));
        }
        Some(p) => out.exact.push(p),
    }
}

/// A file a `scripts` entry RUNS by name rather than importing: vscode's
/// extensions write `"bundle-web": "node ./esbuild.browser.mts"`, immer
/// `"test:perf": "cd __performance_tests__ && node add-data.mjs"`, trpc
/// four `script.*.ts` under its website, mithril `browser.js`.
///
/// A third arm of the entry fields above and not a rule of its own: it
/// writes into the same `exact` set and fires only through
/// `manifest_entry`, and 20 of the ts files it names are vscode
/// `esbuild*.mts` drivers that `tool_driver` names too. Its own
/// marginal worth, once the tool-driver rule is in, is 7 ts and 7 js.
///
/// Read textually, because the value is a command line and not a path —
/// only the tokens carrying a web extension are looked for, and only
/// where they name a file that is really there.
fn scripted_files(json: &serde_json::Value, dir: &Path) -> Vec<PathBuf> {
    let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) else {
        return Vec::new();
    };
    let commands = scripts.values().filter_map(|v| v.as_str());
    commands.flat_map(|c| command_files(c, dir)).collect()
}

/// The files one command line names, followed through its own `cd`:
/// immer runs its four benchmarks as `cd __performance_tests__ && node
/// add-data.mjs && node todo.mjs`, and a token joined against the
/// package root would have named nothing. A second `cd` is relative to
/// the first, as a shell reads it.
///
/// A token that ARGUES A FLAG is not a file the command runs, and one
/// of them inverts the claim outright: ariakit-test writes
/// `"docs-react": "... --entry src/react.tsx ... --exclude src/index.ts"`,
/// where `src/index.ts` is named precisely because it is not an entry.
fn command_files(command: &str, dir: &Path) -> Vec<PathBuf> {
    const NEGATED: &[&str] = &["--exclude", "--ignore", "--external", "--exclude-file"];
    let mut base = dir.to_path_buf();
    let mut out = Vec::new();
    let mut tokens = command.split([' ', '\t', '"', '\'', '=']).peekable();
    while let Some(token) = tokens.next() {
        if token == "cd" {
            base = tokens.next().map_or_else(|| base.clone(), |d| base.join(d));
            continue;
        }
        if NEGATED.contains(&token) {
            tokens.next();
            continue;
        }
        let named = base.join(token.trim_start_matches("./"));
        if crate::lang::web_extension(token) && named.is_file() {
            out.push(named);
        }
    }
    out
}

/// Every package this manifest declares, in any of the four fields.
fn declared_dependencies(json: &serde_json::Value) -> Vec<Box<str>> {
    const DEPENDENCY_FIELDS: &[&str] = &[
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ];
    DEPENDENCY_FIELDS
        .iter()
        .filter_map(|f| json.get(f)?.as_object())
        .flat_map(|o| o.keys())
        .map(|k| k.as_str().into())
        .collect()
}

/// A path's file name up to its first dot: `ipynbMain.node` is named
/// after `ipynbMain`.
fn first_segment(path: &Path) -> Option<Box<str>> {
    let name = path.file_name()?.to_str()?;
    Some(name.split('.').next().unwrap_or("").into())
}

/// `Path::with_extension` REPLACES a dotted stem: `ipynbMain.node` plus
/// `ts` would have become `ipynbMain.ts`.
fn with_suffix(base: &Path, ext: &str) -> PathBuf {
    let mut named = base.as_os_str().to_os_string();
    named.push(".");
    named.push(ext);
    PathBuf::from(named)
}

/// Every string in a JSON subtree: `exports` nests its conditions.
fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|v| collect_strings(v, out)),
        serde_json::Value::Object(o) => o.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

/// The variables of this build file whose contents only ever get
/// shipped or installed: the three names that say so outright, plus
/// every variable spliced into one of them, to a fixed point.
fn shipping_vars(text: &str) -> HashSet<&str> {
    let manifest = |v: &str| v == "EXTRA_DIST" || v.ends_with("_HEADERS") || v.ends_with("_DATA");
    let mut ship: HashSet<&str> = HashSet::new();
    // (variable referenced, variable whose value references it).
    let mut spliced: Vec<(&str, &str)> = Vec::new();
    let mut lhs: Option<&str> = None;
    let mut continued = false;
    for line in text.lines() {
        if !continued {
            lhs = assigned(line);
        }
        continued = line.trim_end().ends_with('\\');
        let Some(var) = lhs else { continue };
        if manifest(var) {
            ship.insert(var);
        }
        spliced.extend(expansions(line).map(|used| (used, var)));
    }
    let mut grew = true;
    while grew {
        grew = false;
        for &(used, by) in &spliced {
            if ship.contains(&by) {
                grew |= ship.insert(used);
            }
        }
    }
    ship
}

/// The variable names a line expands: `$(TESTSCRIPTS)`, `${SOURCES}`.
fn expansions(line: &str) -> impl Iterator<Item = &str> {
    line.match_indices('$').filter_map(|(at, _)| {
        let rest = &line[at + 1..];
        let close = match rest.chars().next()? {
            '(' => ')',
            '{' => '}',
            _ => return None,
        };
        let inner = rest[1..].split(close).next()?;
        let plain = |c: char| c.is_ascii_alphanumeric() || c == '_';
        (!inner.is_empty() && inner.chars().all(plain)).then_some(inner)
    })
}

/// The make variable a line assigns, if it assigns one. `:=`, `+=` and
/// `?=` are the same statement as `=`.
fn assigned(line: &str) -> Option<&str> {
    let var = line
        .split_once('=')?
        .0
        .trim()
        .trim_end_matches([':', '+', '?']);
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '_';
    (!var.is_empty() && var.chars().all(plain)).then_some(var)
}

/// The name must stand on its own. A rule for `unittest.sh` says
/// nothing about `test.sh`, and `easyoptions.c` is not `options.c`. A
/// leading `/` is a path prefix and still names the file, which is how
/// `$(srcdir)/optiontable.pl` is read.
fn spells(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + name.len()..].chars().next();
        free(before) && free(after)
    })
}

/// A delimiter, or the end of the line. `/` is one, which is how
/// `$(srcdir)/optiontable.pl` reads as naming optiontable.pl.
fn free(neighbour: Option<char>) -> bool {
    !neighbour.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// The root module of a dune library or executable, which the `dune`
/// file beside it names.
///
/// `containers/src/core/dune` says `(name containers)` and
/// `containers.ml` is what consumers `open`; nothing inside the project
/// imports it, exactly as nothing imports a `lib.rs`. 53 of gold
/// OCaml's 134 orphans are one, and the name is read from the manifest
/// rather than guessed from the path — a library's module is called
/// after the library, not after the directory holding it.
fn dune_root(path: &Path, name: &str, stem: &str) -> bool {
    let ml = name.ends_with(".ml") || name.ends_with(".mli");
    // A js_of_ocaml stub is JavaScript that the OCaml build links in,
    // and the dune file beside it lists the files by name:
    // `(js_of_ocaml (javascript_files strftime.js runtime.js ...))`.
    // Seven of the gold OCaml corpus's JavaScript orphans are one, and
    // no import in any language can name a linker input.
    let stub = name.ends_with(".js");
    if !ml && !stub {
        return false;
    }
    let Some(dir) = path.parent() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(dir.join("dune")) else {
        return false;
    };
    // `(names ...)` is the plural: core's `bench-bin/dune` declares
    // twelve executables in one stanza, and each is an entry point.
    let fields: &[&str] = match ml {
        true => &["(name ", "(public_name ", "(names "],
        false => &["(javascript_files ", "(wasm_files "],
    };
    // A stub is listed under its FULL name, a module under its stem.
    let wanted = match ml {
        true => stem,
        false => name,
    };
    fields
        .iter()
        .flat_map(|field| text.match_indices(field).map(|(at, f)| at + f.len()))
        .filter_map(|at| text[at..].split(')').next())
        .flat_map(str::split_whitespace)
        .any(|declared| match ml {
            true => declared.rsplit('.').next() == Some(wanted),
            false => declared == wanted,
        })
}

/// A Mix task, which `mix` resolves from the MODULE name — running
/// `mix phx.server` finds `Mix.Tasks.Phx.Server` — so nothing imports
/// one and 33 of gold Elixir's 93 orphans are one.
///
/// The path answers as precisely as the declaration does: the corpus
/// holds 35 files under `mix/tasks/` and 35 declaring `Mix.Tasks.`, and
/// they are the same 35. No other language puts anything there.
///
/// Deliberately an entry point rather than a one-shot directory. A task
/// legitimately has no importers, which is what this says — but
/// `mix phx.server` starts a server and waits, so claiming nothing is
/// waiting on its executor would be the wrong second claim to make.
fn mix_task(path: &Path) -> bool {
    let norm = crate::facts::rooted(&path.display().to_string());
    norm.contains("/mix/tasks/")
}

/// The eight filenames Next.js reserves inside an `app/` or `pages/`
/// tree. The router loads each by NAME, so nothing imports one, and 78
/// of gold tsx's 506 orphans are one.
///
/// Both narrowings are load-bearing rather than decoration. Without the
/// extension the stems reach musl's `include/net/route.h` and immer's
/// eight `default.cpp`; with it but without the directory they still
/// reach vscode's `layout.ts`, mithril's `route.js` and trpc's
/// `error.ts`, which are ordinary modules their neighbours import.
fn routed(path: &Path, name: &str, stem: &str) -> bool {
    const ROUTE_FILES: &[&str] = &[
        "page",
        "layout",
        "loading",
        "error",
        "not-found",
        "template",
        "default",
        "route",
    ];
    const ROUTE_ROOTS: &[&str] = &["app", "pages"];
    let web = name
        .rsplit('.')
        .next()
        .is_some_and(|ext| matches!(ext, "tsx" | "ts" | "jsx" | "js"));
    web && ROUTE_FILES.contains(&stem)
        && path
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .filter_map(|a| a.file_name()?.to_str())
            .any(|d| ROUTE_ROOTS.contains(&d))
}

/// One flag per file: can this module be asked whether anything depends
/// on it? A translation unit is a sink by construction, so its answer
/// is settled before the code is read. See `Lang::is_sink`.
fn judgeable(files: &[&GraphFacts], fan_in: &[u32]) -> Vec<bool> {
    files
        .iter()
        .zip(fan_in)
        .map(|(f, &reached)| {
            let sink = f.lang.is_sink(&f.path) || declares_types_only(f);
            let unnameable = package_private(f) || assembly_metadata(f);
            !(sink || reached == 0 && (runs_once(&f.path) || unnameable))
        })
        .collect()
}

/// A C# file of nothing but `[assembly: ...]`.
///
/// `Lang::is_sink` already makes this argument for the file NAMED
/// `AssemblyInfo.cs`; the content is what that name was standing in for,
/// because FluentValidation writes `AssemblyInfo.FluentValidation.cs`
/// and `CommonAssemblyInfo.cs` and Dapper writes `Global.cs`. Eight
/// files in the gold C# corpus carry an assembly attribute and no type
/// keyword and five of them the name test already caught.
///
/// Zero-fan-in gated and NOT a sink, though a sink is what the shape
/// argues for: `is_sink` is wrong wherever any instance has fan-in, and
/// one does. Dapper's `Global.cs` is answered from the file-stem index
/// on the bare word `Global`, so a sink would drop a measurable file out
/// of the population to remove the two orphans this removes anyway. The
/// weakest mechanism that gets the same number is the one that ships.
///
/// The read is reached only for a file that exports nothing and that
/// nothing references, so an ordinary module is never opened.
fn assembly_metadata(f: &GraphFacts) -> bool {
    f.lang == crate::lang::Lang::CSharp
        && f.exports.is_empty()
        && std::fs::read_to_string(&f.path).is_ok_and(|text| {
            text.contains("[assembly")
                && !["class ", "struct ", "interface ", "record ", "enum "]
                    .iter()
                    .any(|kw| text.contains(kw))
        })
}

/// A Java file that declares nothing another package can name.
///
/// `import a.b.C` of a package-private type does not compile, so the
/// only reference such a file can receive is from its own package —
/// which `fold_over_packages` has already credited by the time this is
/// asked. A zero here therefore means the PACKAGE is unreached, and the
/// package answers for itself through the public types it does declare:
/// netty's `io.netty.handler.pcap` is one public `PcapWriteHandler` and
/// six package-private packet writers, and reporting it seven times
/// states one fact seven ways.
///
/// The three shapes this covers are all build artifacts by their own
/// account: the seven shaded `DoNotRemove` classes say "Placeholder for
/// module-info maven plugin to add the package to the module-info
/// descriptor" in their only comment, the four `io.netty.util.internal.
/// svm` classes carry `@TargetClass` and are read by GraalVM's image
/// builder, and junit5's `eclipse-public-license-2.0.java` is a spotless
/// header template.
///
/// Zero-fan-in gated rather than a sink: 1227 of the gold Java corpus's
/// 4634 non-test files declare no public type, and all but TWENTY of
/// them are credited by their package before this is ever asked. Those
/// twenty are what leaves the population, and only twelve of them were
/// orphans — the other eight were reached by a test, which is why the
/// java corpus's `tested_only` falls 131 to 123 with the rule on.
fn package_private(f: &GraphFacts) -> bool {
    f.lang == crate::lang::Lang::Java && f.exports.is_empty()
}

/// A LuaCATS declaration file: `---@meta` marks a file as TYPES for a
/// runtime, never loaded as code.
///
/// The same argument `is_sink` already makes for a `.d.ts`, and the
/// marker is the language server's own. 50 files in gold carry it, 48
/// of them Lua -- lua-language-server's `meta/template/*.lua`, which
/// are declarations for `debug`, `ffi` and the rest of the standard
/// library.
///
/// The marker opens a COMMENT RUN rather than the file. `ffi.lua` line
/// one is `---#if not JIT then DISABLE() end` and `---@meta ffi` is
/// line two; `bit.lua`, `bit32.lua`, `jit*.lua`, `utf8.lua`,
/// `table.new/clear.lua` and `string.buffer.lua` are written the same
/// way, and reading line one alone missed all ten: 30 Lua files in gold
/// open a comment run holding the marker and only 20 of them open the
/// FILE with it. Only that leading run is scanned, so a `---@meta`
/// written below a line of code is that line's neighbour and not a
/// claim about the file.
fn declares_types_only(f: &GraphFacts) -> bool {
    use crate::lang::Lang;
    if f.lang != Lang::Lua {
        return false;
    }
    std::fs::read_to_string(&f.path).is_ok_and(|text| {
        text.lines()
            .map(str::trim)
            .take_while(|l| l.is_empty() || l.starts_with("--"))
            .any(|l| l.starts_with("---@meta"))
    })
}

/// A demo, a benchmark or a codegen script that NOTHING reaches. The
/// zero-fan-in condition is the whole rule: the claim is that being
/// unreferenced is not a finding for such a file, which says nothing
/// about one that is referenced. Without it the exclusion eats live
/// source — lua-language-server keeps its whole tree under `script/`
/// and kong a library under `kong/tools/`, 349 modules between them,
/// and dropping those from the population raised Lua's orphan rate
/// instead of lowering it.
fn runs_once(path: &Path) -> bool {
    let norm = crate::facts::rooted(&path.display().to_string());
    crate::facts::one_shot_dir(&norm) || glob_loaded(path) || sbt_meta_build(path)
}

/// An sbt BUILD DEFINITION. `project/` is compiled as a project of its
/// own and `build.sbt` is its only caller — zio's opens with `import
/// BuildHelper.*`, `import Dependencies.*` and `import
/// MimaSettings.mimaSettings`, and circe's writes `Boilerplate.gen(...)`
/// — but a `.sbt` file is not source this tool reads, so the three
/// imports are invisible and every helper reads as an orphan.
///
/// `project/build.properties` is what says sbt owns the directory: it
/// pins the launcher version and sbt refuses to build without it. The
/// gold corpus holds twelve of them and sixteen `.scala` files beneath;
/// the other four already reach each other, which the zero-fan-in gate
/// leaves alone.
fn sbt_meta_build(path: &Path) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    dir.file_name().and_then(|n| n.to_str()) == Some("project")
        && dir.join("build.properties").is_file()
}

/// Two filenames a toolchain reserves, matched WHOLE and never as a
/// stem: `docs/Makefile` runs sphinx-build over the directory holding
/// `conf.py` in attrs, rich, bats-core, AutoMapper and
/// transformer-engine, and `.readthedocs.yaml` names it in click and
/// rich; `setup.py` is what pip and setuptools run. Reading the stem
/// instead would have taken kong's `conf.lua` loader and vscode's
/// `setup.ts`, which are ordinary modules their neighbours import.
///
/// `*.gemspec` was the third and is deliberately absent: three files in
/// gold are one, and a build file in their own directory already names
/// two of them — sinatra's Rakefile writes `task 'rack-protection.gemspec'`
/// and `task 'sinatra-contrib.gemspec'`. A whole-extension exemption
/// worth one file is not worth its surface.
fn reserved(name: &str) -> bool {
    matches!(name, "conf.py" | "setup.py")
}

/// A gem's own entry points, named by the gemspec beside them.
///
/// `lib/<X>.rb` is what `require "<X>"` loads and no file inside the
/// gem requires it; `rubocop/rubocop.gemspec:18` sets
/// `s.bindir = 'exe'` and `:19` `s.executables = ['rubocop']`, and
/// `sequel/sequel.gemspec:24` says `'bin'` and pushes `'sequel'`. Four
/// files in gold, and every one of them is the file a user of the gem
/// reaches first.
///
/// The gemspec's FILENAME is the field, not its `name =` line:
/// sinatra's opens `Gem::Specification.new 'sinatra', version` and
/// never assigns a name at all, and rubygems requires the file to be
/// called after the gem it packages. For a script the gemspec has to
/// quote both the directory and the file, which is what keeps this
/// from exempting everything under a `bin/`.
fn gem_entry(path: &Path, name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".rb").or(Some(name)) else {
        return false;
    };
    let Some(dir) = path.parent().and_then(|d| d.file_name()?.to_str()) else {
        return false;
    };
    let Some(root) = path.parent().and_then(Path::parent) else {
        return false;
    };
    match dir {
        "lib" if name.ends_with(".rb") => root.join(format!("{stem}.gemspec")).is_file(),
        "bin" | "exe" => quoted_in_gemspec(root, dir, name),
        _ => false,
    }
}

/// Does the gemspec at this root quote both the directory and the file?
fn quoted_in_gemspec(root: &Path, dir: &str, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".gemspec"))
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .any(|text| {
            ['\'', '"'].iter().any(|q| {
                text.contains(&format!("{q}{dir}{q}")) && text.contains(&format!("{q}{name}{q}"))
            })
        })
}

/// A file a TOOL collects by pattern rather than one any code imports,
/// and the pattern is in the repository: primer-react's
/// `.storybook/main.ts` writes `stories: ['../src/**/*.stories.tsx']`,
/// and Figma's Code Connect claims `.figma.tsx` the same way. 338 of
/// gold tsx's 506 orphans carry one of the two markers.
///
/// The marker is an INNER dot-segment, so `Button.stories.tsx` matches
/// and a module honestly named `stories.tsx` does not.
fn glob_loaded(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let mut segments = name.split('.');
    segments.next();
    segments.next_back();
    // `config` joins them for the same reason: zod's package.json lists
    // vitest, tsdown and rolldown in devDependencies AND names each in
    // `scripts`, and every one of those tools reads its own config by
    // name. Zero-fan-in gated rather than a sink, because 55 of the
    // corpus's 132 config files DO have importers — trpc's per-package
    // `vitest.config.ts` files import a shared base.
    segments.any(|s| matches!(s, "stories" | "figma" | "config"))
}

/// A file at a package's ROOT whose first dot-segment names a package
/// that manifest chain declares as a dependency. The same argument the
/// `config` marker makes, with the tool named directly instead of
/// through a middle segment: vscode's extensions declare `esbuild` and
/// keep an `esbuild.mts` beside the manifest, which
/// `build/lib/extensions.ts` opens by that exact name
/// (`esbuildConfigFileName = forWeb ? 'esbuild.browser.mts' :
/// 'esbuild.mts'`). 42 gold orphans are one.
///
/// The package root is load-bearing and was measured: without it, a
/// `util` polyfill in vscode's devDependencies claims all 17 files
/// named `util.ts` in the tree — 284 matches and 47 orphans. With it
/// gold offers 177 matches, of which 173 are tool drivers; the other
/// four are preact demos named after the library they demonstrate
/// (`demo/mobx.jsx`, `demo/redux.jsx`, `demo/styled-components.jsx`,
/// `demo/zustand.jsx`). That is the rule's real shape — it keys on "a
/// declared dependency", not on "a tool" — and those four sit under a
/// `demo/` directory the one-shot rule already excludes.
///
/// `entryish` and not `glob_loaded`: the two mechanisms remove the same
/// orphan in every one of the 22 corpora, and the stronger one would
/// additionally take 63 files out of the judged population and exempt
/// them from the blocking-async check. Nothing is bought by it.
fn tool_driver(path: &Path) -> bool {
    let Some(dir) = path.parent().filter(|d| d.join("package.json").is_file()) else {
        return false;
    };
    let Some(stem) = first_segment(path).filter(|s| !s.is_empty()) else {
        return false;
    };
    dir.ancestors().any(|a| depends_on(a, &stem))
}

/// Does the manifest in this directory declare a package by this name?
fn depends_on(dir: &Path, name: &str) -> bool {
    package_entries(dir).is_some_and(|e| e.dependencies.iter().any(|d| **d == *name))
}

/// The judged population, and the share of it that nothing transitively
/// depends on.
fn deletability(blasts: &[u32], judged: &[bool]) -> (u32, f64) {
    let mut population = 0;
    let mut deletable = 0;
    for (blast, &counts) in blasts.iter().zip(judged) {
        population += counts as u32;
        deletable += (counts && *blast == 0) as u32;
    }
    match population {
        0 => (0, 0.0),
        n => (n, 100.0 * deletable as f64 / n as f64),
    }
}

/// Modules with no importers and no entry-point name: count plus a
/// capped, sorted sample. `judged` excludes the files whose fan-in the
/// language fixes at zero.
fn find_orphans(
    files: &[&GraphFacts],
    fan_in: &[u32],
    judged: &[bool],
    from_tests: &[u32],
) -> (u32, Vec<String>, u32) {
    let unreached = |i: usize| judged[i] && fan_in[i] == 0 && !entryish(&files[i].path);
    let mut orphans: Vec<String> = (0..files.len())
        .filter(|&i| unreached(i) && from_tests[i] == 0)
        .map(|i| files[i].path.display().to_string())
        .collect();
    // A file its own suite exercises and nothing else is a DIFFERENT
    // finding from one nothing references at all, and conflating them
    // is a false positive on public API: starlette's testclient.py has
    // 11 such importers, click's testing.py 8, and 88 of Solidity's 145
    // production orphans are imported by a mock or a harness. Counted,
    // not listed, because the reader's next question is how many.
    let tested = (0..files.len())
        .filter(|&i| unreached(i) && from_tests[i] > 0)
        .count() as u32;
    orphans.sort_unstable();
    let count = orphans.len() as u32;
    orphans.truncate(ORPHAN_SHOW);
    (count, orphans, tested)
}

/// Orphans and judged modules per language, worst rate first. Only
/// reported for a tree holding more than one language, because for a
/// single one it repeats the headline.
fn orphans_by_language(
    files: &[&GraphFacts],
    fan_in: &[u32],
    judged: &[bool],
    from_tests: &[u32],
) -> Vec<(&'static str, u32, u32)> {
    let mut tally: HashMap<&'static str, (u32, u32)> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if !judged[i] {
            continue;
        }
        let row = tally.entry(f.lang.name()).or_default();
        // The same question the headline asks, so it must make the same
        // two exclusions: an entry point, and a module its own suite
        // exercises. Without the second the rows summed to the headline
        // PLUS `tested_only` — php read 174 against a stated 56.
        let orphan = fan_in[i] == 0 && from_tests[i] == 0 && !entryish(&f.path);
        row.0 += u32::from(orphan);
        row.1 += 1;
    }
    if tally.len() < 2 {
        return Vec::new();
    }
    let mut rows: Vec<(&'static str, u32, u32)> =
        tally.into_iter().map(|(l, (o, n))| (l, o, n)).collect();
    rows.sort_by(|a, b| {
        (b.1 as u64 * a.2 as u64)
            .cmp(&(a.1 as u64 * b.2 as u64))
            .then_with(|| a.0.cmp(b.0))
    });
    rows
}

const UNSET: u32 = u32::MAX;

/// Iterative Tarjan SCC; components come out in reverse topological
/// order of the condensation.
struct Tarjan<'a> {
    succ: &'a [Vec<u32>],
    index: Vec<u32>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    comp: Vec<usize>,
    next_index: u32,
    ncomp: usize,
}

impl Tarjan<'_> {
    fn run(succ: &[Vec<u32>]) -> Vec<usize> {
        let n = succ.len();
        let mut t = Tarjan {
            succ,
            index: vec![UNSET; n],
            low: vec![0; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
            comp: vec![usize::MAX; n],
            next_index: 0,
            ncomp: 0,
        };
        for start in 0..n as u32 {
            if t.index[start as usize] == UNSET {
                t.explore(start);
            }
        }
        t.comp
    }

    /// Depth-first search with an explicit frame stack: (node,
    /// next-successor position).
    fn explore(&mut self, start: u32) {
        let mut frames: Vec<(u32, usize)> = vec![(start, 0)];
        while let Some(frame) = frames.last_mut() {
            let v = frame.0;
            if frame.1 == 0 {
                self.open(v);
            }
            match self.advance(v, &mut frame.1) {
                Some(w) => frames.push((w, 0)),
                None => self.retire(&mut frames),
            }
        }
    }

    fn open(&mut self, v: u32) {
        let vu = v as usize;
        self.index[vu] = self.next_index;
        self.low[vu] = self.next_index;
        self.next_index += 1;
        self.stack.push(v);
        self.on_stack[vu] = true;
    }

    /// Consume successors until one needs descending into; visited ones
    /// on the stack fold into v's low link.
    fn advance(&mut self, v: u32, pos: &mut usize) -> Option<u32> {
        let vu = v as usize;
        while let Some(&w) = self.succ[vu].get(*pos) {
            *pos += 1;
            let wu = w as usize;
            if self.index[wu] == UNSET {
                return Some(w);
            }
            if self.on_stack[wu] {
                self.low[vu] = self.low[vu].min(self.index[wu]);
            }
        }
        None
    }

    /// v is finished: close its SCC if it is a root, then pop the frame
    /// and propagate its low link to the parent.
    fn retire(&mut self, frames: &mut Vec<(u32, usize)>) {
        let (v, _) = frames.pop().expect("retire needs a frame");
        let vu = v as usize;
        if self.low[vu] == self.index[vu] {
            self.close(v);
        }
        if let Some(&(parent, _)) = frames.last() {
            let pu = parent as usize;
            self.low[pu] = self.low[pu].min(self.low[vu]);
        }
    }

    /// Pop the SCC rooted at v off the node stack.
    fn close(&mut self, v: u32) {
        loop {
            let w = self.stack.pop().expect("tarjan stack invariant");
            self.on_stack[w as usize] = false;
            self.comp[w as usize] = self.ncomp;
            if w == v {
                break;
            }
        }
        self.ncomp += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::fixture;
    use crate::lang::Lang;

    /// Structure-only tests: no symbol is mentioned anywhere.
    fn arch(files: &[GraphFacts]) -> Architecture {
        analyze(files, &Mentions::new()).expect("graph present")
    }

    /// Every listed name mentioned by two files — alive, so dead-export
    /// detection stays out of the way of the structural assertions.
    fn seen(names: &[&str]) -> Mentions {
        names.iter().map(|n| ((*n).into(), 2)).collect()
    }

    #[test]
    fn a_build_file_names_a_generator_and_a_shipping_list_names_nothing() {
        // curl's lib/Makefile.am:33 lists optiontable.pl and :182 runs
        // it; its projects/vms/Makefile.am reaches vms_eco_level.h only
        // through EXTRA_DIST, and that file is genuinely dead. Its
        // tests/Makefile.am hides eighteen more behind one indirection:
        // TESTSCRIPTS is spliced into EXTRA_DIST and nothing else.
        // ghostty's pkg/libintl/build.zig mentions config.h only inside
        // a `//!` doc comment.
        let dir = std::env::temp_dir().join(format!("elegance-built-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Makefile.am"),
            "# a comment naming commented.pl and nothing else\n\
             TESTSCRIPTS = \\\n  shipped.pl\n\
             EXTRA_DIST = \\\n  vms_eco_level.h \\\n  $(TESTSCRIPTS)\n\
             noinst_HEADERS = installed.h\n\
             optiontable:\n\t@PERL@ $(srcdir)/optiontable.pl > easyoptions.c\n",
        )
        .unwrap();
        std::fs::write(dir.join("build.zig"), "//! I generated config.h myself.\n").unwrap();
        let is_entry = |n: &str| entryish(&dir.join(n));

        assert!(is_entry("optiontable.pl"), "a recipe invokes it");
        // A distribution list, an install list, a variable spliced into
        // one, and a comment are all things that name a file without
        // anything happening to it.
        for quiet in [
            "vms_eco_level.h",
            "installed.h",
            "shipped.pl",
            "commented.pl",
            "config.h",
        ] {
            assert!(!is_entry(quiet), "{quiet} is named but not invoked");
        }
        // The name must stand on its own: a rule for `optiontable.pl`
        // says nothing about `table.pl`.
        assert!(is_entry("easyoptions.c"), "the recipe writes it");
        assert!(!is_entry("table.pl"), "optiontable.pl is not table.pl");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_js_of_ocaml_stub_is_a_linker_input_and_not_a_module() {
        // `core/core/src/dune` writes `(js_of_ocaml (javascript_files
        // strftime.js runtime.js timezone_js_loader_stubs.js
        // timezone_runtime.js))` and `(wasm_files ...)` beside it. A
        // linker input is not a module: no import in any language the
        // tool reads can name one, and all seven JavaScript orphans in
        // the OCaml corpus were these.
        //
        // The full NAME, where a module is listed under its stem — and
        // scoped to the stanza, so a filename mentioned anywhere else
        // in the dune file states nothing.
        let dir = std::env::temp_dir().join(format!("elegance-dune-js-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("dune"),
            "(library (name core) (preprocess (pps ppx))\n \
             (js_of_ocaml (javascript_files strftime.js runtime.js))\n \
             (wasm_files timezone.wasm.js))\n",
        )
        .unwrap();
        assert!(entryish(&dir.join("strftime.js")));
        assert!(entryish(&dir.join("runtime.js")));
        assert!(entryish(&dir.join("timezone.wasm.js")));
        assert!(!entryish(&dir.join("helper.js")), "an ordinary module");
        // A stanza that lists no JavaScript claims none of it.
        std::fs::write(dir.join("dune"), "(library (name core))\n").unwrap();
        assert!(!entryish(&dir.join("runtime.js")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_toolchain_reserves_two_filenames_whole() {
        // `docs/Makefile` runs sphinx-build over the directory holding
        // `conf.py` in attrs, rich, bats-core, AutoMapper and
        // transformer-engine; `.readthedocs.yaml` names it in click and
        // rich; `setup.py` is what pip runs. Whole filenames, so kong's
        // `conf.lua` loader and vscode's `setup.ts` are untouched.
        assert!(entryish(Path::new("rich/docs/source/conf.py")));
        assert!(entryish(Path::new("attrs/setup.py")));
        assert!(!entryish(Path::new("kong/kong/conf_loader/conf.lua")));
        assert!(!entryish(Path::new("vscode/src/setup.ts")));
    }

    #[test]
    fn a_gem_is_entered_through_the_files_its_gemspec_names() {
        // rubocop.gemspec:18 sets `s.bindir = 'exe'` and :19
        // `s.executables = ['rubocop']`; sinatra's gemspec opens
        // `Gem::Specification.new 'sinatra', version` and never assigns
        // a name at all, which is why `lib/<X>.rb` matches on the
        // gemspec FILE rather than on the field.
        let dir = std::env::temp_dir().join("elegance-gem-entry");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib/sinatra")).unwrap();
        std::fs::create_dir_all(dir.join("exe")).unwrap();
        std::fs::write(
            dir.join("sinatra.gemspec"),
            "Gem::Specification.new 'sinatra', version do |s|\n  s.bindir = 'exe'\n  s.executables = ['rubocop']\nend\n",
        )
        .unwrap();
        assert!(entryish(&dir.join("lib/sinatra.rb")));
        assert!(entryish(&dir.join("exe/rubocop")));
        // A module the gem merely ships is not its entry point, and a
        // script the gemspec does not quote is not an executable.
        assert!(!entryish(&dir.join("lib/sinatra/base.rb")));
        assert!(!entryish(&dir.join("exe/other")));
        assert!(!entryish(&dir.join("lib/rack-protection.rb")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_meta_marker_may_open_a_comment_run_rather_than_the_file() {
        // lua-language-server/meta/template/ffi.lua line one is
        // `---#if not JIT then DISABLE() end` and `---@meta ffi` is
        // line two; bit.lua, jit*.lua, utf8.lua, table.new/clear.lua
        // and string.buffer.lua are written the same way, and reading
        // line one alone missed all ten.
        let dir = std::env::temp_dir().join("elegance-lua-meta");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = "---#if not JIT then DISABLE() end\n---@meta ffi\n\nlocal ffi = {}\n";
        std::fs::write(dir.join("ffi.lua"), stub).unwrap();
        // A marker below the first line of CODE is a neighbour of that
        // code, not a claim about the file.
        std::fs::write(
            dir.join("mid.lua"),
            "local x = 1\n---@meta something\nreturn x\n",
        )
        .unwrap();
        let facts = |name: &str| GraphFacts {
            path: dir.join(name),
            lang: crate::lang::Lang::Lua,
            is_test: false,
            imports: Vec::new(),
            exports: Vec::new(),
            receiver_units: Vec::new(),
            mass: 0,
            surface_cost: 0,
        };
        assert!(declares_types_only(&facts("ffi.lua")));
        assert!(!declares_types_only(&facts("mid.lua")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_published_header_is_named_from_outside_the_repository() {
        // musl's Makefile installs every `$(srcdir)/include/%` and its
        // own syslog.c writes `#include <stdio.h>` for include/stdio.h,
        // so the specifier that reaches one is written relative to a
        // root the CONSUMER is handed. 66 orphans across six corpora.
        assert!(entryish(Path::new("musl/include/sys/vt.h")));
        assert!(entryish(Path::new("cutlass/include/cute/numeric/real.hpp")));
        // The tree is the claim, not the extension: a translation unit
        // under it is a sink already, and a header outside it is
        // published by nothing.
        assert!(!entryish(Path::new("musl/src/locale/big5.h")));
        assert!(!entryish(Path::new("vscode/src/vs/base/include.ts")));
    }

    #[test]
    fn a_variable_below_the_first_component_selects_between_include_roots() {
        // `-I$(srcdir)/arch/$(ARCH)`: the source cannot say which of the
        // seven ksigaction.h an include names, and `$(ARCH)` does.
        let arch = Path::new("arch/x32/ksigaction.h");
        assert!(under_variant(arch, "$(srcdir)/arch/$(ARCH)"));
        assert!(under_variant(
            Path::new("arch/arm/bits/ioctl_fix.h"),
            "$(srcdir)/arch/$(ARCH)"
        ));
        // A variable that is only the source-tree PREFIX selects
        // nothing, and 43 of the corpus's 44 variable-bearing `-I`
        // flags are exactly that. The plain root is the case that
        // matters: `-I$(srcdir)/arch` reaches the same files and states
        // nothing about which of them a build sees, and admitting it
        // would exempt every header under every `-I` in every C
        // repository rather than nine.
        assert!(!under_variant(arch, "$(srcdir)/arch"));
        assert!(!under_variant(arch, "$(srcdir)/include"));
        assert!(!under_variant(arch, "$(top_srcdir)/lib"));
        // A different root, and the root itself rather than a file in it.
        assert!(!under_variant(arch, "$(srcdir)/src/$(SUB)"));
        assert!(!under_variant(
            Path::new("arch/x32"),
            "$(srcdir)/arch/$(ARCH)"
        ));
    }

    #[test]
    fn a_build_file_declares_the_include_root_a_vendored_header_answers() {
        // ghostty's pkg/libintl/build.zig:42 writes
        // `addIncludePath(b.path(""))` for the package directory itself
        // and pkg/libxml2's for `override/config/posix`. The upstream
        // sources those headers substitute for are FETCHED, so nothing
        // in the repository includes one and the declaration is the
        // only statement there is. 5 orphans.
        let dir = std::env::temp_dir().join("elegance-zig-include-root");
        let _ = std::fs::create_dir_all(dir.join("override/config/posix"));
        let build = "pub fn build(b: *std.Build) void {\n    lib.addIncludePath(b.path(\"override/config/posix\"));\n}\n";
        std::fs::write(dir.join("build.zig"), build).unwrap();
        assert!(entryish(&dir.join("override/config/posix/config.h")));
        assert!(
            !entryish(&dir.join("override/config/win32/config.h")),
            "a root the build file does not name"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_xcode_application_target_takes_a_whole_directory() {
        // Xcode 16 hands a `PBXFileSystemSynchronizedRootGroup` to a
        // target with no per-file listing, and an application bundle is
        // a leaf nothing can import. The PRODUCT TYPE is the
        // discriminator: Alamofire declares `framework` for the same
        // shape and its 94 library files ARE importable.
        let app = r#"
		A5B30530299BEAAA0047F10C /* Ghostty */ = {
			isa = PBXNativeTarget;
			fileSystemSynchronizedGroups = (
				81F82BC72E82815D001EDFA7 /* Sources */,
			);
			productType = "com.apple.product-type.application";
		};
		A54F45F22E1F047A0046BD5C /* Lib */ = {
			isa = PBXNativeTarget;
			fileSystemSynchronizedGroups = (
				A54F45F42E1F047A0046BD5C /* Source */,
			);
			productType = "com.apple.product-type.framework";
		};
		81F82BC72E82815D001EDFA7 /* Sources */ = {isa = PBXFileSystemSynchronizedRootGroup; path = Sources; sourceTree = "<group>"; };
		A54F45F42E1F047A0046BD5C /* Source */ = {isa = PBXFileSystemSynchronizedRootGroup; path = Source; sourceTree = "<group>"; };
"#;
        assert_eq!(app_target_dirs(app), ["Sources"]);
        // A watch app and an app extension are leaves for the same
        // reason, which is why the product type is matched as a prefix.
        let watch = app.replace(
            "com.apple.product-type.application\"",
            "com.apple.product-type.application.watchapp2\"",
        );
        assert_eq!(app_target_dirs(&watch), ["Sources"]);
    }

    #[test]
    fn a_swiftpm_leaf_target_is_declared_by_its_manifest() {
        // A library target is reached by `import <name>` and the module
        // fold credits every file in it; an executable has no such name
        // and `@main` replaced the `main.swift` the fold used to key on.
        // 21 orphans. The target's own `name:` is read at ARGUMENT
        // level, so the nested `.product(name:)` in the dependency list
        // can never be mistaken for it.
        assert_eq!(
            target_name("name: \"stackdiff\", dependencies: [.product(name: \"NIO\")])"),
            Some("stackdiff")
        );
        assert_eq!(
            target_name("dependencies: [.product(name: \"NIO\")], name: \"Dev\")"),
            Some("Dev"),
            "a nested name is not the target's"
        );
        assert_eq!(target_name("dependencies: [])"), None);
        let dir = std::env::temp_dir().join("elegance-swiftpm-leaf");
        let _ = std::fs::create_dir_all(dir.join("Sources/stackdiff"));
        let manifest = "let package = Package(targets: [\n  .executableTarget(name: \"stackdiff\", dependencies: []),\n  .target(name: \"NIOCore\"),\n])\n";
        std::fs::write(dir.join("Package.swift"), manifest).unwrap();
        // Not `main.swift` -- that stem is an entry point on its own,
        // and `@main` is exactly what replaced it.
        assert!(entryish(&dir.join("Sources/stackdiff/StackDiff.swift")));
        assert!(
            !entryish(&dir.join("Sources/NIOCore/Channel.swift")),
            "a library target is reached by its import"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cargo_manifest_names_its_own_targets() {
        // `[[bin]] path = "fuzz_targets/fuzz_regex_match.rs"` — what
        // Cargo builds from one is an artefact nothing links against,
        // reached by a name only the manifest writes. 9 orphans.
        //
        // The enclosing TABLE is what separates that from
        // `[lib] path = "src/lib.rs"`, which names the crate root every
        // dependent `use`s; three manifests in gold write one.
        let dir = std::env::temp_dir().join("elegance-cargo-target");
        let _ = std::fs::create_dir_all(dir.join("fuzz_targets"));
        let _ = std::fs::create_dir_all(dir.join("src"));
        let manifest = "[package]\nname = \"regex-fuzz\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"m\"\npath = \"fuzz_targets/m.rs\"\n\n[dependencies]\nregex = { path = \"..\" }\n";
        std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        assert!(entryish(&dir.join("fuzz_targets/m.rs")));
        assert!(!entryish(&dir.join("fuzz_targets/other.rs")));
        // `src/lib.rs` is an entry point on its stem either way, so the
        // [lib] refusal is asserted where it lives.
        let text = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(declares_target(&text, "fuzz_targets/m.rs"));
        assert!(
            !declares_target(&text, "src/lib.rs"),
            "a [lib] path is the crate root, not a target artefact"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cfg_test_module_is_reached_by_the_suite_and_not_by_production() {
        // `rayon/src/iter/mod.rs:93` writes `#[cfg(test)]` then
        // `mod test;`. The pack declines the edge, and rightly — but
        // the file is not unreferenced either. 6 orphans, routed to
        // `tested_only` where they belong.
        assert!(declared_cfg_test("#[cfg(test)]\nmod test;\n", "test"));
        assert!(declared_cfg_test(
            "mod a;\n#[cfg(test)]\npub mod testutil;\n",
            "testutil"
        ));
        assert!(!declared_cfg_test(
            "mod lines;\nmod testutil;\n",
            "testutil"
        ));
        assert!(!declared_cfg_test("#[cfg(test)]\nmod test;\n", "lines"));
    }

    #[test]
    fn a_jvm_package_holding_a_main_is_reached_and_so_are_its_helpers() {
        // `java io.netty.testsuite.autobahn.AutobahnServer` runs a class
        // the tree never names, and the handler and initializer beside
        // it are same-package references with no import to resolve. 19
        // of gold Java's 65 orphans were one package away from a `main`,
        // across five netty test suites and its GraalVM image checks.
        let entry = |path: &str| {
            let mut f = fixture(Lang::Java, path, &[]);
            f.exports = vec!["Server".into(), "main".into()];
            f
        };
        let held = |path: &str| {
            let mut f = fixture(Lang::Java, path, &[]);
            f.exports = vec!["Type".into()];
            f
        };
        let mut root = fixture(
            Lang::Java,
            "suite/src/main/java/io/netty/app/Root.java",
            &["io.netty.autobahn.Server"],
        );
        root.exports = vec!["Root".into(), "main".into()];
        let files = [
            entry("suite/src/main/java/io/netty/autobahn/Server.java"),
            held("suite/src/main/java/io/netty/autobahn/Handler.java"),
            held("suite/src/main/java/io/netty/lonely/Lonely.java"),
            root,
        ];
        let arch = arch(&files);
        // Only the package with no entry point and no importer is left.
        assert_eq!(
            arch.orphans,
            ["suite/src/main/java/io/netty/lonely/Lonely.java"]
        );
    }

    #[test]
    fn a_test_in_the_package_it_exercises_states_no_import() {
        // `src/test/java/io/netty/handler/pcap/PcapWriteHandlerTest.java`
        // declares package `io.netty.handler.pcap` and names
        // `PcapWriteHandler` bare — Java's default access is exactly
        // what a same-package test is for. Fourteen of gold Java's
        // twenty remaining orphans and 58 of Scala's 83 had a test in
        // their own package and no other reader.
        let mut suite = fixture(Lang::Java, "m/src/test/java/io/pcap/WriterTest.java", &[]);
        suite.is_test = true;
        let public = |path: &str, imports: &[&str]| {
            let mut f = fixture(Lang::Java, path, imports);
            f.exports = vec!["Type".into()];
            f
        };
        let mut root = public("m/src/main/java/io/app/Root.java", &["io.other.Used"]);
        root.exports = vec!["Root".into(), "main".into()];
        let files = [
            suite,
            public("m/src/main/java/io/pcap/Writer.java", &[]),
            public("m/src/main/java/io/other/Used.java", &[]),
            root,
            public("m/src/main/java/io/dead/Gone.java", &[]),
        ];
        let arch = arch(&files);
        // The tested package is `tested_only`, which is a different
        // finding from being reached by nobody.
        assert_eq!(arch.orphans, ["m/src/main/java/io/dead/Gone.java"]);
        assert_eq!(arch.tested_only, 1);
    }

    #[test]
    fn an_entry_point_in_the_unnamed_package_answers_for_itself_alone() {
        // A package key is what lies BELOW the source root, and a file
        // sitting directly on one has nothing below it. Every such file
        // in a scan shares that empty key whatever build it came from:
        // tigerbeetle's four `samples/*/src/main/java/Main.java` credited
        // the java client's own root files, and zio's `zio-docs/src/main/
        // scala/utils.scala` was credited by a spec under
        // `streams-tests` — a different sbt project.
        let mut entry = fixture(
            Lang::Java,
            "samples/basic/src/main/java/Main.java",
            &["io.lib.Helper"],
        );
        entry.exports = vec!["Main".into(), "main".into()];
        let mut helper = fixture(Lang::Java, "lib/src/main/java/io/lib/Helper.java", &[]);
        helper.exports = vec!["Helper".into()];
        let mut other = fixture(Lang::Java, "client/src/main/java/Client.java", &[]);
        other.exports = vec!["Client".into()];
        let arch = arch(&[entry, helper, other]);
        // The launcher names the FILE, so an entry point is reached
        // whether it declares a package or not — and it answers for
        // nobody else.
        assert_eq!(arch.orphans, ["client/src/main/java/Client.java"]);
    }

    #[test]
    fn a_java_file_declaring_no_importable_type_is_not_asked_who_imports_it() {
        // `import a.b.C` of a package-private type does not compile, so
        // the only reference such a file can receive is same-package,
        // which `fold_over_packages` has already credited by the time
        // this is asked. netty's seven shaded `DoNotRemove` classes say
        // it themselves: "Placeholder for module-info maven plugin to
        // add the package to the module-info descriptor".
        let public = |path: &str, imports: &[&str]| {
            let mut f = fixture(Lang::Java, path, imports);
            f.exports = vec!["Type".into()];
            f
        };
        let hidden = |path: &str| fixture(Lang::Java, path, &[]);
        let mut root = public("m/src/main/java/io/app/Root.java", &["io.live.Live"]);
        root.exports = vec!["Root".into(), "main".into()];
        let files = [
            root,
            public("m/src/main/java/io/live/Live.java", &[]),
            hidden("m/src/main/java/io/live/Helper.java"),
            public("m/src/main/java/io/dead/Dead.java", &[]),
            hidden("m/src/main/java/io/dead/DoNotRemove.java"),
        ];
        let arch = arch(&files);
        // The dead package answers ONCE, through the public type it
        // declares; its hidden half states the same fact a second time.
        assert_eq!(arch.orphans, ["m/src/main/java/io/dead/Dead.java"]);
        // Zero-fan-in gated, not a sink: the live package's hidden half
        // is credited by its package and stays in the population. 611 of
        // gold Java's 4406 production files declare no public type.
        assert_eq!(arch.judged_modules, 4);
    }

    #[test]
    fn a_dotted_directory_below_the_source_root_is_a_package_written_flat() {
        let pkg = |p: &str| package_of(Path::new(p));
        // zio files its Scala-2 stream sources under a directory named
        // `zio.stream`, and the first line of the file is `package
        // zio.stream`; the sibling implementations live under
        // `scala/zio/stream/`.
        let flat = "zio/streams/shared/src/main/scala-2/zio.stream/ZStreamVersionSpecific.scala";
        assert_eq!(pkg(flat).as_deref(), Some("zio/stream"));
        // Only BELOW the root. `scala-2.13+` is a SOURCE SET, and
        // splitting the whole path instead shifted the root offset and
        // put 47 Scala modules — every file under
        // cats/core/src/main/scala-2.13+ among them — in a package of
        // their own.
        let set = "cats/core/src/main/scala-2.13+/cats/compat/Seq.scala";
        assert_eq!(pkg(set).as_deref(), Some("cats/compat"));
        // Nothing below the root is the UNNAMED package, which groups
        // nothing at all.
        assert_eq!(pkg("zio/zio-docs/src/main/scala/utils.scala"), None);
    }

    #[test]
    fn an_sbt_build_definition_is_the_build_rather_than_a_module() {
        // zio/build.sbt opens `import BuildHelper.*`, `import
        // Dependencies.*` and `import MimaSettings.mimaSettings`, and
        // `.sbt` is not a language this tool reads — so all three are
        // invisible and every helper reads as an orphan.
        // `project/build.properties` is sbt's launcher-version pin and
        // is what says sbt compiles the directory.
        let dir = std::env::temp_dir().join("elegance-sbt-meta/project");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("build.properties"), "sbt.version=1.9.7\n").unwrap();
        assert!(runs_once(&dir.join("BuildHelper.scala")));
        // Without the pin `project` is an ordinary package name.
        std::fs::remove_file(dir.join("build.properties")).unwrap();
        assert!(!runs_once(&dir.join("BuildHelper.scala")));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn a_csharp_file_of_nothing_but_assembly_attributes_states_no_module() {
        // `Lang::is_sink` already makes this argument for the file NAMED
        // `AssemblyInfo.cs`; the content is what that name stood in for,
        // because FluentValidation writes `AssemblyInfo.FluentValidation
        // .cs` and Dapper writes `Global.cs`.
        let dir = std::env::temp_dir().join("elegance-assembly-attrs");
        let _ = std::fs::create_dir_all(&dir);
        let write = |name: &str, text: &str| {
            std::fs::write(dir.join(name), text).unwrap();
            dir.join(name).display().to_string()
        };
        let meta = write("Global.cs", "[assembly: InternalsVisibleTo(\"Tests\")]\n");
        let live = write(
            "Live.cs",
            "[assembly: CLSCompliant(true)]\npublic class Live { }\n",
        );
        let used = write("Used.cs", "public class Used { }\n");
        let mut kept = fixture(Lang::CSharp, &live, &["Used"]);
        kept.exports = vec!["Live".into()];
        let mut target = fixture(Lang::CSharp, &used, &[]);
        target.exports = vec!["Used".into()];
        let arch = arch(&[fixture(Lang::CSharp, &meta, &[]), kept, target]);
        // A file that also declares a type is an ordinary module and
        // stays measurable.
        assert_eq!(arch.orphans, [live]);
        assert_eq!(arch.judged_modules, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deliberate_cycle_is_detected_with_members() {
        let files = [
            fixture(Lang::Python, "app/a.py", &[".b"]),
            fixture(Lang::Python, "app/b.py", &[".c"]),
            fixture(Lang::Python, "app/c.py", &[".a"]),
            fixture(Lang::Python, "app/leaf.py", &[".a"]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.cycle_mass_pct, 75.0);
        assert_eq!(arch.largest_cycle_size, 3);
        assert_eq!(
            arch.largest_cycle,
            ["app/a.py", "app/b.py", "app/c.py"],
            "cycle members listed"
        );
        // One directory: internal wiring, no architectural cycle.
        assert_eq!(arch.dir_cycle_mass_pct, 0.0);
    }

    #[test]
    fn cross_directory_cycles_are_the_architectural_finding() {
        let files = [
            fixture(Lang::Python, "app/core/engine.py", &["app.ui.window"]),
            fixture(Lang::Python, "app/ui/window.py", &["app.core.engine"]),
            fixture(Lang::Python, "app/util/log.py", &[]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.dir_cycle_mass_pct, 100.0 * 2.0 / 3.0);
        assert_eq!(arch.largest_dir_cycle, ["app/core", "app/ui"]);
    }

    #[test]
    fn a_module_directory_answers_for_every_file_inside_it() {
        // Alamofire's Source/Core/AFError.swift is referenced by 24
        // files in its own target and imported by name from none of
        // them, because Swift has no syntax that would say so. At file
        // granularity 98% of Swift was orphaned by construction, and Go
        // sat at 52% for the same reason one tier milder.
        //
        // The odd target out is `Reporter` and not a `Demo`: a demo
        // target joined the one-shot directories, so it would leave the
        // population here for a reason that has nothing to do with the
        // fold this test is about.
        let files = [
            fixture(Lang::Swift, "Sources/App/main.swift", &["NIOCore"]),
            fixture(Lang::Swift, "Sources/Reporter/Server.swift", &["NIOCore"]),
            fixture(
                Lang::Swift,
                "Sources/NIOCore/AsyncChannel/Inbound.swift",
                &[],
            ),
            fixture(Lang::Swift, "Sources/NIOCore/Channel.swift", &[]),
        ];
        let arch = arch(&files);
        // The import lands on one file of NIOCore; the other is a
        // directory deeper and belongs to the same target either way.
        // Nothing imports Reporter, and that is the answer worth reading.
        assert_eq!(arch.orphans, ["Sources/Reporter/Server.swift"]);
        assert_eq!(arch.orphan_count, 1);
    }

    #[test]
    fn a_file_a_tool_collects_by_pattern_is_not_asked_who_imports_it() {
        // primer-react's `.storybook/main.ts` writes
        // `stories: ['../src/**/*.stories.tsx']`, so the glob is IN the
        // repository; Figma's Code Connect claims `.figma.tsx` the same
        // way. And Next.js reserves eight filenames inside an `app/`
        // tree, loading each by name. 416 of gold tsx's 506 orphans are
        // one of the three, and none of them has an import to fix.
        let files = [
            fixture(Lang::Tsx, "src/Button.tsx", &["./Spinner"]),
            fixture(Lang::Tsx, "src/Spinner.tsx", &[]),
            fixture(Lang::Tsx, "src/Button.stories.tsx", &["./Button"]),
            fixture(Lang::Tsx, "src/Button.figma.tsx", &["./Button"]),
            fixture(Lang::Tsx, "app/docs/page.tsx", &["./Button"]),
            fixture(Lang::Tsx, "src/stories.tsx", &[]),
            fixture(Lang::Tsx, "src/layout.tsx", &[]),
        ];
        let arch = arch(&files);
        // The two glob-loaded files leave the population; the route
        // file stays in it and stops counting as unreferenced.
        assert_eq!(arch.judged_modules, 5);
        // A module honestly NAMED stories.tsx is not one, and `layout`
        // outside a route tree is an ordinary module — vscode ships two.
        assert_eq!(arch.orphans, ["src/layout.tsx", "src/stories.tsx"]);
    }

    #[test]
    fn the_per_language_rows_answer_the_same_question_as_the_headline() {
        // The rows summed to the headline PLUS `tested_only`, because
        // they made one of its two exclusions and not the other. Gold
        // php reported 174 orphans across its rows against a stated 56,
        // and a reader who trusted the breakdown read a rate three
        // times the one the tool had just printed.
        let mut suite = fixture(Lang::Python, "tests/test_client.py", &["app.testing"]);
        suite.is_test = true;
        let files = [
            suite,
            fixture(Lang::Python, "app/main.py", &["app.core"]),
            fixture(Lang::Python, "app/core.py", &[]),
            fixture(Lang::Python, "app/testing.py", &[]),
            fixture(Lang::Python, "app/unused.py", &[]),
            fixture(Lang::Ruby, "lib/unused.rb", &[]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.orphan_count, 2, "app/unused.py and lib/unused.rb");
        assert_eq!(
            arch.tested_only, 1,
            "app/testing.py, exercised by its suite"
        );
        let rows: Vec<_> = arch
            .by_language
            .iter()
            .map(|&(l, o, n)| (l, o, n))
            .collect();
        assert_eq!(rows, [("rb", 1, 1), ("py", 1, 4)]);
        let summed: u32 = arch.by_language.iter().map(|r| r.1).sum();
        assert_eq!(summed, arch.orphan_count);
    }

    #[test]
    fn a_one_shot_name_is_a_directory_or_a_module_suffixed_with_one() {
        // `zio-examples`, `rayon-demo` and `cuda-samples` are the same
        // thing as `examples/` spelled the way a build tool names a
        // module, and the list only ever matched a whole component.
        let files = [
            fixture(Lang::Rust, "src/lib.rs", &["crate::core"]),
            fixture(Lang::Rust, "src/core.rs", &[]),
            fixture(Lang::Rust, "rayon-demo/src/quicksort.rs", &[]),
            // The file's own name is never tested, or this would go too.
            fixture(Lang::Rust, "src/parse-demo.rs", &[]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.judged_modules, 3);
        assert_eq!(arch.orphans, ["src/parse-demo.rs"]);
    }

    #[test]
    fn a_test_reaching_a_module_reaches_every_file_of_it() {
        // `fan_in` was folded over the module and `from_tests` was not,
        // so a test importing a Go PACKAGE credited only the file
        // standing for it. go-cmp's `internal/teststructs/` is exactly
        // that: `compare_test.go` imports the package, and the files the
        // representative did not stand for read as reached by nobody.
        // 11 of Go's 15 remaining orphans were this one gap.
        const ROOT: &str = "github.com/google/go-cmp/cmp";
        let mut suite = fixture(
            Lang::Go,
            "cmp/compare_test.go",
            &[&format!("{ROOT}/internal/teststructs")],
        );
        suite.is_test = true;
        let files = [
            suite,
            fixture(
                Lang::Go,
                "cmd/dump/main.go",
                &[&format!("{ROOT}/internal/value")],
            ),
            fixture(Lang::Go, "cmp/internal/value/value.go", &[]),
            fixture(Lang::Go, "cmp/internal/teststructs/structs.go", &[]),
            fixture(Lang::Go, "cmp/internal/teststructs/project1.go", &[]),
            fixture(Lang::Go, "cmp/unused/gone.go", &[]),
        ];
        let arch = arch(&files);
        // Neither teststructs file is an orphan, and both are counted
        // as reached by a test rather than by production code.
        assert_eq!(arch.orphans, ["cmp/unused/gone.go"]);
        assert_eq!(arch.tested_only, 2);
    }

    #[test]
    fn a_file_its_own_toolchain_loads_is_not_asked_who_requires_it() {
        // rubocop's Rakefile writes `Dir['tasks/**/*.rake'].each { |t|
        // load t }`, so the glob is in the repository; Mix evaluates
        // `config/*.exs`, and all eight in the corpus open with
        // `import Config`. Neither is ever named by a require.
        let files = [
            fixture(Lang::Ruby, "lib/rubocop/cop.rb", &["rubocop/version"]),
            fixture(Lang::Ruby, "lib/rubocop/version.rb", &[]),
            fixture(Lang::Ruby, "tasks/cut_release.rake", &[]),
            fixture(Lang::Elixir, "config/config.exs", &[]),
            // An `.exs` elsewhere is an ordinary script and stays
            // judged: `config/` is the whole of the claim.
            fixture(Lang::Elixir, "priv/seeds.exs", &[]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.judged_modules, 3);
        assert_eq!(arch.orphans, ["lib/rubocop/cop.rb", "priv/seeds.exs"]);
    }

    #[test]
    fn a_mix_task_is_reached_by_its_name_and_never_by_an_import() {
        // `mix phx.server` resolves `Mix.Tasks.Phx.Server` from the
        // module name, so nothing imports a task. 33 of gold Elixir's
        // 93 orphans were one, and the corpus holds 35 files under
        // `mix/tasks/` against 35 declaring `Mix.Tasks.` — the same 35,
        // so the path answers as precisely as the declaration.
        let files = [
            fixture(
                Lang::Elixir,
                "lib/mix/tasks/phx.server.ex",
                &["Phoenix.Endpoint"],
            ),
            fixture(Lang::Elixir, "lib/phoenix/endpoint.ex", &[]),
            fixture(Lang::Elixir, "lib/phoenix/unused.ex", &[]),
        ];
        let arch = arch(&files);
        // It stays in the population: a task is a real module, and the
        // claim is only that having no importer is not a finding for
        // one. `mix phx.server` starts a server and waits, so the
        // stronger one-shot claim would be the wrong one to make.
        assert_eq!(arch.judged_modules, 3);
        assert_eq!(arch.orphans, ["lib/phoenix/unused.ex"]);
    }

    #[test]
    fn a_manifest_names_the_module_its_library_is_reached_through() {
        // `containers/src/core/dune` says `(name containers)`, and
        // `containers.ml` is what a consumer opens — nothing inside the
        // project imports it, exactly as nothing imports a `lib.rs`.
        // The name is READ rather than guessed: a library's module is
        // called after the library, not after the directory holding it,
        // and `containers_cbor.ml` sits in `src/cbor`.
        let dir = std::env::temp_dir().join("elegance-dune-root");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("dune"), "(library (name containers_cbor))\n").unwrap();
        assert!(entryish(&dir.join("containers_cbor.ml")));
        assert!(!entryish(&dir.join("encode.ml")), "an ordinary module");
        // `(names ...)` is the plural, and core's `bench-bin/dune`
        // declares twelve executables in one stanza — each an entry
        // point, and 23 of OCaml's orphans were among them.
        let many = "(executables (modes byte exe) (names array_iter bench_hashtbl))\n";
        std::fs::write(dir.join("dune"), many).unwrap();
        assert!(entryish(&dir.join("array_iter.ml")));
        assert!(entryish(&dir.join("bench_hashtbl.ml")));
        assert!(!entryish(&dir.join("byte.ml")), "a mode is not a module");
        // The declaration is what answers, so a file whose stem matches
        // nothing the manifest names is judged.
        std::fs::write(dir.join("dune"), "(library (name other))\n").unwrap();
        assert!(!entryish(&dir.join("containers_cbor.ml")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_info_declares_no_type_so_no_import_can_name_it() {
        // `package-info.java` carries a package's annotations and its
        // Javadoc; `import io.netty.buffer.package-info` is not legal
        // syntax. 89 sat in the gold orphan list by construction, 53 of
        // them netty's.
        let public = |path: &str, imports: &[&str]| {
            let mut f = fixture(Lang::Java, path, imports);
            // A public type is what keeps a file in the population; see
            // `package_private`.
            f.exports = vec!["Type".into()];
            f
        };
        let files = [
            public("io/netty/buffer/ByteBuf.java", &["io.netty.util.Recycler"]),
            public("io/netty/util/Recycler.java", &[]),
            fixture(Lang::Java, "io/netty/util/package-info.java", &[]),
            public("io/netty/unused/Unused.java", &[]),
            fixture(Lang::Java, "io/netty/unused/package-info.java", &[]),
        ];
        let arch = arch(&files);
        // Both leave the population, whether or not their own package
        // is depended on — the package fold fills the zero for `util`
        // and not for `unused`, and neither answer is the one to give.
        assert_eq!(arch.judged_modules, 3);
        assert_eq!(
            arch.orphans,
            [
                "io/netty/buffer/ByteBuf.java",
                "io/netty/unused/Unused.java"
            ]
        );
    }

    #[test]
    fn a_translation_unit_is_not_asked_whether_anything_depends_on_it() {
        // kakoune's src/buffer.hh is included by 13 files and buffer.cc
        // by none, because nothing ever includes a .cc. They are one
        // module and the metric split it, calling half of it dead — 57
        // of kakoune's 58 translation units are that exact shape, and
        // 3437 of 3883 across the three C-family corpora.
        //
        // The .cc still supplies the edge that makes the .hh live.
        let files = [
            fixture(Lang::Cpp, "src/buffer.cc", &["buffer.hh"]),
            fixture(Lang::Cpp, "src/buffer.hh", &[]),
            fixture(Lang::Cpp, "src/unused.hh", &[]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.modules, 3);
        assert_eq!(arch.judged_modules, 2, "the .cc is a sink by construction");
        assert_eq!(arch.orphan_count, 1);
        assert_eq!(arch.orphans, ["src/unused.hh"]);
        assert_eq!(arch.deletable_pct, 50.0, "one of two headers");
    }

    #[test]
    fn chains_yield_depth_blast_and_deletability() {
        // top -> mid -> base; sibling also -> base.
        let files = [
            fixture(Lang::Python, "app/top.py", &[".mid"]),
            fixture(Lang::Python, "app/mid.py", &[".base"]),
            fixture(Lang::Python, "app/base.py", &[]),
            fixture(Lang::Python, "app/sibling.py", &[".base"]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.cycle_mass_pct, 0.0);
        assert_eq!(arch.depth_max, 3, "top -> mid -> base");
        // base is depended on by top, mid, sibling; top and sibling by
        // nobody.
        assert_eq!(arch.load_bearing[0], ("app/base.py".to_string(), 3));
        assert_eq!(arch.deletable_pct, 50.0);
        // top and sibling have no importers and non-entry names.
        assert_eq!(arch.orphans, ["app/sibling.py", "app/top.py"]);
    }

    #[test]
    fn shallow_wide_modules_surface_with_their_mass() {
        // A deep engine (huge body, 3-unit surface) vs a wide barrel
        // (12-unit surface, almost no body).
        let mut deep = fixture(Lang::Python, "app/engine.py", &[]);
        deep.exports = vec!["run".into()];
        deep.mass = 4000;
        deep.surface_cost = 3;
        let mut barrel = fixture(Lang::Python, "app/api.py", &[".engine"]);
        for i in 0..10 {
            barrel.exports.push(format!("f{i}").into());
        }
        barrel.mass = 30;
        barrel.surface_cost = 12;
        let arch = arch(&[deep, barrel]);
        let i = &arch.interfaces;
        assert_eq!(i.median_depth, 1333, "engine hides 1333 mass per unit");
        assert_eq!(i.shallow[0].0, "app/api.py", "barrel is the shallow one");
        assert_eq!(i.shallow[0].1, 2, "30 mass / 12 surface");
    }

    #[test]
    fn fat_surfaces_and_underscore_leaks_are_reported() {
        let mut wide = fixture(Lang::Python, "lib/pkg/util.py", &[]);
        wide.exports = ["a", "b", "c", "d", "e", "f", "g", "h"]
            .iter()
            .map(|s| (*s).into())
            .collect();
        wide.mass = 100;
        wide.surface_cost = 8;
        let mut user = fixture(Lang::Python, "app/handlers.py", &[]);
        user.imports = vec![crate::facts::ImportFact {
            target: "pkg.util".into(),
            names: vec!["a".into(), "_secret".into()],
            reach: crate::lang::Reach::Anywhere,
        }];
        let arch = analyze(
            &[wide, user],
            &seen(&["a", "b", "c", "d", "e", "f", "g", "h"]),
        )
        .expect("graph present");
        let i = &arch.interfaces;
        // Only `a` of 8 exports is ever imported by name (_secret is not
        // part of the surface — it is a leak).
        assert_eq!(i.fat[0], ("lib/pkg/util.py".to_string(), 1, 8));
        assert_eq!(i.leak_count, 1);
        assert_eq!(
            i.leaks[0], "app/handlers.py <- lib/pkg/util.py._secret",
            "cross-package underscore import is content coupling"
        );
    }

    #[test]
    fn dead_exports_are_found_by_mention_not_by_import() {
        // Go and C never import names, so "exported but not in anyone's
        // import list" would condemn every export they have. The evidence
        // is how many FILES mention the identifier.
        let mut lib = fixture(Lang::Go, "app/store/store.go", &[]);
        lib.exports = ["Open", "Close", "Vacuum"]
            .iter()
            .map(|s| (*s).into())
            .collect();
        let user = fixture(Lang::Go, "app/cmd/run.go", &["github.com/x/app/store"]);

        // Open and Close are named by two files; Vacuum only by its own.
        let mentions = seen(&["Open", "Close"])
            .into_iter()
            .chain([("Vacuum".into(), 1)])
            .collect();
        let a = analyze(&[lib, user], &mentions).expect("graph present");
        assert_eq!(a.interfaces.dead_count, 1);
        assert_eq!(a.interfaces.dead, ["app/store/store.go.Vacuum"]);
    }

    #[test]
    fn a_declared_api_surface_is_never_dead() {
        // A library's public API is legitimately unreferenced inside its
        // own repository; condemning it would make the finding useless
        // for exactly the codebases that publish the most surface.
        let mut api = fixture(Lang::Python, "pkg/__init__.py", &[".core"]);
        api.exports = vec!["public_thing".into()];
        let mut core = fixture(Lang::Python, "pkg/core.py", &[]);
        core.exports = vec!["internal_thing".into()];
        let a = analyze(&[api, core], &Mentions::new()).expect("graph present");
        assert_eq!(
            a.interfaces.dead,
            ["pkg/core.py.internal_thing"],
            "__init__ exempt, the module behind it is not"
        );
    }

    #[test]
    fn two_member_cycle_blast_counts_the_partner() {
        let files = [
            fixture(Lang::Python, "app/x.py", &[".y"]),
            fixture(Lang::Python, "app/y.py", &[".x"]),
        ];
        let arch = arch(&files);
        assert_eq!(arch.cycle_mass_pct, 100.0);
        // Each member's deletion breaks the other: blast 1 for both.
        assert_eq!(arch.deletable_pct, 0.0);
        assert!(arch.load_bearing.iter().all(|(_, b)| *b == 1));
    }
}
