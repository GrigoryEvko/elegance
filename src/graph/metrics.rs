//! Rung-5 architecture metrics over the resolved module graph. These are
//! distributional REPORTS, never CI gates: they describe the shape of the
//! dependency structure so a human can judge it.
//!
//! Parnas 1979: a correct uses-hierarchy is acyclic and subsettable —
//! cycle mass is the single best size-free architecture-health ratio.
//! Deletability is the underrated half of evolvability: code you can
//! delete never complected itself into its neighbors.

use std::collections::{HashMap, HashSet};
use std::path::Path;

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
        || routed(path, name, stem)
        || dune_root(path, name, stem)
        || mix_task(path)
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
    if !name.ends_with(".ml") && !name.ends_with(".mli") {
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
    ["(name ", "(public_name ", "(names "]
        .iter()
        .flat_map(|field| text.match_indices(field).map(|(at, f)| at + f.len()))
        .filter_map(|at| text[at..].split(')').next())
        .flat_map(str::split_whitespace)
        .any(|declared| declared.rsplit('.').next() == Some(stem))
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
            !(sink || reached == 0 && (runs_once(&f.path) || package_private(f)))
        })
        .collect()
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

/// A LuaCATS declaration file: `---@meta` on line one marks a file as
/// TYPES for a runtime, never loaded as code.
///
/// The same argument `is_sink` already makes for a `.d.ts`, and the
/// marker is the language server's own. 36 files carry it, 35 of them
/// Lua, and 19 read as orphans -- lua-language-server's
/// `meta/template/*.lua`, which are declarations for `debug`, `ffi` and
/// the rest of the standard library.
fn declares_types_only(f: &GraphFacts) -> bool {
    use crate::lang::Lang;
    if f.lang != Lang::Lua {
        return false;
    }
    std::fs::read_to_string(&f.path).is_ok_and(|text| {
        text.lines()
            .next()
            .is_some_and(|l| l.starts_with("---@meta"))
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
    crate::facts::one_shot_dir(&norm) || glob_loaded(path)
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
