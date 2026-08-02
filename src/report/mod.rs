//! Aggregation and rendering. Distributions are summarized by tails, never
//! means — a codebase is as bad as the code you're forced to read most often.
//! Offender lists hold only budget violations, so clean code reports quietly.

mod json;
mod sarif;
pub mod suggest;

pub use json::render_json;
pub use sarif::render as render_sarif;

use std::collections::HashMap;
use std::fmt::Write;

use crate::facts::FileFacts;
use crate::lang::LANGS;
use crate::metrics::{self, Fmt, LangBudgets, METRICS, N};

/// Offenders kept per metric for display; machine output keeps them all.
const KEEP: usize = 64;

/// Example sites retained per parameter clump.
const CLUMP_SITES: usize = 4;

/// A parameter group must recur this often to be a clump.
const CLUMP_MIN: u32 = 3;

/// Below this many measurements a percentile is noise, not a position.
const MIN_POSITION_SAMPLES: usize = 200;

/// The tail quantiles every distribution is summarized by. p99 is also
/// what budgets are pinned to, so the report shows the number the
/// calibration argues about.
pub(super) const P50: f64 = 0.50;
pub(super) const P90: f64 = 0.90;
pub(super) const P99: f64 = 0.99;

struct Offender {
    value: f32,
    path: String,
    line: u32,
    name: String,
}

impl Offender {
    fn label(&self) -> String {
        if !self.name.is_empty() {
            format!("{}:{}  {}", self.path, self.line, self.name)
        } else if self.line > 1 {
            // File-level finding with a meaningful anchor line.
            format!("{}:{}", self.path, self.line)
        } else {
            self.path.clone()
        }
    }
}

/// Total order for offenders: worst value first, then location — rendering
/// must not depend on rayon's merge order.
fn worse(a: &Offender, b: &Offender) -> std::cmp::Ordering {
    b.value
        .total_cmp(&a.value)
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a.line.cmp(&b.line))
        .then_with(|| a.name.cmp(&b.name))
}

struct Clump {
    count: u32,
    /// A few example units, "path:line name".
    sites: Vec<String>,
}

/// A normalized subtree hash and everywhere it occurred.
///
/// Most candidate subtrees occur exactly once, so storing the first site
/// INLINE to spare those a Vec allocation is the obvious diet — and it
/// measured 7% WORSE (957 MB -> 1028 MB). Widening the struct by 24
/// bytes grows hashbrown's contiguous table across every one of the
/// millions of entries, which costs more than the allocations it saves.
/// Left as it was, deliberately.
struct CloneClass {
    mass: u32,
    sites: Vec<CloneLoc>,
}

/// One occurrence of a clone class. There is a site for every candidate
/// subtree in the codebase — millions on a large tree — so the path is
/// shared rather than cloned: a String here costs a heap allocation per
/// site, an Arc costs a refcount bump.
#[derive(Clone)]
struct CloneLoc {
    path: std::sync::Arc<str>,
    line: u32,
    end_line: u32,
}

pub struct Agg {
    /// Imports the declared layer contract forbids, and how many
    /// layers were declared. Filled after the scan, since a contract
    /// is a question about the whole graph rather than about a file.
    pub breaches: Vec<crate::layers::Breach>,
    pub declared_layers: usize,
    pub files: u32,
    pub skipped: u32,
    pub error_files: u32,
    /// Machine-written files recognized and skipped entirely.
    pub generated: u32,
    /// Files named utils/helpers/misc/common — drawers where concepts go
    /// to lose their names (Zen: namespaces are one honking great idea).
    pub junk_files: u32,
    pub lines: u64,
    pub units: u64,
    /// Units recognized as tests — the base of Beck's ladder, reported as
    /// a share so the test/code split is always visible.
    pub test_units: u64,
    budgets: crate::config::Layers,
    files_by_lang: [u32; LANGS.len()],
    /// Violation counts, recorded at add time — with per-language budgets
    /// they cannot be recomputed from the mixed distributions.
    violations_n: [u64; N],
    total_mass: u64,
    /// Offender retention cap: bounded for display, unbounded for machine
    /// output where baselines need every violation.
    cap: usize,
    /// Files whose parse quality disqualifies their metrics.
    low_confidence: Vec<String>,
    /// Parameter-name groups by joined key: the same names traveling
    /// together through many signatures are a type the language was never
    /// told about (Fowler's Data Clumps).
    clumps: HashMap<String, Clump>,
    /// Case-label sets by key: the same dispatch repeated across the
    /// codebase (every new variant forces N edits).
    switches: HashMap<String, Clump>,
    /// Anonymous record shapes by key set: the same fields built in many
    /// places is a type nobody declared.
    shapes: HashMap<String, Clump>,
    /// Per-language spelling counts of unit names, for idiom entropy.
    spellings: [[u32; metrics::CASES]; LANGS.len()],
    /// Per-unit fingerprints, for near-clones the Merkle hash cannot see.
    prints: Vec<crate::near::Print>,
    clones: HashMap<u64, CloneClass>,
    /// Per-file module-graph facts, resolved at render time (two-phase:
    /// resolution needs the whole file set).
    pub graph: Vec<crate::graph::GraphFacts>,
    /// Per-language narrative ordering sums:
    /// [down refs, up refs, public-first pairs, public/private pairs].
    narrative: [[u64; 4]; LANGS.len()],
    /// Object -> the synonymous verbs the codebase reaches it by. Two
    /// names for one operation means a reader must learn both and a
    /// searcher will find half the call sites.
    synonyms: HashMap<Box<str>, std::collections::BTreeSet<&'static str>>,
    /// Names referenced anywhere in test code (global association set).
    test_refs: std::collections::HashSet<Box<str>>,
    /// How many FILES mention each identifier. An export mentioned by one
    /// file is mentioned only where it is defined — nobody consumes it.
    mentions: HashMap<Box<str>, u32>,
    /// Complexity-over-budget production units awaiting the test join —
    /// McCabe's actual meaning: cyclomatic is a minimum test count.
    untested_candidates: Vec<UntestedCandidate>,
    /// (covered, total) per language per rate metric — the coverage
    /// claims demoted from per-unit suspicions to rates.
    rates: [[(u64, u64); metrics::RATE_METRICS.len()]; LANGS.len()],
    dists: Vec<Vec<f32>>,
    offenders: Vec<Vec<Offender>>,
}

struct UntestedCandidate {
    name: Box<str>,
    label: String,
    cyclomatic: u32,
}

impl Agg {
    #[cfg(test)]
    pub fn new() -> Agg {
        Agg::configured(crate::config::Layers::flat(LangBudgets::defaults()), false)
    }

    /// Retains every violation — required for --json and baselines.
    #[cfg(test)]
    pub fn complete() -> Agg {
        Agg::configured(crate::config::Layers::flat(LangBudgets::defaults()), true)
    }

    /// `complete` retains every violation (machine output, baselines);
    /// display mode caps them.
    pub fn configured(budgets: crate::config::Layers, complete: bool) -> Agg {
        Agg {
            files: 0,
            skipped: 0,
            error_files: 0,
            generated: 0,
            junk_files: 0,
            lines: 0,
            units: 0,
            test_units: 0,
            budgets,
            files_by_lang: [0; LANGS.len()],
            violations_n: [0; N],
            total_mass: 0,
            cap: if complete { usize::MAX } else { KEEP },
            low_confidence: Vec::new(),
            clumps: HashMap::new(),
            switches: HashMap::new(),
            shapes: HashMap::new(),
            spellings: [[0; metrics::CASES]; LANGS.len()],
            prints: Vec::new(),
            clones: HashMap::new(),
            graph: Vec::new(),
            narrative: [[0; 4]; LANGS.len()],
            synonyms: HashMap::new(),
            test_refs: std::collections::HashSet::new(),
            breaches: Vec::new(),
            declared_layers: 0,
            mentions: HashMap::new(),
            untested_candidates: Vec::new(),
            rates: [[(0, 0); metrics::RATE_METRICS.len()]; LANGS.len()],
            dists: (0..N).map(|_| Vec::new()).collect(),
            offenders: (0..N).map(|_| Vec::new()).collect(),
        }
    }

    pub fn add_file(&mut self, facts: &FileFacts) {
        self.files += 1;
        self.files_by_lang[facts.lang as usize] += 1;
        self.lines += facts.lines as u64;
        self.error_files += (facts.parse_errors > 0) as u32;
        match facts.low_confidence() {
            true => self.count_unmeasurable(facts),
            false => self.absorb(facts),
        }
    }

    /// A file too garbled to measure still exists as an import target, so
    /// it keeps its place in the graph — only its own facts are dropped.
    fn count_unmeasurable(&mut self, facts: &FileFacts) {
        self.graph.push(crate::graph::GraphFacts {
            path: facts.path.clone(),
            lang: facts.lang,
            is_test: facts.is_test_file,
            imports: Vec::new(),
            exports: Vec::new(),
            mass: 0,
            surface_cost: 0,
        });
        self.low_confidence.push(facts.path.display().to_string());
    }

    fn absorb(&mut self, facts: &FileFacts) {
        let path = facts.path.display().to_string();
        let shared: std::sync::Arc<str> = path.as_str().into();
        self.units += (facts.units.len() - 1) as u64;
        self.test_units += facts.units.iter().filter(|u| u.is_test).count() as u64;
        self.total_mass += facts.mass as u64;
        self.junk_files += is_junk_drawer(facts) as u32;
        self.collect_recurrences(facts, &path, &shared);
        self.graph.push(graph_facts(facts));
        // Narrative ordering is a production-code property; generated-
        // style test files would drown the signal.
        if !facts.is_test_file {
            let n = &mut self.narrative[facts.lang as usize];
            n[0] += facts.step_refs.0 as u64;
            n[1] += facts.step_refs.1 as u64;
            n[2] += facts.pub_order.0 as u64;
            n[3] += facts.pub_order.1 as u64;
        }
        for u in &facts.units {
            // Tests copy each other on purpose. A module's top-level
            // scope is excluded too: it is not a thing anyone extracts,
            // and file-level duplication is what the clone classes
            // already report.
            if u.fingerprints.is_empty() || u.is_test || u.is_module {
                continue;
            }
            self.prints.push(crate::near::Print {
                label: format!("{path}:{}  {}", u.line, u.qualname),
                prints: u.fingerprints.clone(),
            });
        }
        self.collect_spellings(facts);
        self.collect_synonyms(facts);
        self.test_refs.extend(facts.test_refs.iter().cloned());
        for name in &facts.mentioned {
            *self.mentions.entry(name.clone()).or_insert(0) += 1;
        }
        self.collect_untested(facts, &path);
        let budgets = *self.budgets.for_file(&facts.path).for_lang(facts.lang);
        metrics::for_each(facts, |m, value, line, name| {
            if let Some(r) = metrics::RATE_METRICS.iter().position(|x| *x == m) {
                let cell = &mut self.rates[facts.lang as usize][r];
                cell.0 += (value > 0.0) as u64;
                cell.1 += 1;
            }
            self.dists[m].push(value);
            if budgets.violates(m, value) {
                self.violations_n[m] += 1;
                let o = Offender {
                    value,
                    path: path.clone(),
                    line,
                    name: name.to_string(),
                };
                push_offender(&mut self.offenders[m], o, self.cap);
            }
        });
    }

    /// The codebase-wide recurrences this file contributes to: clone
    /// classes, parameter clumps, and repeated dispatch.
    fn collect_recurrences(&mut self, facts: &FileFacts, path: &str, shared: &std::sync::Arc<str>) {
        for site in &facts.clone_sites {
            let class = self.clones.entry(site.hash).or_insert(CloneClass {
                mass: site.mass,
                sites: Vec::new(),
            });
            class.sites.push(CloneLoc {
                path: shared.clone(),
                line: site.line,
                end_line: site.end_line,
            });
        }
        for u in &facts.units {
            self.collect_clumps(u, path);
        }
        note_sets(&mut self.switches, &facts.switch_sigs, path);
        note_sets(&mut self.shapes, &facts.record_shapes, path);
    }

    /// Over-budget production units, held for the test-name join that
    /// restores cyclomatic to McCabe's meaning: a minimum test count.
    fn collect_untested(&mut self, facts: &FileFacts, path: &str) {
        let here = self.budgets.for_file(&facts.path);
        let cyc_budget = here.for_lang(facts.lang).0[metrics::CYCLOMATIC].1;
        for u in &facts.units {
            let (_, cyc) = metrics::complexity(u);
            if !u.is_test && !u.is_module && cyc_budget.is_some_and(|hi| cyc as f32 > hi) {
                self.untested_candidates.push(UntestedCandidate {
                    name: u.name.clone(),
                    label: format!("{path}:{}  {}", u.line, u.qualname),
                    cyclomatic: cyc,
                });
            }
        }
    }

    pub fn merge(mut a: Agg, mut b: Agg) -> Agg {
        a.files += b.files;
        a.skipped += b.skipped;
        a.error_files += b.error_files;
        a.generated += b.generated;
        a.junk_files += b.junk_files;
        a.lines += b.lines;
        a.units += b.units;
        a.test_units += b.test_units;
        a.total_mass += b.total_mass;
        a.cap = a.cap.max(b.cap);
        a.low_confidence.append(&mut b.low_confidence);
        for (i, n) in b.files_by_lang.iter().enumerate() {
            a.files_by_lang[i] += n;
        }
        for (hash, mut class) in b.clones.drain() {
            a.clones
                .entry(hash)
                .and_modify(|c| c.sites.append(&mut class.sites))
                .or_insert(class);
        }
        for (lang, row) in b.narrative.iter().enumerate() {
            for (k, v) in row.iter().enumerate() {
                a.narrative[lang][k] += v;
            }
        }
        merge_recurrences(&mut a.clumps, &mut b.clumps);
        merge_recurrences(&mut a.switches, &mut b.switches);
        merge_recurrences(&mut a.shapes, &mut b.shapes);
        for (lang, row) in b.spellings.iter().enumerate() {
            for (case, n) in row.iter().enumerate() {
                a.spellings[lang][case] += n;
            }
        }
        a.prints.append(&mut b.prints);
        a.graph.append(&mut b.graph);
        for (object, verbs) in b.synonyms.drain() {
            a.synonyms.entry(object).or_default().extend(verbs);
        }
        a.test_refs.extend(b.test_refs.drain());
        for (name, n) in b.mentions.drain() {
            *a.mentions.entry(name).or_insert(0) += n;
        }
        a.untested_candidates.append(&mut b.untested_candidates);
        for (lang, row) in b.rates.iter().enumerate() {
            for (r, (covered, total)) in row.iter().enumerate() {
                a.rates[lang][r].0 += covered;
                a.rates[lang][r].1 += total;
            }
        }
        for m in 0..N {
            a.violations_n[m] += b.violations_n[m];
            a.dists[m].append(&mut b.dists[m]);
            let cap = a.cap;
            for o in b.offenders[m].drain(..) {
                push_offender(&mut a.offenders[m], o, cap);
            }
        }
        a
    }

    /// Fowler's Data Clumps: 3- and 4-name parameter groups per unit,
    /// counted across the whole run. The same names traveling together
    /// through >=3 signatures are a struct the language was never told
    /// about.
    fn collect_clumps(&mut self, u: &crate::facts::UnitFacts, path: &str) {
        if u.params.len() < 3 || u.params.len() > 8 {
            return;
        }
        let mut names: Vec<&str> = u.params.iter().map(|p| &*p.name).collect();
        names.sort_unstable();
        let site = format!("{path}:{}  {}", u.line, u.name);
        let n = names.len();
        let mut add = |group: &[&str]| {
            let key = group.join("\u{1f}");
            let entry = self.clumps.entry(key).or_insert(Clump {
                count: 0,
                sites: Vec::new(),
            });
            entry.count += 1;
            if entry.sites.len() < CLUMP_SITES {
                entry.sites.push(site.clone());
            }
        };
        for i in 0..n {
            for j in i + 1..n {
                for k in j + 1..n {
                    add(&[names[i], names[j], names[k]]);
                    for l in k + 1..n {
                        add(&[names[i], names[j], names[k], names[l]]);
                    }
                }
            }
        }
    }

    /// How this file spells its unit names. Tests are excluded: a test
    /// name is a sentence, not an identifier following a convention.
    fn collect_spellings(&mut self, facts: &FileFacts) {
        let row = &mut self.spellings[facts.lang as usize];
        for u in &facts.units {
            if u.is_module || u.is_test {
                continue;
            }
            let slot = match crate::metrics::case_of(&u.name) {
                crate::metrics::Case::Snake => 0,
                crate::metrics::Case::Camel => 1,
                crate::metrics::Case::Pascal => 2,
                crate::metrics::Case::Screaming => 3,
                crate::metrics::Case::Neutral => continue,
            };
            row[slot] += 1;
        }
    }

    /// Which synonymous verb each named object is reached by. Only
    /// public units count: an internal helper may call its operation
    /// whatever it likes, but the surface is vocabulary others must
    /// learn.
    fn collect_synonyms(&mut self, facts: &FileFacts) {
        for u in &facts.units {
            if u.is_module || !u.is_public || u.is_test {
                continue;
            }
            if let Some((verb, object)) = crate::metrics::split_synonym(&u.name) {
                self.synonyms.entry(object.into()).or_default().insert(verb);
            }
        }
    }

    /// Sorted values of one metric — calibration's raw material.
    pub fn sorted_dist(&mut self, m: usize) -> &[f32] {
        self.dists[m].sort_unstable_by(f32::total_cmp);
        &self.dists[m]
    }

    /// Record pre-computed measurements, skipping extraction. Feeds the
    /// distribution-only scan; populates nothing else, so an aggregate
    /// built this way may only be asked for percentiles.
    pub fn add_values(&mut self, lang: crate::lang::Lang, values: &[(u8, f32)]) {
        let budgets = *self.budgets.root().for_lang(lang);
        for &(m, value) in values {
            let m = m as usize;
            self.dists[m].push(value);
            if budgets.violates(m, value) {
                self.violations_n[m] += 1;
            }
        }
    }

    /// Violations recorded for one metric, uncapped.
    pub fn violations_of(&self, m: usize) -> u64 {
        self.violations_n[m]
    }

    /// How many files of a language this run measured.
    pub fn files_of(&self, lang: crate::lang::Lang) -> u32 {
        self.files_by_lang[lang as usize]
    }

    /// Every path this scan saw — the ratchet's evidence that a
    /// baselined path is GONE rather than merely clean this run.
    pub fn scanned_paths(&self) -> std::collections::HashSet<String> {
        self.graph
            .iter()
            .map(|g| g.path.display().to_string())
            .collect()
    }

    /// Share of measurements that violated this metric's budget.
    pub fn violation_rate(&self, m: usize) -> f64 {
        match self.dists[m].len() {
            0 => 0.0,
            n => self.violations_n[m] as f64 / n as f64,
        }
    }

    /// Where a value sits in THIS repository's distribution for a metric.
    /// Budgets are ceilings; a position tells you whether you are writing
    /// like the codebase or like its ugliest tail. `None` below
    /// MIN_POSITION_SAMPLES, where a percentile would be noise.
    pub fn percentile(&mut self, m: usize, value: f32) -> Option<u8> {
        let dist = self.sorted_dist(m);
        if dist.len() < MIN_POSITION_SAMPLES {
            return None;
        }
        let below = dist.partition_point(|v| *v < value);
        Some((100 * below / dist.len()) as u8)
    }

    /// One budget across the languages present in this run, or None when
    /// they differ (mixed-language runs with per-language calibration).
    fn uniform_budget(&self, m: usize) -> Option<(Option<f32>, Option<f32>)> {
        let mut present = LANGS
            .iter()
            .filter(|l| self.files_by_lang[**l as usize] > 0);
        let first = *present.next()?;
        let band = self.budgets.root().for_lang(first).0[m];
        present
            .all(|l| self.budgets.root().for_lang(*l).0[m] == band)
            .then_some(band)
    }

    fn budget_label(&self, m: usize) -> String {
        let Some(band) = self.uniform_budget(m) else {
            return "varies".to_string();
        };
        // A trailing dot marks a budget resting on nothing but the
        // compiled default: the corpus was too thin to pin it, or it
        // is a policy no percentile may legitimize. Printing a pinned
        // budget and a guessed one identically implies evidence the
        // tool does not have.
        let label = metrics::band_label(m, band);
        match self.budget_is_pinned(m) {
            true => label,
            false => format!("{label}."),
        }
    }

    /// Does every language present in this run pin this budget to the
    /// gold corpus? One default among them makes the shown budget a
    /// default, because that is the weaker claim.
    fn budget_is_pinned(&self, m: usize) -> bool {
        LANGS
            .iter()
            .filter(|l| self.files_by_lang[**l as usize] > 0)
            .all(|l| metrics::is_pinned(*l, m))
    }

    /// What a budget rests on, for machine consumers.
    pub fn budget_source(&self, m: usize) -> &'static str {
        match self.budget_is_pinned(m) {
            true => "pinned",
            false => "default",
        }
    }
}

/// One retained violation, borrowed from the aggregate.
pub struct ViolationView<'a> {
    pub metric: usize,
    pub path: &'a str,
    pub line: u32,
    pub unit: &'a str,
    pub value: f32,
}

impl Agg {
    /// Every retained violation in metric order (complete only when the
    /// aggregate was built uncapped).
    pub fn violations(&self) -> impl Iterator<Item = ViolationView<'_>> {
        self.offenders.iter().enumerate().flat_map(|(m, os)| {
            os.iter().map(move |o| ViolationView {
                metric: m,
                path: &o.path,
                line: o.line,
                unit: &o.name,
                value: o.value,
            })
        })
    }
}

/// The retained slice of one file's facts for the dependency tier.
/// Surface cost is Ousterhout's: every export widens the interface;
/// parameters and especially flag parameters widen it further.
fn graph_facts(facts: &FileFacts) -> crate::graph::GraphFacts {
    let public_units = || {
        facts
            .units
            .iter()
            .filter(|u| u.is_public && !u.is_module && !u.is_test)
    };
    let unit_cost: u32 = public_units()
        .map(|u| 1 + u.params.len() as u32 + 2 * u.flag_params() as u32)
        .sum();
    let type_exports = facts.exports.len().saturating_sub(public_units().count()) as u32;
    crate::graph::GraphFacts {
        path: facts.path.clone(),
        lang: facts.lang,
        is_test: facts.is_test_file,
        imports: facts.imports.clone(),
        exports: facts.exports.clone(),
        mass: facts.mass,
        surface_cost: unit_cost + type_exports,
    }
}

/// Tally one file's label sets into a codebase-wide recurrence map.
fn note_sets(into: &mut HashMap<String, Clump>, sets: &[crate::facts::LabelSet], path: &str) {
    for set in sets {
        let entry = into.entry(set.key.to_string()).or_insert(Clump {
            count: 0,
            sites: Vec::new(),
        });
        entry.count += 1;
        if entry.sites.len() < CLUMP_SITES {
            entry.sites.push(format!("{path}:{}", set.line));
        }
    }
}

/// A drawer where concepts go to lose their names (Zen: namespaces are
/// one honking great idea).
fn is_junk_drawer(facts: &FileFacts) -> bool {
    let stem = facts
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    matches!(
        stem,
        "utils" | "util" | "helpers" | "helper" | "misc" | "common" | "stuff"
    )
}

/// Merge recurrence maps (clumps, switch signatures): counts add, example
/// sites cap out.
fn merge_recurrences(a: &mut HashMap<String, Clump>, b: &mut HashMap<String, Clump>) {
    for (key, clump) in b.drain() {
        let entry = a.entry(key).or_insert(Clump {
            count: 0,
            sites: Vec::new(),
        });
        entry.count += clump.count;
        for s in clump.sites {
            if entry.sites.len() < CLUMP_SITES {
                entry.sites.push(s);
            }
        }
    }
}

fn push_offender(heap: &mut Vec<Offender>, o: Offender, cap: usize) {
    heap.push(o);
    if cap < usize::MAX && heap.len() > 2 * cap {
        heap.sort_unstable_by(worse);
        heap.truncate(cap);
    }
}

pub fn render(agg: &mut Agg, top: usize) -> String {
    let mut out = String::new();
    let langs: Vec<String> = LANGS
        .iter()
        .filter(|l| agg.files_by_lang[**l as usize] > 0)
        .map(|l| format!("{} {}", l.name(), agg.files_by_lang[*l as usize]))
        .collect();
    let test_share = if agg.units > 0 {
        format!(
            " ({:.0}% test)",
            100.0 * agg.test_units as f64 / agg.units as f64
        )
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "elegance — {} files ({}), {} units{}, {} lines{}{}{}{}{}\n",
        agg.files,
        langs.join(", "),
        agg.units,
        test_share,
        agg.lines,
        note(agg.skipped, "skipped"),
        note(agg.generated, "generated skipped"),
        note(agg.junk_files, "junk-drawer files"),
        note(agg.error_files, "with parse errors"),
        note(
            agg.low_confidence.len() as u32,
            "low-confidence excluded from metrics"
        ),
    );

    render_distributions(agg, &mut out);
    render_verdict(agg, &mut out);
    render_rates(agg, &mut out);
    render_architecture(agg, &mut out);
    render_narrative(agg, &mut out);
    render_idioms(agg, &mut out);
    render_clones(agg, top, &mut out);
    render_clumps(agg, top, &mut out);
    render_switches(agg, top, &mut out);
    render_shapes(agg, top, &mut out);
    render_near(agg, top, &mut out);
    render_untested(agg, top, &mut out);
    render_synonyms(agg, top, &mut out);

    for (m, def) in METRICS.iter().enumerate() {
        if agg.offenders[m].is_empty() {
            continue;
        }
        agg.offenders[m].sort_unstable_by(worse);
        let budget = agg.budget_label(m);
        let offenders = &agg.offenders[m];
        let _ = writeln!(out, "\nworst — {} (budget {}):", def.name, budget);
        for o in offenders.iter().take(top) {
            let _ = writeln!(out, "  {:>6}  {}", fmt(def, o.value), o.label());
        }
        // The offender list is capped for display; the true count comes
        // from violations_n, which is recorded uncapped at add time.
        // Printing the capped residual understated it by up to 13x.
        let hidden = (agg.violations_n[m] as usize).saturating_sub(top);
        if hidden > 0 {
            let _ = writeln!(out, "  ... and {hidden} more");
        }
    }
    out
}

/// One row per (rate metric, language present): the repo's coverage
/// beside gold's. Used by text render and JSON alike.
pub(super) fn rate_rows(agg: &Agg) -> Vec<RateRow> {
    let mut rows = Vec::new();
    for (r, &m) in metrics::RATE_METRICS.iter().enumerate() {
        for lang in LANGS {
            let (covered, total) = agg.rates[lang as usize][r];
            if total == 0 {
                continue;
            }
            rows.push(RateRow {
                metric: METRICS[m].name,
                lang: lang.name(),
                covered,
                total,
                gold: metrics::gold_rate(lang, m),
            });
        }
    }
    rows
}

pub(super) struct RateRow {
    pub metric: &'static str,
    pub lang: &'static str,
    pub covered: u64,
    pub total: u64,
    pub gold: Option<f32>,
}

/// The coverage claims demoted from per-unit suspicions: admired code
/// fails "every public unit documented" 80.6% of the time and "every
/// complex unit asserts" 89.6%, so the honest verdict is a rate beside
/// gold's rate — not thousands of findings a reader learns to scroll
/// past.
fn render_rates(agg: &Agg, out: &mut String) {
    let rows = rate_rows(agg);
    if rows.is_empty() {
        return;
    }
    let subject = |metric: &str| match metric {
        "public docs" => "public units documented",
        _ => "complex units asserting",
    };
    let _ = writeln!(out, "\ncoverage — rates, not per-unit findings:");
    for row in rows {
        let gold = row
            .gold
            .map(|g| format!("   gold {}: {:.0}%", row.lang, g * 100.0))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {:<12} {:<4} {:>3.0}% of {} {}{}",
            row.metric,
            row.lang,
            100.0 * row.covered as f64 / row.total as f64,
            row.total,
            subject(row.metric),
            gold,
        );
    }
}

/// Rung-5 view: describes the dependency structure, gates nothing.
fn render_architecture(agg: &mut Agg, out: &mut String) {
    crate::layers::render(&agg.breaches, agg.declared_layers, out);
    if agg.graph.iter().all(|g| g.imports.is_empty()) {
        return;
    }
    // Path order makes ambiguous resolutions and listings deterministic.
    agg.graph.sort_by(|a, b| a.path.cmp(&b.path));
    let exports: usize = agg.graph.iter().map(|g| g.exports.len()).sum();
    let Some(arch) = crate::graph::analyze(&agg.graph, &agg.mentions) else {
        let r = crate::graph::resolve(&agg.graph);
        let _ = writeln!(
            out,
            "\nimports — {} internal, {} external, {} unresolved; {exports} exported symbols",
            r.internal, r.external, r.unresolved,
        );
        return;
    };
    let r = &arch.resolution;
    let _ = writeln!(
        out,
        "\narchitecture — {} modules, {} edges; imports {} internal / {} external / {} unresolved ({:.0}% resolved); {exports} exported symbols",
        arch.modules,
        arch.edges,
        r.internal,
        r.external,
        r.unresolved,
        100.0 * r.rate(),
    );
    let _ = writeln!(
        out,
        "  cycle mass {:.0}% files / {:.0}% dirs   depth p50 {}  p90 {}  max {}   deletable {:.0}%",
        arch.cycle_mass_pct,
        arch.dir_cycle_mass_pct,
        arch.depth_p50,
        arch.depth_p90,
        arch.depth_max,
        arch.deletable_pct,
    );
    if arch.largest_dir_cycle_size >= 2 {
        let _ = writeln!(
            out,
            "  largest dir cycle ({}): {}",
            arch.largest_dir_cycle_size,
            arch.largest_dir_cycle.join(", "),
        );
    }
    if arch.largest_cycle_size >= 2 {
        let _ = writeln!(
            out,
            "  largest file cycle ({}): {}",
            arch.largest_cycle_size,
            arch.largest_cycle.join(", "),
        );
    }
    if !arch.load_bearing.is_empty() {
        let worst: Vec<String> = arch
            .load_bearing
            .iter()
            .map(|(p, b)| format!("{p} <-{b}"))
            .collect();
        let _ = writeln!(out, "  load-bearing: {}", worst.join("  "));
    }
    if arch.orphan_count > 0 {
        let more = arch.orphan_count as usize - arch.orphans.len();
        let _ = writeln!(
            out,
            "  orphans ({}): {}{}",
            arch.orphan_count,
            arch.orphans.join(", "),
            if more > 0 {
                format!(" ... and {more} more")
            } else {
                String::new()
            },
        );
    }
    render_interfaces(&arch.interfaces, out);
}

/// The tail-summarized distribution of every metric this run measured.
/// Tails, never means: a codebase is as bad as the code you are forced
/// to read most often.
fn render_distributions(agg: &mut Agg, out: &mut String) {
    let _ = writeln!(
        out,
        "{:<15} {:>2} {:>7} {:>7} {:>7} {:>7}   {:<9} {:>8}",
        "metric", "r", "p50", "p90", "p99", "max", "budget", "violate"
    );
    for (m, def) in METRICS.iter().enumerate() {
        if agg.dists[m].is_empty() {
            continue;
        }
        let budget = agg.budget_label(m);
        let violate = 100.0 * agg.violations_n[m] as f64 / agg.dists[m].len() as f64;
        let dist = agg.sorted_dist(m);
        let _ = writeln!(
            out,
            "{:<15} {:>2} {:>7} {:>7} {:>7} {:>7}   {:<9} {:>7.1}%",
            def.name,
            def.rung,
            fmt(def, quantile(dist, P50)),
            fmt(def, quantile(dist, P90)),
            fmt(def, quantile(dist, P99)),
            fmt(def, *dist.last().expect("non-empty")),
            budget,
            violate,
        );
    }
    let _ = writeln!(
        out,
        "{:<15} r = ladder rung: 0-2 violation (gate), 3-4 suspicion, 5-6 report, 7 tension",
        ""
    );
    let _ = writeln!(
        out,
        "{:<15} a budget ending in `.` rests on the compiled default, not on the gold corpus",
        ""
    );
}

/// Share of the codebase's mass that is duplicated logic.
pub fn duplicated_pct(agg: &mut Agg) -> f64 {
    select_clones(agg).1.all_pct
}

/// The counts a verdict is made of, per ladder class.
pub struct Verdict {
    pub gates: u64,
    pub suspicions: u64,
    /// Metric names carrying the most weight in each class, worst first.
    pub gate_drivers: Vec<(&'static str, u64)>,
    pub suspicion_drivers: Vec<(&'static str, u64)>,
}

const DRIVERS_SHOWN: usize = 3;

/// What the numbers add up to, per verdict class. Deliberately NOT a
/// score: a single figure would trade away the one thing this tool has,
/// which is knowing what kind of claim each rung can support.
pub fn verdict(agg: &Agg) -> Verdict {
    let mut v = Verdict {
        gates: 0,
        suspicions: 0,
        gate_drivers: Vec::new(),
        suspicion_drivers: Vec::new(),
    };
    for (m, def) in METRICS.iter().enumerate() {
        let n = agg.violations_n[m];
        if n == 0 {
            continue;
        }
        let (total, drivers) = if def.rung <= 2 {
            (&mut v.gates, &mut v.gate_drivers)
        } else if def.rung <= 4 {
            (&mut v.suspicions, &mut v.suspicion_drivers)
        } else {
            continue;
        };
        *total += n;
        drivers.push((def.name, n));
    }
    for drivers in [&mut v.gate_drivers, &mut v.suspicion_drivers] {
        drivers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        drivers.truncate(DRIVERS_SHOWN);
    }
    v
}

fn render_verdict(agg: &Agg, out: &mut String) {
    let v = verdict(agg);
    let named = |drivers: &[(&str, u64)]| {
        drivers
            .iter()
            .map(|(name, n)| format!("{name} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(out);
    if v.gates == 0 {
        let _ = writeln!(out, "gates (rungs 0-2)      clean");
    } else {
        let _ = writeln!(
            out,
            "gates (rungs 0-2)      {} violations — {}",
            v.gates,
            named(&v.gate_drivers)
        );
    }
    if v.suspicions > 0 {
        let _ = writeln!(
            out,
            "suspicions (rungs 3-4) {} — {}",
            v.suspicions,
            named(&v.suspicion_drivers)
        );
    }
}

/// One row per language with narrative data: direction of intra-file
/// calls and public-first ordering. Report-only (rung 5): ecosystems
/// legitimately differ — C's define-before-use heritage reads bottom-up
/// by design, and the split is itself the finding.
pub(super) fn narrative_rows(agg: &Agg) -> Vec<(&'static str, f64, u64, f64, u64)> {
    let mut rows = Vec::new();
    for lang in LANGS {
        let [down, up, pub_first, pairs] = agg.narrative[lang as usize];
        let refs = down + up;
        if refs == 0 && pairs == 0 {
            continue;
        }
        let down_pct = 100.0 * down as f64 / refs.max(1) as f64;
        let pub_pct = 100.0 * pub_first as f64 / pairs.max(1) as f64;
        rows.push((lang.name(), down_pct, refs, pub_pct, pairs));
    }
    rows
}

/// One object and every synonymous verb the codebase reaches it by.
pub(super) type Drift = (String, Vec<&'static str>);

/// Objects the codebase reaches by more than one synonymous verb, with
/// the verbs. Report-only: a team may have a good reason to distinguish
/// `get` from `fetch`, and this cannot know whether they did.
pub(super) fn select_synonyms(agg: &Agg) -> Vec<Drift> {
    let mut drifted: Vec<Drift> = Vec::new();
    for (object, verbs) in &agg.synonyms {
        if verbs.len() < 2 {
            continue;
        }
        let spellings: Vec<&'static str> = verbs.iter().copied().collect();
        drifted.push((object.to_string(), spellings));
    }
    drifted.sort_by(widest_drift_first);
    drifted
}

/// An object spelled three ways is worse than one spelled two.
fn widest_drift_first(a: &Drift, b: &Drift) -> std::cmp::Ordering {
    b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0))
}

fn render_synonyms(agg: &Agg, top: usize, out: &mut String) {
    let drifted = select_synonyms(agg);
    if drifted.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\nsynonym drift — {} objects reached by more than one verb:",
        drifted.len()
    );
    for (object, verbs) in drifted.iter().take(top) {
        let listed: Vec<String> = verbs.iter().map(|v| format!("{v}_{object}")).collect();
        let _ = writeln!(out, "  {}", listed.join("  "));
    }
    if drifted.len() > top {
        let _ = writeln!(out, "  ... and {} more", drifted.len() - top);
    }
}

/// Per language: the dominant spelling, its share, and the entropy of
/// the distribution. Report-only — a repository may deliberately mix
/// (Rust types are Pascal and its functions are snake), which is why the
/// number is shown next to the names rather than judged.
pub(super) fn idiom_rows(agg: &Agg) -> Vec<(&'static str, &'static str, f64, f64)> {
    const NAMES: [&str; metrics::CASES] = ["snake_case", "camelCase", "PascalCase", "SCREAMING"];
    let mut rows = Vec::new();
    for lang in LANGS {
        let counts = &agg.spellings[lang as usize];
        let total: u32 = counts.iter().sum();
        if total == 0 {
            continue;
        }
        let (best, n) = counts
            .iter()
            .enumerate()
            .max_by_key(|(i, n)| (**n, std::cmp::Reverse(*i)))
            .expect("CASES is non-empty");
        rows.push((
            lang.name(),
            NAMES[best],
            100.0 * *n as f64 / total as f64,
            metrics::idiom_entropy(counts),
        ));
    }
    rows
}

fn render_idioms(agg: &Agg, out: &mut String) {
    let rows = idiom_rows(agg);
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\nidiom — how one way of spelling a name this codebase has:\n\
         {:<6} {:<12} {:>9} {:>8}",
        "lang", "dominant", "share", "entropy"
    );
    for (lang, dominant, share, entropy) in rows {
        let _ = writeln!(out, "{lang:<6} {dominant:<12} {share:>8.0}% {entropy:>8.2}");
    }
    let _ = writeln!(
        out,
        "entropy 0 is one spelling throughout, 1 is every spelling equally likely.\n\
         A mix is often MANDATED rather than sloppy: Go spells exported names with a\n\
         capital by language rule, and Zig's type-returning functions are Pascal by\n\
         convention, so both read high without anyone having been inconsistent."
    );
}

fn render_narrative(agg: &Agg, out: &mut String) {
    let rows = narrative_rows(agg);
    if rows.is_empty() {
        return;
    }
    let parts: Vec<String> = rows
        .iter()
        .map(|(lang, down, refs, pub_pct, pairs)| {
            let mut s = format!("{lang} {down:.0}% down/{refs}");
            if *pairs > 0 {
                let _ = write!(s, ", {pub_pct:.0}% public-first");
            }
            s
        })
        .collect();
    let _ = writeln!(
        out,
        "\nnarrative — step-down reading order (share of intra-file calls pointing down):\n  {}",
        parts.join("   ")
    );
}

/// Parnas made visible: hide a lot behind a little. Shallow listings
/// carry mass so god-modules with narrow facades stay suspicious.
fn render_interfaces(i: &crate::graph::metrics::Interfaces, out: &mut String) {
    let _ = writeln!(
        out,
        "  interfaces — median depth {}x (mass per surface unit)",
        i.median_depth
    );
    if let Some((label, depth, mass, cost)) = i.shallow.first() {
        let rest: Vec<String> = i.shallow[1..]
            .iter()
            .map(|(l, d, _, _)| format!("{l} {d}x"))
            .collect();
        let _ = writeln!(
            out,
            "  shallow-wide: {label} depth {depth}x (mass {mass} / surface {cost})  {}",
            rest.join("  "),
        );
    }
    if let Some((label, used, exports)) = i.fat.first() {
        let rest: Vec<String> = i.fat[1..]
            .iter()
            .map(|(l, u, e)| format!("{l} {u}/{e}"))
            .collect();
        let _ = writeln!(
            out,
            "  fat surface: {label} {used}/{exports} exports imported by name  {}",
            rest.join("  "),
        );
    }
    if i.leak_count > 0 {
        let _ = writeln!(out, "  leaks ({}): {}", i.leak_count, i.leaks.join("  "),);
    }
    if i.dead_count > 0 {
        let more = i.dead_count as usize - i.dead.len();
        let _ = writeln!(
            out,
            "  dead exports ({}, no other file names them{}): {}",
            i.dead_count,
            if more > 0 {
                format!(", {more} more")
            } else {
                String::new()
            },
            i.dead.join("  "),
        );
    }
}

pub(super) struct SelectedClump {
    pub names: Vec<String>,
    pub count: u32,
    pub sites: Vec<String>,
}

/// Recurring parameter groups, largest-and-strongest first. A smaller
/// group is suppressed only when a kept superset fully explains it
/// (equal counts) — an independently stronger 3-group still surfaces.
pub(super) fn select_clumps(agg: &Agg) -> Vec<SelectedClump> {
    let mut all: Vec<(Vec<&str>, &Clump)> = agg
        .clumps
        .iter()
        .filter(|(_, c)| c.count >= CLUMP_MIN)
        .map(|(key, c)| (key.split('\u{1f}').collect(), c))
        .collect();
    // Widest clump first, then most frequent, then by name.
    all.sort_by(|(a_names, a), (b_names, b)| {
        b_names
            .len()
            .cmp(&a_names.len())
            .then(b.count.cmp(&a.count))
            .then(a_names.cmp(b_names))
    });
    let mut kept: Vec<SelectedClump> = Vec::new();
    for (names, c) in all {
        let explained = kept
            .iter()
            .any(|k| k.count == c.count && names.iter().all(|n| k.names.iter().any(|kn| kn == n)));
        if explained {
            continue;
        }
        let mut sites = c.sites.clone();
        sites.sort_unstable();
        kept.push(SelectedClump {
            names: names.into_iter().map(str::to_string).collect(),
            count: c.count,
            sites,
        });
    }
    kept.sort_by(|a, b| b.count.cmp(&a.count).then(a.names.cmp(&b.names)));
    kept
}

/// Anonymous record shapes built in >=3 places: a type nobody declared.
pub(super) fn select_shapes(agg: &Agg) -> Vec<SelectedClump> {
    recurring_sets(&agg.shapes)
}

/// Repeated dispatch: label-sets recurring >=3 times, strongest first.
pub(super) fn select_switches(agg: &Agg) -> Vec<SelectedClump> {
    recurring_sets(&agg.switches)
}

fn recurring_sets(sets: &HashMap<String, Clump>) -> Vec<SelectedClump> {
    let mut kept: Vec<SelectedClump> = sets
        .iter()
        .filter(|(_, c)| c.count >= CLUMP_MIN)
        .map(|(key, c)| {
            let mut sites = c.sites.clone();
            sites.sort_unstable();
            SelectedClump {
                names: key.split('\u{1f}').map(str::to_string).collect(),
                count: c.count,
                sites,
            }
        })
        .collect();
    kept.sort_by(|a, b| b.count.cmp(&a.count).then(a.names.cmp(&b.names)));
    kept
}

/// The same fields built again and again with nothing checking them.
/// Units that share most of their shape without being identical — the
/// copy-paste that has since been edited, which the Merkle detector
/// stops seeing the moment a line is inserted.
fn render_near(agg: &Agg, top: usize, out: &mut String) {
    let found = crate::near::pairs(&agg.prints, top);
    if found.pairs.is_empty() && found.suppressed_cores == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "\nnear-clones — {} pairs sharing most of their shape (edited copies):",
        found.pairs.len()
    );
    for pair in &found.pairs {
        let _ = writeln!(
            out,
            "  {:.0}%  {}\n        {}",
            pair.overlap * 100.0,
            pair.a,
            pair.b
        );
    }
    if found.suppressed_cores > 0 {
        let _ = writeln!(
            out,
            "  {} shared cores sit in more units than the idiom cap (widest: {} units)\n  and were not paired — an idiom, or duplication too wide to pair cheaply.",
            found.suppressed_cores, found.widest_core,
        );
    }
}

fn render_shapes(agg: &Agg, top: usize, out: &mut String) {
    let kept = select_shapes(agg);
    if kept.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\nundeclared shapes — {} field sets built in >=3 places:",
        kept.len()
    );
    for s in kept.iter().take(top) {
        let _ = writeln!(out, "  {{{}}} ×{}:", s.names.join(", "), s.count);
        for site in s.sites.iter().take(3) {
            let _ = writeln!(out, "      {site}");
        }
        let _ = writeln!(
            out,
            "      -> Declare the type: nothing catches a typo in these keys, and\n\
             \x20        adding a field means finding every site by hand"
        );
    }
    if kept.len() > top {
        let _ = writeln!(out, "  ... and {} more shapes", kept.len() - top);
    }
}

fn render_switches(agg: &Agg, top: usize, out: &mut String) {
    let kept = select_switches(agg);
    if kept.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\nrepeated dispatch — {} case-label sets switched on in >=3 places:",
        kept.len()
    );
    for s in kept.iter().take(top) {
        let labels = s.names.join(" | ");
        let mut end = labels.len().min(70);
        while !labels.is_char_boundary(end) {
            end -= 1;
        }
        let _ = writeln!(out, "  [{}] ×{}:", &labels[..end], s.count);
        for site in s.sites.iter().take(3) {
            let _ = writeln!(out, "      {site}");
        }
        let _ = writeln!(out, "      -> {}", suggest::for_dispatch(&s.names, s.count));
    }
    if kept.len() > top {
        let _ = writeln!(out, "  ... and {} more sets", kept.len() - top);
    }
}

/// Complexity-over-budget units no test ever mentions, worst first —
/// cyclomatic restored to McCabe's meaning (a minimum test count).
/// Name association, not coverage: an approximation, labeled as such.
pub(super) fn select_untested(agg: &Agg) -> Vec<(String, u32)> {
    let mut hits: Vec<(String, u32)> = agg
        .untested_candidates
        .iter()
        .filter(|c| !agg.test_refs.contains(&c.name))
        .map(|c| (c.label.clone(), c.cyclomatic))
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    hits
}

fn render_untested(agg: &Agg, top: usize, out: &mut String) {
    let hits = select_untested(agg);
    if hits.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\nuntested complexity — {} over-budget units no test mentions (name association):",
        hits.len()
    );
    for (label, cyc) in hits.iter().take(top) {
        let _ = writeln!(out, "  cyclomatic {cyc:>3}  {label}");
    }
    if hits.len() > top {
        let _ = writeln!(out, "  ... and {} more", hits.len() - top);
    }
}

fn render_clumps(agg: &Agg, top: usize, out: &mut String) {
    let kept = select_clumps(agg);
    if kept.is_empty() {
        return;
    }
    let _ = writeln!(out, "\nparam clumps — {} recurring groups:", kept.len());
    for clump in kept.iter().take(top) {
        let _ = writeln!(out, "  ({}) ×{}:", clump.names.join(", "), clump.count);
        for s in clump.sites.iter().take(3) {
            let _ = writeln!(out, "      {s}");
        }
        let _ = writeln!(
            out,
            "      -> {}",
            suggest::for_clump(&clump.names, clump.count)
        );
    }
    if kept.len() > top {
        let _ = writeln!(out, "  ... and {} more groups", kept.len() - top);
    }
}

struct SelectedClone {
    mass: u32,
    sites: Vec<CloneLoc>,
}

/// Sites of one recurring class, materialized and ordered. Only classes
/// that actually recur pay for this.
fn recurring(agg: &Agg) -> Vec<SelectedClone> {
    agg.clones
        .values()
        .filter(|c| c.sites.len() >= 2)
        .map(|c| {
            let mut sites: Vec<CloneLoc> = c.sites.clone();
            // Total order: two candidates can start on the same line of
            // the same file (a subtree and its parent), and an unstable
            // sort would let rayon's merge order pick between them.
            sites.sort_unstable_by(|a, b| {
                a.path
                    .cmp(&b.path)
                    .then_with(|| a.line.cmp(&b.line))
                    .then_with(|| a.end_line.cmp(&b.end_line))
            });
            SelectedClone {
                mass: c.mass,
                sites,
            }
        })
        .collect()
}

/// Duplicated mass, split by where it lives. Test duplication is a
/// different finding from production duplication: a copied fixture costs
/// a reader nothing, while copied production logic must change in every
/// place at once. Reporting one number for both flattered or damned a
/// codebase depending only on how much of it was tests.
#[derive(Default, Clone, Copy)]
pub struct Duplication {
    pub all_pct: f64,
    pub production_pct: f64,
    /// Classes with sites on both sides — production logic that has been
    /// copied into a test, so the test no longer exercises the original.
    pub straddling: u32,
}

/// Maximal clone classes, worst first: recurring hashes only, nested copies
/// of an already-selected clone suppressed — each class is one distinct
/// refactoring opportunity.
fn select_clones(agg: &mut Agg) -> (Vec<SelectedClone>, Duplication) {
    // Sites arrive in rayon merge order; ordering them makes class
    // representatives and listings stable across runs.
    let mut classes = recurring(agg);
    classes.sort_unstable_by(|a, b| {
        let saved = |c: &SelectedClone| (c.sites.len() - 1) as u64 * c.mass as u64;
        saved(b)
            .cmp(&saved(a))
            .then_with(|| a.sites[0].path.cmp(&b.sites[0].path))
            .then_with(|| a.sites[0].line.cmp(&b.sites[0].line))
            .then_with(|| a.sites[0].end_line.cmp(&b.sites[0].end_line))
            .then_with(|| a.mass.cmp(&b.mass))
    });

    // Keyed by the shared path so a kept class can be moved out; an
    // Arc clone is a refcount bump.
    // Which paths are test files is already known from the graph, so the
    // split costs a lookup rather than a byte on every clone site.
    let tests: std::collections::HashSet<String> = agg
        .graph
        .iter()
        .filter(|g| g.is_test)
        .map(|g| g.path.display().to_string())
        .collect();
    let mut covered: HashMap<std::sync::Arc<str>, Vec<(u32, u32)>> = HashMap::new();
    let mut kept: Vec<SelectedClone> = Vec::new();
    let mut dup = Duplication::default();
    let mut duplicated: u64 = 0;
    let mut production: u64 = 0;
    for class in classes {
        let fresh = class
            .sites
            .iter()
            .filter(|s| {
                covered
                    .get(&s.path)
                    .is_none_or(|iv| !iv.iter().any(|&(a, b)| a <= s.line && s.end_line <= b))
            })
            .count();
        if fresh < 2 {
            continue;
        }
        let saved = (fresh - 1) as u64 * class.mass as u64;
        duplicated += saved;
        let in_test = class
            .sites
            .iter()
            .filter(|s| tests.contains(&*s.path))
            .count();
        if in_test == 0 {
            production += saved;
        } else if in_test < class.sites.len() {
            dup.straddling += 1;
        }
        for s in &class.sites {
            covered
                .entry(s.path.clone())
                .or_default()
                .push((s.line, s.end_line));
        }
        kept.push(class);
    }
    let mass = agg.total_mass.max(1) as f64;
    dup.all_pct = 100.0 * duplicated as f64 / mass;
    dup.production_pct = 100.0 * production as f64 / mass;
    (kept, dup)
}

fn render_clones(agg: &mut Agg, top: usize, out: &mut String) {
    let (kept, dup) = select_clones(agg);
    if kept.is_empty() {
        return;
    }
    let straddle = match dup.straddling {
        0 => String::new(),
        n => format!(", {n} straddling test and production"),
    };
    let _ = writeln!(
        out,
        "\nclones — {} classes, ~{:.1}% of code duplicated ({:.1}% outside tests{straddle}):",
        kept.len(),
        dup.all_pct,
        dup.production_pct,
    );
    for class in kept.iter().take(top) {
        let _ = writeln!(out, "  {} sites × mass {}:", class.sites.len(), class.mass);
        for s in class.sites.iter().take(4) {
            let _ = writeln!(out, "      {}:{}-{}", s.path, s.line, s.end_line);
        }
        if class.sites.len() > 4 {
            let _ = writeln!(out, "      ... and {} more", class.sites.len() - 4);
        }
        let _ = writeln!(
            out,
            "      -> {}",
            suggest::for_clone(class.sites.len(), class.mass)
        );
    }
    if kept.len() > top {
        let _ = writeln!(out, "  ... and {} more classes", kept.len() - top);
    }
}

/// Per-unit breakdown: every construct's contribution, so a score is an
/// explanation, not a verdict.
pub fn render_explain(facts: &FileFacts, line: Option<u32>) -> String {
    let mut out = String::new();
    for u in &facts.units {
        let targeted = match line {
            Some(l) => u.line <= l && l < u.line + u.lines,
            None => !u.is_module,
        };
        if targeted {
            explain_unit(u, facts, &mut out);
        }
    }
    let file_lines = |label: &str, lines: &[u32], out: &mut String| {
        if !lines.is_empty() {
            let shown: Vec<String> = lines.iter().take(8).map(u32::to_string).collect();
            let _ = writeln!(out, "{label} at lines: {}", shown.join(", "));
        }
    };
    file_lines("spooky constructs", &facts.spooky_lines, &mut out);
    file_lines("echo comments", &facts.echo_comments, &mut out);
    if out.is_empty() {
        out.push_str("no unit at that location\n");
    }
    out
}

fn explain_unit(u: &crate::facts::UnitFacts, facts: &FileFacts, out: &mut String) {
    let (cog, cyc) = metrics::complexity(u);
    let _ = writeln!(
        out,
        "{}  {}:{}  cognitive {}  cyclomatic {}  depth {}  expr {}  {} lines  {} params",
        u.qualname,
        facts.path.display(),
        u.line,
        cog,
        cyc,
        u.max_vis_depth,
        u.max_expr_depth,
        u.lines,
        u.params.len(),
    );
    let flags: Vec<&str> = u
        .params
        .iter()
        .filter(|p| p.boolish)
        .map(|p| &*p.name)
        .collect();
    if !flags.is_empty() {
        let _ = writeln!(out, "  {:<17} {}", "flag params", flags.join(", "));
    }
    if u.max_live_span > 0 {
        let _ = writeln!(
            out,
            "  {:<17} {} lines ({})",
            "live span", u.max_live_span, u.max_live_var
        );
    }
    for (label, n) in [
        ("demeter chains", u.demeter),
        ("negations", u.negations),
        ("magic numbers", u.magic_numbers),
    ] {
        if n > 0 {
            let _ = writeln!(out, "  {:<17} {n}", label);
        }
    }
    for remedy in remedies_for(u, facts) {
        let _ = writeln!(out, "  -> {remedy}");
    }
    for c in &u.ctrl {
        let (dc, dy) = metrics::event_score(c);
        if dc == 0 && dy == 0 {
            continue;
        }
        let label = format!("{:?}", c.sem).to_lowercase();
        let nesting = if c.sem.cognitive_nested() && c.cog_depth > 0 {
            format!("  (1 + nesting {})", c.cog_depth)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "  L{:<5} {:<10} cognitive +{dc}  cyclomatic +{dy}{nesting}",
            c.line, label
        );
    }
    if u.self_recursive {
        let _ = writeln!(out, "  {:<17} cognitive +1", "recursion");
    }
    let _ = writeln!(out);
}

/// Named refactorings for what this unit actually VIOLATES — a
/// suggestion on a metric that is within budget is noise dressed as
/// advice.
fn remedies_for(u: &crate::facts::UnitFacts, facts: &FileFacts) -> Vec<String> {
    let budgets = *LangBudgets::calibrated().for_lang(facts.lang);
    let mut remedies = Vec::new();
    metrics::for_each(facts, |m, value, line, name| {
        if line == u.line && name == &*u.qualname && budgets.violates(m, value) {
            remedies.extend(suggest::for_metric(m, u));
        }
    });
    remedies.sort_unstable();
    remedies.dedup();
    remedies
}

fn note(n: u32, what: &str) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(", {n} {what}")
    }
}

/// A sorted distribution's quantile, formatted as the metric would show
/// it. (The raw `quantile` is internal to rendering.)
pub fn quantile_of(sorted: &[f32], q: f64) -> String {
    format!("{:.0}", quantile(sorted, q))
}

/// The value at quantile `q` of a sorted distribution — shared with
/// calibration, which pins budgets to the same arithmetic the report
/// prints.
pub(crate) fn quantile(sorted: &[f32], q: f64) -> f32 {
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx]
}

fn fmt(def: &metrics::MetricDef, v: f32) -> String {
    match def.fmt {
        Fmt::Int => format!("{v:.0}"),
        Fmt::Pct => format!("{:.0}%", v * 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FileFacts, extract};
    use crate::lang::Lang;
    use std::path::Path;

    fn facts(path: &str, source: &str) -> FileFacts {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), source)
    }

    /// Deeply nested violator, also big enough to be a clone site; twin
    /// copies in different files produce equal metric values (tie on value)
    /// and one clone class — both tie-break paths get exercised.
    const TWIN: &str = "def NAME(xs):\n    t = 0\n    for x in xs:\n        if x > 0:\n            if x > 1:\n                if x > 2:\n                    if x > 3:\n                        if x > 4:\n                            t += x\n    return t\n";

    #[test]
    fn render_is_independent_of_aggregation_order() {
        let a = facts("a.py", &TWIN.replace("NAME", "f0"));
        let b = facts("b.py", &TWIN.replace("NAME", "f1"));

        let mut fwd = Agg::new();
        fwd.add_file(&a);
        fwd.add_file(&b);

        let mut left = Agg::new();
        left.add_file(&b);
        let mut right = Agg::new();
        right.add_file(&a);
        let mut rev = Agg::merge(left, right);

        let fwd_out = render(&mut fwd, 10);
        assert_eq!(fwd_out, render(&mut rev, 10));
        assert!(
            fwd_out.contains("clones"),
            "twin functions must form a clone class"
        );
        // Equal values: a.py must consistently precede b.py.
        let a_pos = fwd_out.find("a.py:1").expect("a.py offender listed");
        let b_pos = fwd_out.find("b.py:1").expect("b.py offender listed");
        assert!(a_pos < b_pos);
    }

    #[test]
    fn duplicated_data_is_content_not_a_clone() {
        // Two identical thirty-entry tables collapse under Type-2
        // normalization, but duplicated DATA is content: the clone
        // classes require MIN_CLONE_LOGIC control/call mass, and a
        // literal blob has none.
        let rows = "        \"k\": 1,\n".repeat(30);
        let table = |name: &str| format!("def {name}():\n    return {{\n{rows}    }}\n");
        let mut agg = Agg::new();
        agg.add_file(&facts("a.py", &table("first")));
        agg.add_file(&facts("b.py", &table("second")));
        let (kept, dup) = select_clones(&mut agg);
        assert!(kept.is_empty(), "a data table is not a refactoring target");
        assert_eq!(dup.all_pct, 0.0);
    }

    #[test]
    fn clumps_need_three_recurrences_and_supersets_absorb_equal_subsets() {
        let sig = |name: &str| {
            format!("def {name}(host, port, timeout, retries):\n    return connect(host)\n")
        };
        let mut agg = Agg::new();
        agg.add_file(&facts(
            "a.py",
            &format!("{}{}", sig("open_a"), sig("open_b")),
        ));
        assert!(
            select_clumps(&agg).is_empty(),
            "two recurrences are coincidence"
        );

        agg.add_file(&facts("b.py", &sig("open_c")));
        let three = select_clumps(&agg);
        // Four 3-subsets all have count 3 too; the 4-group explains them.
        assert_eq!(three.len(), 1, "equal-count subsets absorbed by superset");
        assert_eq!(three[0].names, ["host", "port", "retries", "timeout"]);
        assert_eq!(three[0].count, 3);
    }

    #[test]
    fn a_shape_built_three_times_is_a_type_nobody_declared() {
        const SHAPE: &str = "    return {\"host\": \"h\", \"port\": 1, \"timeout\": 2}\n";
        let make = |name: &str| format!("def {name}():\n{SHAPE}");
        let mut agg = Agg::new();
        let twice = format!("{}{}", make("one"), make("two"));
        agg.add_file(&facts("a.py", &twice));
        assert!(
            select_shapes(&agg).is_empty(),
            "two constructions are a coincidence"
        );
        let third = make("three");
        agg.add_file(&facts("b.py", &third));
        let found = select_shapes(&agg);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].names, ["host", "port", "timeout"]);
        assert_eq!(found[0].count, 3);

        // A pair is not a shape somebody should have named.
        let mut small = Agg::new();
        for i in 0..4 {
            let path = format!("p{i}.py");
            let pair = facts(&path, "def f():\n    return {\"a\": 1, \"b\": 2}\n");
            small.add_file(&pair);
        }
        assert!(select_shapes(&small).is_empty());
    }

    #[test]
    fn repeated_dispatch_needs_the_same_label_set_three_times() {
        let m = |x: &str| {
            format!(
                "def h_{x}(kind):\n    match kind:\n        case 'a':\n            pass\n        case 'b':\n            pass\n        case _:\n            pass\n"
            )
        };
        let mut agg = Agg::new();
        agg.add_file(&facts("a.py", &format!("{}{}", m("one"), m("two"))));
        assert!(select_switches(&agg).is_empty());
        agg.add_file(&facts("b.py", &m("three")));
        let hits = select_switches(&agg);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].count, 3);
        assert_eq!(hits[0].names, ["'a'", "'b'", "_"]);
    }

    #[test]
    fn untested_complexity_joins_names_against_test_references() {
        let violator = |name: &str| {
            format!(
                "def {name}(a):\n    if a == 1:\n        return 1\n    elif a == 2:\n        return 2\n    elif a == 3:\n        return 3\n    elif a == 4:\n        return 4\n    elif a == 5:\n        return 5\n    elif a == 6:\n        return 6\n    elif a == 7:\n        return 7\n    elif a == 8:\n        return 8\n    elif a == 9:\n        return 9\n    elif a == 10:\n        return 10\n    elif a == 11:\n        return 11\n    elif a == 12:\n        return 12\n    return 0\n"
            )
        };
        let mut agg = Agg::new();
        agg.add_file(&facts(
            "prod.py",
            &format!(
                "{}{}{}",
                violator("covered"),
                violator("orphan"),
                violator("dispatch").replace("def dispatch", "class W:\n    def dispatch"),
            ),
        ));
        // Method exercised through a receiver: `w.dispatch(...)` must join.
        agg.add_file(&facts(
            "test_prod.py",
            "def test_covered_works():\n    assert covered(1) == 1\n\ndef test_dispatch_works():\n    w = W()\n    assert w.dispatch(1) == 1\n",
        ));
        let hits = select_untested(&agg);
        assert_eq!(hits.len(), 1, "only the unreferenced one remains");
        assert!(hits[0].0.contains("orphan"));
    }

    #[test]
    fn coverage_renders_as_a_rate_and_never_as_suspicions() {
        // Admired code fails "every public unit documented" 80.6% of
        // the time; the demotion means an undocumented public unit adds
        // a rate row, not a finding.
        let mut agg = Agg::new();
        agg.add_file(&facts(
            "a.py",
            "def api(x):\n    \"\"\"doc\"\"\"\n    return x\n\ndef bare(y):\n    return y\n",
        ));
        let v = verdict(&agg);
        assert!(
            v.suspicion_drivers
                .iter()
                .all(|(name, _)| *name != "public docs"),
            "public docs must not appear as a suspicion: {:?}",
            v.suspicion_drivers
        );
        let out = render(&mut agg, 5);
        assert!(
            out.contains("public docs  py    50% of 2 public units documented"),
            "one of two public units documented:\n{out}"
        );
    }

    #[test]
    fn verdict_counts_match_the_underlying_violations() {
        let mut agg = Agg::new();
        agg.add_file(&facts("a.py", &TWIN.replace("NAME", "f0")));
        let v = verdict(&agg);
        // Every driver's count must be exactly what the metric recorded,
        // and the class totals their sum — synthesis is selection, never
        // a new number.
        let gate_sum: u64 = v.gate_drivers.iter().map(|(_, n)| n).sum();
        assert!(v.gates >= gate_sum, "totals cover the shown drivers");
        for (name, n) in v.gate_drivers.iter().chain(&v.suspicion_drivers) {
            let m = METRICS.iter().position(|d| d.name == *name).unwrap();
            assert_eq!(*n, agg.violations_n[m], "{name}");
            assert!(METRICS[m].rung <= 4, "reports never enter a verdict");
        }
        let out = render(&mut agg, 3);
        assert!(out.contains("gates (rungs 0-2)"), "verdict block rendered");

        let mut clean = Agg::new();
        clean.add_file(&facts("b.py", "def f(x):\n    return x + 1\n"));
        assert!(render(&mut clean, 3).contains("gates (rungs 0-2)      clean"));
    }

    #[test]
    fn percentiles_place_a_value_in_the_repo_and_stay_silent_when_thin() {
        let mut agg = Agg::new();
        // 60 files x ~4 units: enough units to clear MIN_POSITION_SAMPLES.
        for i in 0..60 {
            agg.add_file(&facts(
                &format!("f{i}.py"),
                "def a():\n    pass\n\ndef b():\n    if 1:\n        pass\n\ndef c():\n    pass\n",
            ));
        }
        let flat = agg
            .percentile(metrics::COGNITIVE, 0.0)
            .expect("enough samples");
        let tall = agg
            .percentile(metrics::COGNITIVE, 50.0)
            .expect("enough samples");
        assert_eq!(flat, 0, "a zero sits at the bottom of the distribution");
        assert_eq!(tall, 100, "an outlier sits above everything measured");

        // A metric with few measurements must not pretend to a position.
        let mut thin = Agg::new();
        thin.add_file(&facts("only.py", "def a():\n    pass\n"));
        assert_eq!(thin.percentile(metrics::COGNITIVE, 1.0), None);
    }

    #[test]
    fn low_confidence_files_are_counted_but_not_measured() {
        let garbage = facts("broken.py", "def f(((:\n@@@ ??? !!!\n]] )) ((\n");
        assert!(garbage.low_confidence());
        let mut agg = Agg::new();
        agg.add_file(&garbage);
        assert_eq!(agg.files, 1);
        assert_eq!(agg.units, 0, "no metrics from an unparseable file");
        let out = render(&mut agg, 5);
        assert!(out.contains("1 low-confidence excluded"));
    }
}
