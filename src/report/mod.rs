//! Aggregation and rendering. Distributions are summarized by tails, never
//! means — a codebase is as bad as the code you're forced to read most often.
//! Offender lists hold only budget violations, so clean code reports quietly.

pub mod ink;
mod json;
mod sarif;
pub mod suggest;

pub use json::render_json;
pub use sarif::render as render_sarif;

use std::collections::HashMap;
use std::fmt::Write;

use crate::facts::{CommentRole, FileFacts};
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

/// Distinct budgets a mixed run names before the column stops being a
/// column. A 22-language scan can have twenty of them; three covers the
/// languages a reader is actually reading.
const BUDGET_LANGS: usize = 3;

/// The tail quantiles every distribution is summarized by. p99 is also
/// what budgets are pinned to, so the report shows the number the
/// calibration argues about.
pub(super) const P50: f64 = 0.50;
pub(super) const P90: f64 = 0.90;
pub(super) const P99: f64 = 0.99;

/// What a set of comment runs adds up to. Sums rather than a
/// distribution: this answers "what does this role look like here",
/// which is the question a per-role budget starts from.
#[derive(Clone, Copy, Default)]
pub struct Tally {
    pub runs: u64,
    pub words: u64,
    pub sentences: u64,
    pub grounds: u64,
    pub purposes: u64,
}

impl Tally {
    fn add(&mut self, other: Tally) {
        self.runs += other.runs;
        self.words += other.words;
        self.sentences += other.sentences;
        self.grounds += other.grounds;
        self.purposes += other.purposes;
    }

    /// Per-run average of a total, or zero when nothing was counted.
    pub fn per_run(&self, total: u64) -> f64 {
        match self.runs {
            0 => 0.0,
            n => total as f64 / n as f64,
        }
    }
}

struct Offender {
    value: f32,
    path: String,
    line: u32,
    name: String,
    /// The band this value was judged against, kept because the judging
    /// budget is per language and cannot be recovered afterwards: a mixed
    /// scan has no single budget for `cognitive` to look up later. Without
    /// it a finding can only say how big it is, never how far out it is.
    band: (Option<f32>, Option<f32>),
    /// What the rest of the file did with the same budget, kept for the
    /// same reason as the band: the file's own measurements are gone by
    /// render time, and re-reading the file to recover them is a second
    /// parse of every file in the scan.
    local: Local,
}

/// What the OTHER bodies in one file did with the budget this one broke.
///
/// A finding states a global rule and a global budget; the reader is
/// standing in a particular file, and the answer to "is this one body
/// that drifted, or is this file like this" decides whether the work is
/// an extraction or an afternoon. Measured on this stage's reference
/// tree: 285 of 344 shape findings (83%) share their file with another
/// body over the same budget, and the ranked list caps at one body per
/// file, so it can never say so on its own.
///
/// It cannot read as an excuse, which was the risk in putting a local
/// distribution beside a global rule: of the 332 findings there with
/// three or more peers, TWO sat at or below their file's median for the
/// metric — `main` reads live span 626 where the next body in the file
/// reads 7, `_build_v5` reads 101 magic numbers where the next reads 6.
#[derive(Default)]
struct Local {
    /// Other bodies here the same metric measured.
    peers: u32,
    /// How many of those broke the same budget.
    over: u32,
    /// The largest peer value that did not — what staying inside looks
    /// like in this file.
    clean: f32,
    /// A peer whose own name claims the same job and stayed inside, with
    /// what it measured. The strongest thing a finding can say, because
    /// it names the shape the author already uses one screen away.
    kin: Option<(Box<str>, f32)>,
}

/// One measurement: the metric, what it read, and the body it read.
type Measured<'a> = (usize, f32, u32, &'a str);

/// One file's whole reading of one metric, before any body asks about
/// itself.
#[derive(Default)]
struct Around<'a> {
    /// Bodies here the metric measured.
    n: u32,
    /// How many broke the budget.
    over: u32,
    /// The largest value that did not.
    clean: f32,
    /// Per job word, the largest clean value and the name that read it.
    kin: HashMap<String, (f32, &'a str)>,
}

impl Around<'_> {
    /// The same neighbourhood seen from one body: itself taken out of
    /// the counts, and the peer that claims its job if there is one.
    ///
    /// Only a body OVER the budget asks — every caller is a violation —
    /// so it is always one of the `over`. Saturating anyway, because a
    /// count that goes backwards should read as "none" rather than wrap
    /// to four billion.
    fn seen_from(&self, name: &str) -> Local {
        let kin = metrics::job_word(name)
            .and_then(|job| self.kin.get(&job))
            .map(|(value, peer)| ((*peer).into(), *value));
        Local {
            peers: self.n.saturating_sub(1),
            over: self.over.saturating_sub(1),
            clean: self.clean,
            kin,
        }
    }
}

/// What every body in this file measured, per metric it broke.
///
/// Only metrics something here broke: a file measures every metric and
/// breaks a handful, and the neighbourhood of a budget nobody crossed is
/// never asked for. Linear — each metric's readings are folded once and
/// every violator of it then reads the same fold.
fn neighbourhoods<'a>(
    measured: &[Measured<'a>],
    budgets: &metrics::Budgets,
) -> HashMap<usize, Around<'a>> {
    let mut broken = [false; N];
    for (m, value, _, _) in measured {
        broken[*m] |= budgets.violates(*m, *value);
    }
    let mut around: HashMap<usize, Around> = HashMap::new();
    for &(m, value, _, name) in measured {
        if !broken[m] {
            continue;
        }
        let seen = around.entry(m).or_default();
        seen.n += 1;
        if budgets.violates(m, value) {
            seen.over += 1;
            continue;
        }
        seen.clean = seen.clean.max(value);
        // A peer must be doing comparable work to be worth naming. Half
        // the budget, because the largest CLEAN value in a file is
        // otherwise routinely a two-line accessor, and "`nm` does the
        // same at 1" is not advice about a 195-line builder.
        if budgets.0[m].1.is_some_and(|hi| value < hi / 2.0) {
            continue;
        }
        // Its own name, not its qualname: the clause has already said
        // the peer is in this file, so the class it hangs off is width
        // spent saying that twice.
        if let Some(job) = metrics::job_word(name) {
            let own = metrics::own_name(name);
            let best = seen.kin.entry(job).or_insert((value, own));
            if value > best.0 {
                *best = (value, own);
            }
        }
    }
    around
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

    /// How far outside the band this sits, as a multiple, so findings from
    /// different metrics can be ranked against each other. `None` where the
    /// budget is zero: "121 times a budget of none" is a count wearing a
    /// ratio's clothes, and mixing the two lets a policy count outrank
    /// every real overage.
    fn severity(&self) -> Option<f32> {
        // Which SIDE was crossed, not which side exists. A two-sided band
        // has both, and testing `hi` first scored a value under the floor
        // against the ceiling it never reached: `comment ratio` at 0%
        // against a 2%-60% band came out 0x.
        match self.band {
            (Some(lo), _) if self.value < lo && lo > 0.0 => Some((lo - self.value) / lo + 1.0),
            (_, Some(hi)) if self.value > hi && hi > 0.0 => Some(self.value / hi),
            _ => None,
        }
    }

    /// The values behind the finding: what was measured against what it
    /// had to beat. Built from the band rather than by editing its label,
    /// which on a two-sided band produced `0%2%-60%`.
    fn over(&self, m: usize) -> String {
        let def = &METRICS[m];
        let show = |v: f32| fmt(def, v);
        match self.band {
            (Some(lo), _) if self.value < lo => format!("{}<{}", show(self.value), show(lo)),
            (_, Some(hi)) => format!("{}>{}", show(self.value), show(hi)),
            (Some(lo), None) => format!("{}<{}", show(self.value), show(lo)),
            _ => show(self.value),
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
/// A clone class holds its FIRST site inline.
///
/// 89.9% of classes have exactly one site — 698,800 of 777,638 on the
/// TypeScript gold corpus — and a `Vec` for a single 16-byte element is
/// a heap block, a capacity and a pointer chase for a class that will
/// never be reported. Only a class that recurs allocates.
struct CloneClass {
    mass: u32,
    first: CloneLoc,
    rest: Vec<CloneLoc>,
}

impl CloneClass {
    fn first(mass: u32, loc: CloneLoc) -> CloneClass {
        CloneClass {
            mass,
            first: loc,
            rest: Vec::new(),
        }
    }

    fn push(&mut self, loc: CloneLoc) {
        self.rest.push(loc);
    }

    fn absorb(&mut self, mut other: CloneClass) {
        self.rest.push(other.first);
        self.rest.append(&mut other.rest);
    }

    fn len(&self) -> usize {
        1 + self.rest.len()
    }

    fn sites(&self) -> impl Iterator<Item = &CloneLoc> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }
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

/// Which corpus-sized accumulators a run will read.
///
/// Everything else in `Agg` is a counter or a distribution and costs the
/// same whatever the corpus is. These nine grow WITH the tree — a clone
/// class per candidate subtree, a print per unit, a graph row per file —
/// and each mode reads only some of them. Building the rest is memory
/// spent on an answer nobody asks for: `calibrate` reads distributions
/// only, and on the gold corpus it was carrying a 777,638-entry clone map
/// to the end of the scan and dropping it unread.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Wants {
    pub clones: bool,
    pub prints: bool,
    pub graph: bool,
    pub mentions: bool,
    pub synonyms: bool,
    pub test_refs: bool,
    pub untested: bool,
    pub clumps: bool,
    /// Switch signatures and record shapes: one section renders both.
    pub sets: bool,
}

impl Wants {
    /// Distributions only — what `calibrate` and `--brief` need.
    pub const NONE: Wants = Wants {
        clones: false,
        prints: false,
        graph: false,
        mentions: false,
        synonyms: false,
        test_refs: false,
        untested: false,
        clumps: false,
        sets: false,
    };

    pub const ALL: Wants = Wants {
        clones: true,
        prints: true,
        graph: true,
        mentions: true,
        synonyms: true,
        test_refs: true,
        untested: true,
        clumps: true,
        sets: true,
    };
}

/// An accumulator that is only filled when something will read it.
///
/// A disabled accumulator is EMPTY, and an empty one is indistinguishable
/// from a real result with nothing in it — which is how a section renders
/// blank under a green suite. That failure has a history here: nine
/// detector families were dying silently because nothing checked they
/// were reachable. So a read of a disabled accumulator panics where a
/// test can see it, and `every_mode_asks_for_what_it_reads` runs every
/// mode to prove no renderer touches state its mode did not request.
pub struct Gated<T> {
    on: bool,
    name: &'static str,
    value: T,
}

impl<T> Gated<T> {
    fn new(name: &'static str, on: bool, value: T) -> Gated<T> {
        Gated { on, name, value }
    }

    fn wanted(&self) -> bool {
        self.on
    }

    /// Read it. Panics in a debug build if this mode never asked for it.
    pub fn read(&self) -> &T {
        debug_assert!(
            self.on,
            "`{}` was read by a mode that did not ask for it — add it to that mode's Wants, \
             or the section reading it renders blank on a green suite",
            self.name
        );
        &self.value
    }

    pub fn read_mut(&mut self) -> &mut T {
        debug_assert!(
            self.on,
            "`{}` was mutated by a mode that did not ask for it",
            self.name
        );
        &mut self.value
    }

    /// Fold another accumulator in, or drop it. Both sides carry the
    /// same flag: they came from one `Wants`.
    fn merge(&mut self, other: &mut Gated<T>, f: impl FnOnce(&mut T, &mut T)) {
        if self.on {
            f(&mut self.value, &mut other.value);
        }
    }

    /// Fill it, or do nothing. Dropping the write IS the saving.
    fn fill(&mut self, f: impl FnOnce(&mut T)) {
        if self.on {
            f(&mut self.value);
        }
    }
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
    /// Units per language. Every metric here is per-unit or per-file, so
    /// a per-language RATE needs a per-language denominator on both sides
    /// of any comparison: dividing one language's violations by the whole
    /// run's units is how a corpus comparison once reported `params`
    /// firing 651x more often on admired code than on real code.
    units_by_lang: [u64; LANGS.len()],
    /// Per language, per metric, what that language measured and what
    /// broke its budget. Neither is recoverable from the pooled totals:
    /// budgets are per-language, and a metric's measurement domain is
    /// narrower than "units" for most of them.
    metric_by_lang: [[LangTally; N]; LANGS.len()],
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
    clumps: Gated<HashMap<String, Clump>>,
    /// Case-label sets by key: the same dispatch repeated across the
    /// codebase (every new variant forces N edits).
    switches: Gated<HashMap<String, Clump>>,
    /// Anonymous record shapes by key set: the same fields built in many
    /// places is a type nobody declared.
    shapes: Gated<HashMap<String, Clump>>,
    /// Per-language spelling counts of unit names, for idiom entropy.
    spellings: [[u32; metrics::CASES]; LANGS.len()],
    /// Per-language, per-role comment tallies. A budget for how much a
    /// comment should say has to be pinned per ROLE — a field's doc is
    /// a phrase and a module header is a page — so the distribution
    /// they are pinned against has to be readable per role first.
    comments: [[Tally; CommentRole::ALL.len()]; LANGS.len()],
    /// Per-unit fingerprints, for near-clones the Merkle hash cannot see.
    prints: Gated<Vec<crate::near::Print>>,
    clones: Gated<HashMap<u64, CloneClass>>,
    /// Per-file module-graph facts, resolved at render time (two-phase:
    /// resolution needs the whole file set).
    pub graph: Gated<Vec<crate::graph::GraphFacts>>,
    /// Per-language narrative ordering sums:
    /// [down refs, up refs, public-first pairs, public/private pairs].
    narrative: [[u64; 4]; LANGS.len()],
    /// Object -> the synonymous verbs the codebase reaches it by. Two
    /// names for one operation means a reader must learn both and a
    /// searcher will find half the call sites.
    synonyms: Gated<HashMap<Box<str>, std::collections::BTreeSet<&'static str>>>,
    /// Names referenced anywhere in test code (global association set).
    test_refs: Gated<std::collections::HashSet<Box<str>>>,
    /// How many FILES mention each identifier. An export mentioned by one
    /// file is mentioned only where it is defined — nobody consumes it.
    mentions: Gated<HashMap<Box<str>, u32>>,
    /// Complexity-over-budget production units awaiting the test join —
    /// McCabe's actual meaning: cyclomatic is a minimum test count.
    untested_candidates: Gated<Vec<UntestedCandidate>>,
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

/// One language's tally for one metric: how many measurements it made,
/// and how many of them broke that language's budget. `measured` is the
/// exact denominator — `units` over-counts for every metric a module
/// scope, a test body or an untyped language never reaches.
#[derive(Clone, Copy)]
struct LangTally {
    measured: u64,
    violated: u64,
}

impl LangTally {
    const ZERO: LangTally = LangTally {
        measured: 0,
        violated: 0,
    };
}

impl Agg {
    #[cfg(test)]
    pub fn new() -> Agg {
        Agg::configured(
            crate::config::Layers::flat(LangBudgets::defaults()),
            false,
            Wants::ALL,
        )
    }

    /// Retains every violation — required for --json and baselines.
    #[cfg(test)]
    pub fn complete() -> Agg {
        Agg::configured(
            crate::config::Layers::flat(LangBudgets::defaults()),
            true,
            Wants::ALL,
        )
    }

    /// `complete` retains every violation (machine output, baselines);
    /// display mode caps them.
    pub fn configured(budgets: crate::config::Layers, complete: bool, wants: Wants) -> Agg {
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
            units_by_lang: [0; LANGS.len()],
            metric_by_lang: [[LangTally::ZERO; N]; LANGS.len()],
            violations_n: [0; N],
            total_mass: 0,
            cap: if complete { usize::MAX } else { KEEP },
            low_confidence: Vec::new(),
            clumps: Gated::new("clumps", wants.clumps, HashMap::new()),
            switches: Gated::new("switches", wants.sets, HashMap::new()),
            shapes: Gated::new("shapes", wants.sets, HashMap::new()),
            spellings: [[0; metrics::CASES]; LANGS.len()],
            comments: [[Tally::default(); CommentRole::ALL.len()]; LANGS.len()],
            prints: Gated::new("prints", wants.prints, Vec::new()),
            clones: Gated::new("clones", wants.clones, HashMap::new()),
            graph: Gated::new("graph", wants.graph, Vec::new()),
            narrative: [[0; 4]; LANGS.len()],
            synonyms: Gated::new("synonyms", wants.synonyms, HashMap::new()),
            test_refs: Gated::new(
                "test refs",
                wants.test_refs,
                std::collections::HashSet::new(),
            ),
            breaches: Vec::new(),
            declared_layers: 0,
            mentions: Gated::new("mentions", wants.mentions, HashMap::new()),
            untested_candidates: Gated::new("untested candidates", wants.untested, Vec::new()),
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
    ///
    /// Its IMPORTS are not among them. A parse breaks in the body, never
    /// in the header block that precedes it: 5280 of the 5365 `#include`
    /// lines inside low-confidence C-family files extract correctly and
    /// none extracts spuriously. Dropping them cost 165 orphans — curl
    /// 16 of 16, ctre 14 of 14, fmt 9 of 9, cutlass 89 of 171 — and 8 of
    /// fmt's 9 were headers its own format.h includes.
    fn count_unmeasurable(&mut self, facts: &FileFacts) {
        self.graph.fill(|g| {
            g.push(crate::graph::GraphFacts {
                path: facts.path.clone(),
                lang: facts.lang,
                is_test: facts.is_test_file,
                imports: facts.imports.clone(),
                exports: Vec::new(),
                mass: 0,
                surface_cost: 0,
            })
        });
        self.low_confidence.push(facts.path.display().to_string());
    }

    fn absorb(&mut self, facts: &FileFacts) {
        let path = facts.path.display().to_string();
        let shared: std::sync::Arc<str> = path.as_str().into();
        let lang = facts.lang as usize;
        self.units += (facts.units.len() - 1) as u64;
        self.units_by_lang[lang] += (facts.units.len() - 1) as u64;
        self.test_units += facts.units.iter().filter(|u| u.is_test).count() as u64;
        self.total_mass += facts.mass as u64;
        self.junk_files += is_junk_drawer(facts) as u32;
        self.collect_recurrences(facts, &path, &shared);
        self.graph.fill(|g| g.push(graph_facts(facts)));
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
            self.prints.fill(|p| {
                p.push(crate::near::Print {
                    label: format!("{path}:{}  {}", u.line, u.qualname),
                    prints: u.fingerprints.clone(),
                })
            });
        }
        self.collect_spellings(facts);
        self.collect_comments(facts);
        self.collect_synonyms(facts);
        self.test_refs
            .fill(|t| t.extend(facts.test_refs.iter().cloned()));
        self.mentions.fill(|m| {
            for name in &facts.mentioned {
                *m.entry(name.clone()).or_insert(0) += 1;
            }
        });
        self.collect_untested(facts, &path);
        self.measure(facts, &path);
    }

    /// Measure one file and record what broke, each finding carrying what
    /// the rest of the file did with the same budget.
    ///
    /// Two passes over the file's own measurements rather than one,
    /// because a finding cannot be told about peers the walk has not
    /// reached yet. The measurements are held between them, not the
    /// source: `for_each` LENDS its labels, so the hold costs one `Vec`
    /// per file and not a string per measurement.
    fn measure(&mut self, facts: &FileFacts, path: &str) {
        let lang = facts.lang as usize;
        let budgets = *self.budgets.for_file(&facts.path).for_lang(facts.lang);
        let mut measured: Vec<Measured> = Vec::new();
        metrics::for_each(facts, |m, value, line, name| {
            if let Some(r) = metrics::RATE_METRICS.iter().position(|x| *x == m) {
                let cell = &mut self.rates[lang][r];
                cell.0 += (value > 0.0) as u64;
                cell.1 += 1;
            }
            self.dists[m].push(value);
            self.metric_by_lang[lang][m].measured += 1;
            if budgets.violates(m, value) {
                self.metric_by_lang[lang][m].violated += 1;
                self.violations_n[m] += 1;
            }
            measured.push((m, value, line, name));
        });
        let around = neighbourhoods(&measured, &budgets);
        for (m, value, line, name) in measured {
            if !budgets.violates(m, value) {
                continue;
            }
            let o = Offender {
                value,
                path: path.to_string(),
                line,
                name: name.to_string(),
                band: budgets.0[m],
                local: around[&m].seen_from(name),
            };
            push_offender(&mut self.offenders[m], o, self.cap);
        }
    }

    /// The codebase-wide recurrences this file contributes to: clone
    /// classes, parameter clumps, and repeated dispatch.
    fn collect_recurrences(&mut self, facts: &FileFacts, path: &str, shared: &std::sync::Arc<str>) {
        self.clones.fill(|classes| {
            for site in &facts.clone_sites {
                let loc = CloneLoc {
                    path: shared.clone(),
                    line: site.line,
                    end_line: site.end_line,
                };
                match classes.entry(site.hash) {
                    std::collections::hash_map::Entry::Occupied(mut e) => e.get_mut().push(loc),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(CloneClass::first(site.mass, loc));
                    }
                }
            }
        });
        for u in &facts.units {
            self.collect_clumps(u, path);
        }
        self.switches
            .fill(|w| note_sets(w, &facts.switch_sigs, path));
        self.shapes
            .fill(|w| note_sets(w, &facts.record_shapes, path));
    }

    /// Over-budget production units, held for the test-name join that
    /// restores cyclomatic to McCabe's meaning: a minimum test count.
    fn collect_untested(&mut self, facts: &FileFacts, path: &str) {
        let here = self.budgets.for_file(&facts.path);
        let cyc_budget = here.for_lang(facts.lang).0[metrics::CYCLOMATIC].1;
        for u in &facts.units {
            let (_, cyc) = metrics::complexity(u);
            if !u.is_test && !u.is_module && cyc_budget.is_some_and(|hi| cyc as f32 > hi) {
                self.untested_candidates.fill(|c| {
                    c.push(UntestedCandidate {
                        name: u.name.clone(),
                        label: format!("{path}:{}  {}", u.line, u.qualname),
                        cyclomatic: cyc,
                    })
                });
            }
        }
    }

    /// Fold the corpus-sized accumulators of one partial aggregate into
    /// another. Each is a no-op when this run never asked for it, so a
    /// mode that reads none of them pays nothing to reduce.
    fn merge_accumulators(&mut self, b: &mut Agg) {
        self.clones.merge(&mut b.clones, |dst, src| {
            for (hash, class) in src.drain() {
                match dst.entry(hash) {
                    std::collections::hash_map::Entry::Occupied(mut e) => e.get_mut().absorb(class),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(class);
                    }
                }
            }
        });
        self.clumps.merge(&mut b.clumps, merge_recurrences);
        self.switches.merge(&mut b.switches, merge_recurrences);
        self.shapes.merge(&mut b.shapes, merge_recurrences);
        self.prints.merge(&mut b.prints, |d, s| d.append(s));
        self.graph.merge(&mut b.graph, |d, s| d.append(s));
        self.synonyms.merge(&mut b.synonyms, |d, s| {
            for (object, verbs) in s.drain() {
                d.entry(object).or_default().extend(verbs);
            }
        });
        self.test_refs
            .merge(&mut b.test_refs, |d, s| d.extend(s.drain()));
        self.mentions.merge(&mut b.mentions, |d, s| {
            for (name, n) in s.drain() {
                *d.entry(name).or_insert(0) += n;
            }
        });
        self.untested_candidates
            .merge(&mut b.untested_candidates, |d, s| d.append(s));
    }

    /// Fold the per-LANGUAGE tables. Each is a fixed array indexed by
    /// `Lang` and each folds elementwise; only what an element IS
    /// differs, which is why they belong together rather than strung
    /// through the counters above them.
    fn merge_by_lang(&mut self, b: &Agg) {
        for (i, n) in b.files_by_lang.iter().enumerate() {
            self.files_by_lang[i] += n;
        }
        for (i, n) in b.units_by_lang.iter().enumerate() {
            self.units_by_lang[i] += n;
        }
        for (lang, row) in b.metric_by_lang.iter().enumerate() {
            for (m, t) in row.iter().enumerate() {
                self.metric_by_lang[lang][m].measured += t.measured;
                self.metric_by_lang[lang][m].violated += t.violated;
            }
        }
        for (lang, row) in b.narrative.iter().enumerate() {
            for (k, v) in row.iter().enumerate() {
                self.narrative[lang][k] += v;
            }
        }
        for (lang, row) in b.spellings.iter().enumerate() {
            for (case, n) in row.iter().enumerate() {
                self.spellings[lang][case] += n;
            }
        }
        for (lang, row) in b.rates.iter().enumerate() {
            for (r, (covered, total)) in row.iter().enumerate() {
                self.rates[lang][r].0 += covered;
                self.rates[lang][r].1 += total;
            }
        }
        self.absorb_comments(&b.comments);
    }

    /// Fold the per-METRIC tables: distributions concatenate, violation
    /// counts add, and offenders re-enter the retention heap so the cap
    /// applies to the merged list rather than to each half.
    fn merge_by_metric(&mut self, b: &mut Agg) {
        let cap = self.cap;
        for m in 0..N {
            self.violations_n[m] += b.violations_n[m];
            self.dists[m].append(&mut b.dists[m]);
            for o in b.offenders[m].drain(..) {
                push_offender(&mut self.offenders[m], o, cap);
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
        a.merge_by_lang(&b);
        a.merge_accumulators(&mut b);
        a.merge_by_metric(&mut b);
        a
    }

    /// Fowler's Data Clumps: 3- and 4-name parameter groups per unit,
    /// counted across the whole run. The same names traveling together
    /// through >=3 signatures are a struct the language was never told
    /// about.
    fn collect_clumps(&mut self, u: &crate::facts::UnitFacts, path: &str) {
        // The combination walk below is O(n^4) in parameter count, so a
        // mode that never reads clumps should not pay for it either.
        if !self.clumps.wanted() || u.params.len() < 3 || u.params.len() > 8 {
            return;
        }
        let mut names: Vec<&str> = u.params.iter().map(|p| &*p.name).collect();
        names.sort_unstable();
        let site = format!("{path}:{}  {}", u.line, u.name);
        let n = names.len();
        let mut add = |group: &[&str]| {
            let key = group.join("\u{1f}");
            let entry = self.clumps.read_mut().entry(key).or_insert(Clump {
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

    /// What this file's comments are for, and how much they say.
    fn collect_comments(&mut self, facts: &FileFacts) {
        let row = &mut self.comments[facts.lang as usize];
        for c in &facts.comments {
            row[c.role as usize].add(Tally {
                runs: 1,
                words: c.prose.words as u64,
                sentences: c.prose.sentences as u64,
                grounds: c.prose.grounds as u64,
                purposes: c.prose.purposes as u64,
            });
        }
    }

    /// Another aggregate's comment tallies, added to this one's.
    fn absorb_comments(&mut self, other: &[[Tally; CommentRole::ALL.len()]]) {
        for (lang, row) in other.iter().enumerate() {
            for (role, tally) in row.iter().enumerate() {
                self.comments[lang][role].add(*tally);
            }
        }
    }

    /// What each role's comments amount to in this corpus, per
    /// language. Read by `calibrate`, which is the only caller that has
    /// a corpus worth asking.
    pub fn comment_roles(&self, lang: crate::lang::Lang) -> &[Tally] {
        &self.comments[lang as usize]
    }

    /// Which synonymous verb each named object is reached by. Only
    /// public units count: an internal helper may call its operation
    /// whatever it likes, but the surface is vocabulary others must
    /// learn.
    fn collect_synonyms(&mut self, facts: &FileFacts) {
        if !self.synonyms.wanted() {
            return;
        }
        for u in &facts.units {
            if u.is_module || !u.is_public || u.is_test {
                continue;
            }
            if let Some((verb, object)) = crate::metrics::split_synonym(&u.name) {
                self.synonyms
                    .read_mut()
                    .entry(object.into())
                    .or_default()
                    .insert(verb);
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
            self.metric_by_lang[lang as usize][m].measured += 1;
            if budgets.violates(m, value) {
                self.metric_by_lang[lang as usize][m].violated += 1;
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
            .read()
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
    fn uniform_budget(&self, m: usize) -> Option<Band> {
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
        let label = match self.uniform_budget(m) {
            Some(band) => metrics::band_label(m, band),
            None => self.mixed_budget(m),
        };
        // A trailing dot marks a budget resting on nothing but the
        // compiled default: the corpus was too thin to pin it, or it
        // is a policy no percentile may legitimize. Printing a pinned
        // budget and a guessed one identically implies evidence the
        // tool does not have.
        match self.budget_is_pinned(m) {
            true => label,
            false => format!("{label}."),
        }
    }

    /// Every band this run judged against, most files first, languages
    /// sharing one named together.
    ///
    /// `varies` named the problem and then withheld the answer. On a
    /// mixed tree the metrics that fire most are exactly the ones
    /// calibrated per language, so the word landed on the rows a reader
    /// most needs the number for — twelve of them on the reference
    /// tree, `cognitive`, `cyclomatic`, `length` and `live span` among
    /// them — and said only that the tool knew and would not say.
    fn mixed_budget(&self, m: usize) -> String {
        let mut langs: Vec<crate::lang::Lang> = LANGS
            .iter()
            .copied()
            .filter(|l| self.files_by_lang[*l as usize] > 0)
            .collect();
        langs.sort_by_key(|l| {
            (
                std::cmp::Reverse(self.files_by_lang[*l as usize]),
                *l as usize,
            )
        });
        let mut groups: Vec<(Band, Vec<&'static str>)> = Vec::new();
        for lang in langs {
            let band = self.budgets.root().for_lang(lang).0[m];
            match groups.iter_mut().find(|(seen, _)| *seen == band) {
                Some((_, names)) => names.push(lang.name()),
                None => groups.push((band, vec![lang.name()])),
            }
        }
        let named = groups.iter().take(BUDGET_LANGS);
        let shown: Vec<String> = named
            .map(|(band, names)| format!("{} {}", metrics::band_label(m, *band), names.join("/")))
            .collect();
        match groups.len().saturating_sub(BUDGET_LANGS) {
            0 => shown.join(" · "),
            rest => format!("{} +{rest}", shown.join(" · ")),
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

/// Findings shown per ranked section, and rows in the shape table.
const SHOW_RANKED: usize = 5;
const SHOW_POLICY: usize = 3;
const SHOW_SHAPE: usize = 4;

/// The report as a page. It answers "what kind of trouble is this, and
/// where do I start", which is the question asked before anyone has
/// decided to look; `--full` answers "what should I fix", which is the
/// question asked after.
///
/// Findings are ranked by how far outside the budget they sit, so units
/// from unrelated metrics can be compared. The rung still picks the
/// SECTION — sorting by distance across rungs floats a 3x `abbreviated`
/// above a 34x `live span` purely because token hygiene is rung 0.
///
/// One entry is one BODY, not one finding: the body-shape metrics move
/// together, so a row per metric named a different function five times
/// for one fact and left the functions breaking six budgets each
/// unnamed.
pub fn render(agg: &mut Agg, ink: ink::Ink) -> String {
    let page = Page {
        ink,
        trim: shared_prefix(agg),
        costs: Costs::read(agg),
    };
    let mut out = headline(agg);
    while out.ends_with("\n\n") {
        out.pop();
    }
    render_ranked(agg, &page, &mut out);
    render_policy(agg, &page, &mut out);
    render_shape(agg, ink, &mut out);
    render_aligned(agg, &page, &mut out);
    render_where(agg, ink, &mut out);
    out
}

/// What every ranked section needs and none of them can recompute
/// cheaply: how to colour, how much of each path is boilerplate every
/// finding shares, and what a finding costs beyond its own body. Built
/// once so the import graph is analysed once, which is what it cost
/// before the cost clause existed.
struct Page {
    ink: ink::Ink,
    trim: usize,
    costs: Costs,
}

impl Page {
    /// Ask what each entry costs beyond itself. Asked AFTER the ranking
    /// and never before it, so the answer can change what a reader is
    /// told and not which body they are told about.
    fn costed<'a>(&self, bodies: Vec<Carried<'a>>) -> Vec<Carried<'a>> {
        bodies
            .into_iter()
            .map(|g| Carried {
                cost: self.costs.of(&g),
                ..g
            })
            .collect()
    }
}

/// Bytes of leading path every finding shares, cut at a separator. Whole
/// absolute paths repeated down a report cost more width than the part
/// that differs, and the part that differs is the only part read.
fn shared_prefix(agg: &Agg) -> usize {
    let mut paths = agg.offenders.iter().flatten().map(|o| o.path.as_str());
    let Some(first) = paths.next() else {
        return 0;
    };
    let mut common = first.len();
    for path in paths {
        common = common.min(
            first
                .bytes()
                .zip(path.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    first[..common].rfind(['/', '\\']).map_or(0, |cut| cut + 1)
}

/// Widest metric name, so the column never runs into the next one.
/// `wildcard match` is fourteen characters and a hardcoded 14 printed it
/// as `wildcard match3>1`.
const NAME_COL: usize = 15;

/// Which body a finding is about: file, line, and the unit's name. A
/// file-level finding has an empty name, so those group per file.
type Body<'a> = (&'a str, u32, &'a str);

/// A metric and the finding it produced there.
type Finding<'a> = (usize, &'a Offender);

/// A budget as a low and a high, either of which may be absent.
type Band = (Option<f32>, Option<f32>);

/// Every rankable finding one body carries, as one entry.
///
/// The body-shape metrics state ONE fact between them, and the tool
/// already knows it — `aligned_tensions` says so in as many words and
/// collapses them for rung 7. Measured per unit on this stage's
/// reference tree: P(cognitive | cyclomatic) 93%, P(live span | length)
/// 96%, P(loop depth | depth) 93%. So a list capped at one row per
/// metric spent its rows restating one fact about five bodies: 646
/// findings sat on 391 units there, and 94 of those units carried 349 of
/// them.
struct Carried<'a> {
    /// Any of the findings — they share path, line and name. Kept for
    /// the location, which is what the entry is about.
    at: &'a Offender,
    /// Metric index and its finding, furthest out first.
    findings: Vec<Finding<'a>>,
    /// The largest distance among them: what the entry is ranked by.
    ///
    /// Ranking by the COUNT instead — one edit clearing six budgets
    /// before one clearing four — was measured on the reference tree and
    /// rejected. It put a body 2x past on five budgets above the body
    /// 13x past on four, and dropped the furthest-out body in the tree
    /// to tenth. Breadth is worth showing and is; it is not worth
    /// ranking by.
    worst: f32,
    /// What this body costs beyond itself, once a `Costs` has been
    /// asked. Ranking must not depend on it: a load-bearing file is a
    /// reason to be careful, not evidence of a bigger overage.
    cost: Option<String>,
    /// How many OTHER bodies make the identical claim, to the digit.
    /// Five copies of a generated locale file each read `demeter 35>2`,
    /// and a list that named all five said one thing five times.
    alike: usize,
}

impl Carried<'_> {
    /// What this entry claims, exactly: which budgets, at which values.
    /// Two bodies with the same claim are one finding about a repeated
    /// shape, however many files it was pasted into.
    fn claim(&self) -> Vec<(usize, u32)> {
        let mut claim: Vec<(usize, u32)> = self
            .findings
            .iter()
            .map(|(m, o)| (*m, o.value.to_bits()))
            .collect();
        claim.sort_unstable();
        claim
    }
}

/// Metric names and values one entry lists before the line stops being
/// read. A WIDTH rather than a count: `depth 5>3` and `magic numbers
/// 101>11` are not the same size, so a fixed count of four printed a
/// 41-column line beside a 96-column one.
const CARRIED_WIDTH: usize = 74;

/// Width of the "how many budgets" column, sized for `12 suspicions`.
const CARRIED_COL: usize = 13;

/// Two lines per body: how far out it is and how many budgets say so,
/// then every one of them. `4697x` rather than `4697.4x` — a tenth of a
/// multiple has never changed anyone's mind about which function to
/// open.
fn carried_lines(g: &Carried, noun: &str, trim: usize, tint: (ink::Ink, &str)) -> String {
    let (ink, colour) = tint;
    let at = g.at;
    let sev = g.worst.round() as u64;
    let budgets = match g.findings.len() {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    };
    let unit = match at.name.is_empty() {
        true => String::new(),
        false => format!("  {}", at.name),
    };
    // Then outward: what the rest of this FILE does with the same
    // budget, then what the body costs beyond its own file. Neither is
    // another budget, so neither belongs on the line that lists them.
    let aside = |clause: Option<String>| match clause {
        None => String::new(),
        Some(clause) => format!("\n          {}{clause}{}", ink.faint(), ink.off()),
    };
    let cost = aside(neighbours(g.findings[0].0, at)) + &aside(g.cost.clone());
    format!(
        // Indented to the budget column, so the metric list reads as a
        // continuation of the line that counted it rather than as
        // another finding.
        "  {colour}{sev:>5}x{}  {budgets:<CARRIED_COL$}{}{}:{}{}{unit}\n          {}{}{}{cost}",
        ink.off(),
        ink.faint(),
        &at.path[trim.min(at.path.len())..],
        at.line,
        ink.off(),
        ink.faint(),
        carried_metrics(g),
        ink.off(),
    )
}

/// A finding repeated this often in one file is a fact about the FILE,
/// not about the body: one edit there answers for all of them. Eight
/// because a file with eight of anything wrote it on purpose.
const REPEATED: u32 = 8;

/// What a finding costs beyond its own body.
///
/// Every fact here is one the report already computed for rung 7 and
/// then printed as an integer — `render_aligned` calls
/// `aligned_tensions`, counts the result and throws the structure away.
/// On the reference tree that discarded `config.py` carrying the
/// report's second-worst body while 17 of the tree's 39 modules import
/// it: both facts known, neither ever on the same line.
struct Costs {
    /// Files enough of the codebase imports that changing them is costly.
    heavy: HashMap<String, u32>,
    /// Over-budget bodies no test names, by label.
    untested: std::collections::HashSet<String>,
    /// How many findings each (metric, file) pair holds.
    repeats: HashMap<(usize, String), u32>,
}

impl Costs {
    fn read(agg: &Agg) -> Costs {
        let mut repeats: HashMap<(usize, String), u32> = HashMap::new();
        for (m, os) in agg.offenders.iter().enumerate() {
            for o in os {
                let seen = repeats.entry((m, o.path.clone())).or_default();
                *seen += 1;
            }
        }
        let untested = select_untested(agg).into_iter().map(|(label, _)| label);
        Costs {
            heavy: load_bearing_files(agg),
            untested: untested.collect(),
            repeats,
        }
    }

    /// How many findings this metric has in that file.
    fn repeats_in(&self, m: usize, path: &str) -> Option<u32> {
        let found = self.repeats.get(&(m, path.to_string()));
        found.copied()
    }

    /// The metric this body trips most often across the whole file, and
    /// how often.
    fn repeated(&self, g: &Carried) -> Option<(usize, u32)> {
        let path = &g.at.path;
        g.findings
            .iter()
            .filter_map(|(m, _)| Some((*m, self.repeats_in(*m, path)?)))
            .max_by_key(|(_, n)| *n)
    }

    /// The rarest fact that is true of this body, or none.
    ///
    /// Rarest, because they stack — a body can be load-bearing AND
    /// untested AND one of a hundred like it — and a clause per fact
    /// would put the entry back where the grouping found it. Measured
    /// over the reference tree's 391 over-budget bodies: a load-bearing
    /// file covers 4.1% of them, an untested body 7.9%, a repeated
    /// finding 44.5%, an identical claim elsewhere 64.2%.
    fn of(&self, g: &Carried) -> Option<String> {
        let at = g.at;
        if let Some(n) = self.heavy.get(&at.path) {
            return Some(format!("{n} files import this one"));
        }
        if self.untested.contains(&at.label()) {
            return Some("no test names it".to_string());
        }
        if let Some((m, n)) = self.repeated(g).filter(|(_, n)| *n >= REPEATED) {
            return Some(format!("{n} {} findings in this one file", METRICS[m].name));
        }
        // Last, and commonest: the entry stands for bodies the list
        // collapsed, so the count has to be said somewhere.
        match g.alike {
            0 => None,
            n => Some(format!("{n} more bodies carry these same numbers")),
        }
    }
}

/// What this body's own file already does with the budget it broke
/// worst — the fact a finding was missing, because every other fact on
/// the line is about the global rule.
///
/// One clause, chained most useful first, because all three answer the
/// same question and a line each would put the entry back where the
/// grouping found it. In order:
///
///   a peer that claims the same job and stayed inside — the shape the
///   author already uses, one screen away, and the only clause that
///   names a target rather than describing a situation;
///
///   nothing else here is over — the file knows how to do this and one
///   body drifted, so the work is an extraction;
///
///   others here are over too — the unit of work is the FILE, which the
///   ranked list can never say for itself because it shows at most one
///   body per file.
///
/// Measured on the reference tree: the third case is 83% of shape
/// findings, and the first covers 29 of 49 cognitive findings, 23 of 43
/// cyclomatic and 16 of 27 depth. It is thinnest exactly where severity
/// ranking looks — `main`, `<module>` and `generate_spice` have no
/// namesake by construction — which is where the second clause has the
/// most to say.
fn neighbours(m: usize, at: &Offender) -> Option<String> {
    let local = &at.local;
    let show = |v: f32| fmt(&METRICS[m], v);
    let name = METRICS[m].name;
    if let Some((peer, value)) = &local.kin {
        return Some(format!(
            "{name} — {peer} does the same job at {}",
            show(*value)
        ));
    }
    if local.peers == 0 {
        return None;
    }
    if local.over > 0 {
        let (over, all) = (local.over + 1, local.peers + 1);
        return Some(format!("{name} — {over} of {all} bodies here are over it"));
    }
    // What staying inside looks like here, but only against a CEILING:
    // under a floor the largest clean peer is the FURTHEST from the
    // finding, and "the other 30 peak at 12 asserts" answers nothing
    // about a body that has none.
    let peak = match at.band.1 {
        Some(hi) if at.value > hi => {
            format!(", the other {} peak at {}", local.peers, show(local.clean))
        }
        _ => format!(", the other {} stay inside", local.peers),
    };
    Some(format!("{name} — alone here{peak}"))
}

/// Every budget the body broke, worst first, until the line is as wide
/// as it may be — then a count of what did not fit, so nothing the entry
/// stands for goes unmentioned.
fn carried_metrics(g: &Carried) -> String {
    let mut shown = String::new();
    for (i, (m, o)) in g.findings.iter().enumerate() {
        let one = format!("{} {}", METRICS[*m].name, o.over(*m));
        if !shown.is_empty() && shown.len() + 2 + one.len() > CARRIED_WIDTH {
            let _ = write!(shown, "  +{} more", g.findings.len() - i);
            break;
        }
        if !shown.is_empty() {
            shown.push_str("  ");
        }
        shown.push_str(&one);
    }
    shown
}

/// One entry per body, worst first, at most one body per file.
///
/// The per-file cap survives the regrouping for the reason it was added:
/// one generated lookup table carrying 51,671 magic numbers took two of
/// six slots and said the same thing twice. The per-METRIC cap does not,
/// because it was the thing forcing five rows to name five bodies for
/// one fact.
///
/// A finding a zero budget leaves unrankable stays out, as it always
/// has: "121 times a budget of none" is a count wearing a ratio's
/// clothes, and the policy section names those per metric. So the count
/// on an entry is of gates that have a distance, not of every gate the
/// body trips.
fn ranked_units(agg: &Agg, rungs: std::ops::RangeInclusive<u8>, k: usize) -> Vec<Carried<'_>> {
    let mut by_body: HashMap<Body, Vec<Finding>> = HashMap::new();
    for (m, o) in rankable(agg, rungs) {
        by_body
            .entry((&o.path, o.line, &o.name))
            .or_default()
            .push((m, o));
    }
    let distance = |(_, o): &Finding| o.severity().unwrap_or_default();
    let mut bodies: Vec<Carried> = by_body
        .into_values()
        .map(|mut findings| {
            findings.sort_by(|a, b| {
                distance(b)
                    .total_cmp(&distance(a))
                    .then_with(|| METRICS[a.0].name.cmp(METRICS[b.0].name))
            });
            Carried {
                at: findings[0].1,
                worst: distance(&findings[0]),
                findings,
                cost: None,
                alike: 0,
            }
        })
        .collect();
    bodies.sort_by(|a, b| {
        b.worst
            .total_cmp(&a.worst)
            .then_with(|| b.findings.len().cmp(&a.findings.len()))
            .then_with(|| (&a.at.path, a.at.line).cmp(&(&b.at.path, b.at.line)))
    });
    let mut class: HashMap<Vec<(usize, u32)>, usize> = HashMap::new();
    for g in &bodies {
        *class.entry(g.claim()).or_default() += 1;
    }
    let mut seen_file: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut seen_claim: std::collections::HashSet<Vec<(usize, u32)>> =
        std::collections::HashSet::new();
    bodies.retain(|g| seen_file.insert(&g.at.path) && seen_claim.insert(g.claim()));
    bodies.truncate(k);
    for g in &mut bodies {
        g.alike = class[&g.claim()] - 1;
    }
    bodies
}

/// Distinct bodies carrying a rankable finding at these rungs. Read off
/// the same offenders the list ranks, so the gloss and the entries can
/// never disagree about how much is hidden below the fold.
fn bodies_at(agg: &Agg, rungs: std::ops::RangeInclusive<u8>) -> usize {
    let places = rankable(agg, rungs).map(|(_, o)| (o.path.as_str(), o.line, o.name.as_str()));
    let seen: std::collections::HashSet<Body> = places.collect();
    seen.len()
}

/// Every finding at these rungs that has a distance to be ranked by.
fn rankable(
    agg: &Agg,
    rungs: std::ops::RangeInclusive<u8>,
) -> impl Iterator<Item = Finding<'_>> + '_ {
    agg.offenders
        .iter()
        .enumerate()
        .filter(move |(m, _)| rungs.contains(&METRICS[*m].rung))
        .flat_map(|(m, os)| os.iter().map(move |o| (m, o)))
        .filter(|(_, o)| o.severity().is_some())
}

/// What a rung's findings add up to. EVERY violation at the rung, not
/// only the ones the ranked list can show: ten metrics gate on a budget
/// of zero — `secrets`, `built query`, `shelled out` among them — and
/// counting only the rankable ones reported 8,866 gates on a tree that
/// fails CI on 12,597.
fn count_rung(agg: &Agg, rungs: std::ops::RangeInclusive<u8>) -> u64 {
    METRICS
        .iter()
        .enumerate()
        .filter(|(_, def)| rungs.contains(&def.rung))
        .map(|(m, _)| agg.violations_n[m])
        .sum()
}

fn section(ink: ink::Ink, name: &str, gloss: &str, out: &mut String) {
    let _ = writeln!(
        out,
        "\n{}{name}{} {}—{} {gloss}",
        ink.bold(),
        ink.off(),
        ink.faint(),
        ink.off()
    );
}

fn cta(ink: ink::Ink, label: &str, command: &str, out: &mut String) {
    let _ = writeln!(
        out,
        "  {}→ {label}{}  {}{command}{}",
        ink.faint(),
        ink.off(),
        ink.command(),
        ink.off()
    );
}

/// Rungs that fail a build, and rungs that only ask for a look. Named
/// because they are the report's two spines and appear in five places.
const GATES: std::ops::RangeInclusive<u8> = 0..=2;
const SUSPICIONS: std::ops::RangeInclusive<u8> = 3..=4;

fn render_ranked(agg: &mut Agg, page: &Page, out: &mut String) {
    let (ink, trim) = (page.ink, page.trim);
    let gates = count_rung(agg, GATES);
    if gates == 0 {
        section(ink, "gates", "clean — nothing here fails a build", out);
    } else {
        let gloss = format!(
            "{gates} over budget across {} bodies. Old sludge is tolerated until touched, new sludge is blocked.",
            bodies_at(agg, GATES)
        );
        section(ink, "gates", &gloss, out);
        for g in page.costed(ranked_units(agg, GATES, SHOW_RANKED)) {
            let _ = writeln!(
                out,
                "{}",
                carried_lines(&g, "gate", trim, (ink, ink.gate()))
            );
        }
        cta(ink, "see the rest, worst first", "elegance --full", out);
    }
    let susp = count_rung(agg, SUSPICIONS);
    if susp == 0 {
        return;
    }
    let gloss = format!(
        "{susp} over budget across {} bodies; is the design wrong, or does the metric not fit here?",
        bodies_at(agg, SUSPICIONS)
    );
    section(ink, "suspicions", &gloss, out);
    let listed = page.costed(ranked_units(agg, SUSPICIONS, SHOW_POLICY));
    // Point the command at something real. A reader who has to invent the
    // argument has been handed a manual page, not a next step.
    let example = listed
        .first()
        .map(|g| format!("{}:{}", &g.at.path[trim.min(g.at.path.len())..], g.at.line));
    for g in &listed {
        let _ = writeln!(
            out,
            "{}",
            carried_lines(g, "suspicion", trim, (ink, ink.suspicion()))
        );
    }
    if let Some(at) = example {
        let cmd = format!("elegance --explain {at}");
        cta(ink, "break one down line by line", &cmd, out);
    }
}

/// Metrics whose budget is zero. Their distance from it is a bare count,
/// not a multiple, and letting a count share a ranked list with real
/// multiples put `suppressions 121` above `cognitive 262>18`.
/// Findings a budget of zero leaves unrankable, per metric, worst first.
///
/// Classified per FINDING. A metric can be pinned in one language and
/// left at zero in another, as `demeter` is here, so judging the metric
/// as a whole strands 3,147 findings between the two lists.
fn zero_budget_rows(agg: &Agg) -> Vec<(usize, u64)> {
    let mut rows = Vec::new();
    for (m, def) in METRICS.iter().enumerate() {
        if !GATES.contains(&def.rung) && !SUSPICIONS.contains(&def.rung) {
            continue;
        }
        let n = unrankable(agg, m).count() as u64;
        if n > 0 {
            rows.push((m, n));
        }
    }
    // Most sites first, ties by name so the order never depends on the
    // registry's. A key beats a comparator here: the comparator nested
    // an index inside a field inside a compare inside two closures.
    rows.sort_by_key(|(m, n)| (std::cmp::Reverse(*n), METRICS[*m].name));
    rows
}

fn unrankable(agg: &Agg, m: usize) -> impl Iterator<Item = &Offender> {
    agg.offenders[m].iter().filter(|o| o.severity().is_none())
}

fn render_policy(agg: &mut Agg, page: &Page, out: &mut String) {
    let (ink, trim) = (page.ink, page.trim);
    let rows = zero_budget_rows(agg);
    if rows.is_empty() {
        return;
    }
    let total: u64 = rows.iter().map(|(_, n)| n).sum();
    // Ten of these sit at gating rungs — `secrets`, `built query` and
    // `shelled out` among them — so a section that only said "any
    // occurrence is a finding" would file a credential leak next to a
    // style note and let the reader guess which stops a release.
    let gates_too = |(m, _): &&(usize, u64)| GATES.contains(&METRICS[*m].rung);
    let gating: u64 = rows.iter().filter(gates_too).map(|(_, n)| n).sum();
    let gloss = match gating {
        0 => format!("{total} where the budget is zero, so any occurrence is a finding"),
        n => format!(
            "{total} where the budget is zero, so any occurrence is a finding; {n} of them gate"
        ),
    };
    section(ink, "policy", &gloss, out);
    for (m, n) in rows.iter().take(SHOW_POLICY) {
        let files: std::collections::HashSet<&str> =
            unrankable(agg, *m).map(|o| o.path.as_str()).collect();
        // A binary metric's worst is always 1, and printing it says nothing.
        let worst = unrankable(agg, *m)
            .max_by(|a, b| a.value.total_cmp(&b.value))
            .filter(|w| w.value > 1.0);
        let tail = worst.map_or(String::new(), |w| {
            let at = &w.path[trim.min(w.path.len())..];
            let value = fmt(&METRICS[*m], w.value);
            format!(
                ", worst {}{at}:{}{} ({value})",
                ink.faint(),
                w.line,
                ink.off()
            )
        });
        let name = METRICS[*m].name;
        let _ = writeln!(
            out,
            "  {name:<NAME_COL$}{n:>5} sites in {} files{tail}",
            files.len()
        );
    }
    cta(
        ink,
        "disagree with one?",
        "set it in .elegance.toml [budgets]",
        out,
    );
}

/// Only budgets actually pinned to the corpus. Sorted by violation rate
/// the head of this table is otherwise dominated by compiled-in defaults,
/// and a section claiming to compare against admired code must not.
fn render_shape(agg: &mut Agg, ink: ink::Ink, out: &mut String) {
    // A budget is per language, so a mixed scan has several and
    // `budget_label` names them all — too many for a column that also
    // carries three quantiles. This section speaks for the language that
    // dominates the tree and says which, so one band can stand per row.
    let Some(lang) = LANGS
        .iter()
        .copied()
        .filter(|l| agg.files_by_lang[*l as usize] > 0)
        .max_by_key(|l| agg.files_by_lang[*l as usize])
    else {
        return;
    };
    let budgets = *agg.budgets.root().for_lang(lang);
    let mut rows: Vec<(usize, f64)> = METRICS
        .iter()
        .enumerate()
        .filter(|(m, _)| {
            !agg.dists[*m].is_empty() && agg.violations_n[*m] > 0 && metrics::is_pinned(lang, *m)
        })
        .map(|(m, _)| {
            (
                m,
                100.0 * agg.violations_n[m] as f64 / agg.dists[m].len() as f64,
            )
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    rows.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(METRICS[a.0].name.cmp(METRICS[b.0].name))
    });
    section(
        ink,
        "shape",
        &format!(
            "where this sits against gold {}; budgets pinned to it, others left out",
            lang.name()
        ),
        out,
    );
    let _ = writeln!(
        out,
        "  {}{:<NAME_COL$}{:>8}  {:<9}{:>7}{:>7}{:>8}{}",
        ink.faint(),
        "metric",
        "outside",
        "budget",
        "p50",
        "p99",
        "max",
        ink.off()
    );
    for (m, rate) in rows.iter().take(SHOW_SHAPE) {
        let def = &METRICS[*m];
        let budget = metrics::band_label(*m, budgets.0[*m]);
        let dist = agg.sorted_dist(*m);
        let (p50, p99, max) = (
            fmt(def, quantile(dist, P50)),
            fmt(def, quantile(dist, P99)),
            fmt(def, *dist.last().expect("non-empty")),
        );
        let _ = writeln!(
            out,
            "  {:<NAME_COL$}{rate:>7.1}%  {budget:<9}{p50:>7}{p99:>7}{max:>8}",
            def.name
        );
    }
    let quiet = METRICS
        .iter()
        .enumerate()
        .filter(|(m, _)| !agg.dists[*m].is_empty() && agg.violations_n[*m] == 0)
        .count();
    let dup = duplicated_pct(agg);
    let _ = writeln!(
        out,
        "  {}{quiet} metrics never fired · {dup:.1}% of the code is duplicated{}",
        ink.faint(),
        ink.off()
    );
    cta(
        ink,
        "the same numbers as a style guide",
        "elegance --context",
        out,
    );
}

fn render_aligned(agg: &mut Agg, page: &Page, out: &mut String) {
    let ink = page.ink;
    let n = aligned_tensions(agg, &page.costs).len();
    if n == 0 {
        return;
    }
    section(
        ink,
        "tensions",
        &format!("{n} units where three independent facts land at once"),
        out,
    );
    cta(ink, "list them", "elegance --full", out);
}

fn render_where(agg: &mut Agg, ink: ink::Ink, out: &mut String) {
    let worst = crate::rollup::worst_dirs(agg, 3);
    if worst.is_empty() {
        return;
    }
    section(
        ink,
        "hotspots",
        "gated findings per file, worst directory first",
        out,
    );
    let shown: Vec<String> = worst
        .iter()
        .map(|(dir, per_file)| format!("{dir} {per_file:.1}"))
        .collect();
    let _ = writeln!(out, "  {}", shown.join(" · "));
    cta(ink, "full rollup", "elegance --by", out);
}

pub fn render_full(agg: &mut Agg, top: usize) -> String {
    let mut out = headline(agg);
    render_distributions(agg, &mut out);
    render_verdict(agg, &mut out);
    render_rates(agg, &mut out);
    render_architecture(agg, &mut out);
    let costs = Costs::read(agg);
    render_tensions(agg, &costs, &mut out);
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
/// What this run measured, and everything it declined to measure —
/// the one line a reader sees before any finding.
fn headline(agg: &Agg) -> String {
    let langs: Vec<String> = LANGS
        .iter()
        .filter(|l| agg.files_by_lang[**l as usize] > 0)
        .map(|l| format!("{} {}", l.name(), agg.files_by_lang[*l as usize]))
        .collect();
    let test_share = match agg.units > 0 {
        true => format!(
            " ({:.0}% test)",
            100.0 * agg.test_units as f64 / agg.units as f64
        ),
        false => String::new(),
    };
    format!(
        "elegance — {} files ({}), {} units{}, {} lines{}{}{}{}{}\n\n",
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
    )
}

/// Facts must align at least this many ways before the alignment is
/// worth a reader's attention. Two is a coincidence in any codebase of
/// size; three is a place to look.
const MIN_TENSIONS: usize = 3;

/// A file this many others import is expensive to change.
const LOAD_BEARING: u32 = 5;

/// Rung 7: facts that are worse together than apart.
///
/// Every other rung measures ONE property and reports it. A tension is
/// a CO-OCCURRENCE — a unit over budget, that no test mentions, in a
/// file half the codebase imports. Each is already reported at its own
/// rung, where each is survivable. Arriving together they are not: the
/// thing hardest to change safely is also the thing nobody is watching.
///
/// Deliberately NOT a score. A composite number would be the
/// risk_score this project refused: weights nobody can defend,
/// normalisation that makes repositories incomparable, and a figure
/// that launders a rung-5 description into the same currency as a
/// rung-0 gate. A tension names every fact it is made of.
/// Units where several independent facts land at once, worst first. The
/// short report prints only how many there are; the long one prints them.
fn aligned_tensions(agg: &mut Agg, costs: &Costs) -> Vec<(String, u32, String, Vec<String>)> {
    let (untested, heavy) = (&costs.untested, &costs.heavy);
    let duplicated: std::collections::HashSet<String> = recurring(agg)
        .iter()
        .flat_map(|c| c.sites.iter().map(|s| s.path.to_string()))
        .collect();
    let mut gated: HashMap<(String, u32, String), Vec<&'static str>> = HashMap::new();
    for v in agg.violations() {
        if METRICS[v.metric].rung > 2 {
            continue;
        }
        gated
            .entry((v.path.to_string(), v.line, v.unit.to_string()))
            .or_default()
            .push(METRICS[v.metric].name);
    }
    let mut aligned: Vec<(String, u32, String, Vec<String>)> = gated
        .into_iter()
        .filter_map(|((path, line, unit), mut metrics)| {
            metrics.sort_unstable();
            // ONE fact, however many metrics state it: cognitive,
            // cyclomatic, length and live span move together, and
            // counting each would let a single big function reach the
            // threshold by itself, which is the opposite of a tension.
            let mut facts = vec![format!("over budget on {}", metrics.join(", "))];
            if untested.contains(&format!("{path}:{line}  {unit}")) {
                facts.push("no test mentions it".to_string());
            }
            if let Some(n) = heavy.get(&path) {
                facts.push(format!("{n} files import this one"));
            }
            if duplicated.contains(&*path) {
                facts.push("its file holds duplicated logic".to_string());
            }
            (facts.len() >= MIN_TENSIONS).then_some((path, line, unit, facts))
        })
        .collect();
    aligned.sort_by(|a, b| {
        b.3.len()
            .cmp(&a.3.len())
            .then_with(|| (&a.0, a.1).cmp(&(&b.0, b.1)))
    });
    aligned
}

fn render_tensions(agg: &mut Agg, costs: &Costs, out: &mut String) {
    let aligned = aligned_tensions(agg, costs);
    if aligned.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\ntensions — {} places where independent facts align:",
        aligned.len()
    );
    for (path, line, unit, facts) in aligned.iter().take(SHOW_TENSIONS) {
        let _ = writeln!(out, "  {path}:{line}  {unit}");
        for fact in facts {
            let _ = writeln!(out, "      - {fact}");
        }
    }
    if aligned.len() > SHOW_TENSIONS {
        let _ = writeln!(out, "  ... and {} more", aligned.len() - SHOW_TENSIONS);
    }
    let _ = writeln!(
        out,
        "  Each fact is survivable alone and reported at its own rung.\n\
         \x20 No score, deliberately: one number would hide which of them\n\
         \x20 is true."
    );
}

/// Tensions listed before the list stops being read.
const SHOW_TENSIONS: usize = 10;

/// Files enough of the codebase imports that changing them is costly.
fn load_bearing_files(agg: &Agg) -> HashMap<String, u32> {
    let Some(arch) = crate::graph::analyze(agg.graph.read(), agg.mentions.read()) else {
        return HashMap::new();
    };
    arch.load_bearing
        .iter()
        .filter(|(_, n)| *n >= LOAD_BEARING)
        .map(|(path, n)| (path.clone(), *n))
        .collect()
}

fn render_architecture(agg: &mut Agg, out: &mut String) {
    crate::layers::render(&agg.breaches, agg.declared_layers, out);
    if agg.graph.read().iter().all(|g| g.imports.is_empty()) {
        return;
    }
    // Path order makes ambiguous resolutions and listings deterministic.
    agg.graph.read_mut().sort_by(|a, b| a.path.cmp(&b.path));
    let exports: usize = agg.graph.read().iter().map(|g| g.exports.len()).sum();
    let Some(arch) = crate::graph::analyze(agg.graph.read(), agg.mentions.read()) else {
        let r = crate::graph::resolve(agg.graph.read());
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
    render_orphans(&arch, out);
    render_interfaces(&arch.interfaces, out);
}

/// Modules nothing depends on — over the population that could have
/// had a dependent, and saying how many could not.
fn render_orphans(arch: &crate::graph::metrics::Architecture, out: &mut String) {
    if arch.judged_modules < arch.modules {
        let _ = writeln!(
            out,
            "  {} translation units are not judged here: nothing #includes a source file",
            arch.modules - arch.judged_modules,
        );
    }
    if arch.orphan_count == 0 {
        return;
    }
    let more = arch.orphan_count as usize - arch.orphans.len();
    let tail = match more {
        0 => String::new(),
        n => format!(" ... and {n} more"),
    };
    let _ = writeln!(
        out,
        "  orphans ({} of {}): {}{tail}",
        arch.orphan_count,
        arch.judged_modules,
        arch.orphans.join(", "),
    );
}

/// The tail-summarized distribution of every metric this run measured.
/// Tails, never means: a codebase is as bad as the code you are forced
/// to read most often.
fn render_distributions(agg: &mut Agg, out: &mut String) {
    // Budget LAST, and unpadded. A mixed tree names one band per
    // language, and a column wide enough for `0%-47% py · 0%-64% cpp ·
    // 0%-63% sh` pushed every row of `<=0.` thirty characters wide to
    // pad it. Nothing follows it, so nothing has to.
    let budgets: Vec<(usize, String)> = METRICS
        .iter()
        .enumerate()
        .filter(|(m, _)| !agg.dists[*m].is_empty())
        .map(|(m, _)| (m, agg.budget_label(m)))
        .collect();
    let _ = writeln!(
        out,
        "{:<15} {:>2} {:>7} {:>7} {:>7} {:>7} {:>8}   budget",
        "metric", "r", "p50", "p90", "p99", "max", "violate"
    );
    for (m, budget) in budgets {
        let def = &METRICS[m];
        let violate = 100.0 * agg.violations_n[m] as f64 / agg.dists[m].len() as f64;
        let dist = agg.sorted_dist(m);
        let _ = writeln!(
            out,
            "{:<15} {:>2} {:>7} {:>7} {:>7} {:>7} {:>7.1}%   {}",
            def.name,
            def.rung,
            fmt(def, quantile(dist, P50)),
            fmt(def, quantile(dist, P90)),
            fmt(def, quantile(dist, P99)),
            fmt(def, *dist.last().expect("non-empty")),
            violate,
            budget,
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
    let _ = writeln!(
        out,
        "{:<15} `<=76 py · <=115 cpp` is one band per language: gold pinned them apart",
        ""
    );
}

/// Share of the codebase's mass that is duplicated logic.
pub fn duplicated_pct(agg: &mut Agg) -> f64 {
    select_clones(agg).1.all_pct
}

/// How many metrics the brief report names. Enough to show what kind of
/// trouble a codebase is in, few enough to stay quotable.
const BRIEF_METRICS: usize = 5;

/// The report in a dozen lines. The full render answers "what should I
/// fix"; this answers "is anything wrong here, and of what kind" — the
/// question asked by someone who has not decided to look yet. Every
/// number is one the long form also prints, so the two cannot disagree.
pub fn render_brief(agg: &mut Agg) -> String {
    let mut out = headline(agg);
    while out.ends_with("\n\n") {
        out.pop();
    }
    render_verdict(agg, &mut out);

    // Ranked by how often a metric is outside budget rather than by raw
    // count, because a codebase with a million lines fails everything a
    // few times; the rate is what says which failure is characteristic.
    // The budget is printed because the direction is not always down —
    // `test asserts >=1` is violated by having too FEW, and a header
    // saying "over budget" would have been a lie about that row.
    let mut rows: Vec<(f64, &'static str, u8, String, String, String)> = Vec::new();
    for (m, def) in METRICS.iter().enumerate() {
        if def.rung > 4 || agg.dists[m].is_empty() || agg.violations_n[m] == 0 {
            continue;
        }
        let rate = 100.0 * agg.violations_n[m] as f64 / agg.dists[m].len() as f64;
        let budget = agg.budget_label(m);
        let dist = agg.sorted_dist(m);
        let p99 = fmt(def, quantile(dist, P99));
        let max = fmt(def, *dist.last().expect("non-empty"));
        rows.push((rate, def.name, def.rung, budget, p99, max));
    }
    rows.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(b.1)));
    if !rows.is_empty() {
        let _ = writeln!(out, "\nmost often outside budget");
        for (rate, name, rung, budget, p99, max) in rows.iter().take(BRIEF_METRICS) {
            let _ = writeln!(
                out,
                "  {name:<16} {rate:>5.1}%   r{rung}  p99 {p99:<7} max {max:<7} {budget}"
            );
        }
    }

    let (classes, dup) = select_clones(agg);
    if !classes.is_empty() {
        let _ = writeln!(
            out,
            "\nclones                 {:.1}% of code duplicated across {} classes",
            dup.all_pct,
            classes.len()
        );
    }
    render_rates(agg, &mut out);
    out
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
    for (object, verbs) in agg.synonyms.read() {
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
        .read()
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
    recurring_sets(agg.shapes.read())
}

/// Repeated dispatch: label-sets recurring >=3 times, strongest first.
pub(super) fn select_switches(agg: &Agg) -> Vec<SelectedClump> {
    recurring_sets(agg.switches.read())
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
    let found = crate::near::pairs(agg.prints.read(), top);
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
        .read()
        .iter()
        .filter(|c| !agg.test_refs.read().contains(&c.name))
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
        .read()
        .values()
        .filter(|c| c.len() >= 2)
        .map(|c| {
            let mut sites: Vec<CloneLoc> = c.sites().cloned().collect();
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
        .read()
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
    explain_comments(facts, &mut out);
    if out.is_empty() {
        out.push_str("no unit at that location\n");
    }
    out
}

/// What this file's comments are for, and the longest thing they say.
///
/// The counts are the role classification made checkable by hand — the
/// only way to know a comment was read as a field's doc rather than a
/// function's is to look — and the longest run is where a per-role
/// documentation budget will be argued from.
fn explain_comments(facts: &FileFacts, out: &mut String) {
    let Some(widest) = facts.comments.iter().max_by_key(|c| c.prose.words) else {
        return;
    };
    let counts: Vec<String> = CommentRole::ALL
        .iter()
        .filter_map(
            |role| match facts.comments.iter().filter(|c| c.role == *role).count() {
                0 => None,
                n => Some(format!("{} {n}", role.name())),
            },
        )
        .collect();
    let _ = writeln!(out, "comments: {}", counts.join(", "));
    let on = widest
        .unit
        .and_then(|u| facts.units.get(u as usize))
        .map_or(String::new(), |u| format!(" on {}", u.qualname));
    let _ = writeln!(
        out,
        "longest comment: {} at line {}{on} — {} words, {} sentences",
        widest.role.name(),
        widest.line,
        widest.prose.words,
        widest.prose.sentences,
    );
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

    /// The per-language denominators are only worth having if they add
    /// up to the pooled totals this report has always printed — a
    /// language row that silently lost a unit would produce exactly the
    /// kind of inflated rate it exists to prevent. Two languages and a
    /// merge, because the rows are accumulated per rayon worker and
    /// folded together afterwards.
    #[test]
    fn language_rows_reconcile_with_the_pooled_totals() {
        let rust = {
            let pack = Lang::Rust.pack();
            let mut parser = pack.make_parser();
            let src = "pub fn total(xs: &[i32]) -> i32 {\n    let mut t = 0;\n    for x in xs {\n        if *x > 0 {\n            t += *x;\n        }\n    }\n    t\n}\n";
            extract(pack, &mut parser, Path::new("b.rs"), src)
        };
        let mut left = Agg::new();
        left.add_file(&facts("a.py", &TWIN.replace("NAME", "f0")));
        let mut right = Agg::new();
        right.add_file(&rust);
        let agg = Agg::merge(left, right);

        assert_eq!(agg.units_by_lang.iter().sum::<u64>(), agg.units);
        assert!(agg.units_by_lang[Lang::Python as usize] > 0);
        assert!(agg.units_by_lang[Lang::Rust as usize] > 0);
        for m in 0..N {
            let cell = |l: &Lang| agg.metric_by_lang[*l as usize][m];
            let measured: u64 = LANGS.iter().map(|l| cell(l).measured).sum();
            let violated: u64 = LANGS.iter().map(|l| cell(l).violated).sum();
            let name = metrics::METRICS[m].name;
            assert_eq!(measured as usize, agg.dists[m].len(), "{name} measured");
            assert_eq!(violated, agg.violations_n[m], "{name} violated");
        }
    }

    #[test]
    fn every_mode_asks_for_what_it_reads() {
        // A `Wants` set is declared by hand, and a mode that forgets one
        // renders that section BLANK on a green suite — the same silent
        // failure that let nine detector families die unnoticed. So each
        // mode is run here against its own declared set, over a tree
        // holding every shape the accumulators feed on: a clone class, a
        // near-clone, an import, a parameter clump, a repeated dispatch,
        // an anonymous record and an over-budget untested unit.
        //
        // `Gated::read` panics in a debug build when its mode did not
        // ask, so a missing want fails HERE rather than shipping silence.
        use crate::config::Layers;
        let dup = "def handler(alpha, beta, gamma, delta):\n                       cfg = {'host': 1, 'port': 2, 'name': 3}\n                       if alpha == 'a':\n        return 1\n                       elif alpha == 'b':\n        return 2\n                       elif alpha == 'c':\n        return 3\n                       for i in range(9):\n                           for j in range(9):\n                               for k in range(9):\n                                   beta += i * j * k\n    return beta\n";
        let files = [
            ("a.py", format!("import os\n{dup}")),
            ("b.py", format!("import os\n{dup}")),
        ];
        let build = |wants: Wants| {
            let mut agg = Agg::configured(Layers::flat(LangBudgets::defaults()), true, wants);
            for (name, src) in &files {
                let pack = crate::lang::Lang::Python.pack();
                let mut parser = pack.make_parser();
                let f = crate::facts::extract(pack, &mut parser, Path::new(name), src);
                agg.add_file(&f);
            }
            agg
        };
        // Each entry is a mode and the set `wants_for` grants it. Both
        // halves have to agree; that agreement is the whole test.
        /// A mode: what it is called, what it declares, how it renders.
        type Mode = (&'static str, Wants, fn(&mut Agg) -> String);
        let modes: Vec<Mode> = vec![
            ("default", Wants::ALL, |a| render(a, ink::Ink::none())),
            ("--full", Wants::ALL, |a| render_full(a, 10)),
            (
                "--brief",
                Wants {
                    clones: true,
                    graph: true,
                    ..Wants::NONE
                },
                render_brief,
            ),
            ("--json", Wants::ALL, crate::report::json::render_json),
            (
                "--sarif",
                Wants {
                    clones: true,
                    prints: true,
                    graph: true,
                    clumps: true,
                    sets: true,
                    ..Wants::NONE
                },
                crate::report::sarif::render,
            ),
            (
                "--by",
                Wants {
                    graph: true,
                    ..Wants::NONE
                },
                |a| crate::rollup::run(a, 10),
            ),
        ];
        for (name, wants, render_it) in modes {
            let mut agg = build(wants);
            if wants.graph {
                agg.graph.read_mut().sort_by(|x, y| x.path.cmp(&y.path));
            }
            let out = render_it(&mut agg);
            assert!(!out.is_empty(), "{name} rendered nothing");
        }
    }

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

        let fwd_out = render_full(&mut fwd, 10);
        assert_eq!(fwd_out, render_full(&mut rev, 10));
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
    fn correlated_metrics_are_one_fact_not_four() {
        // A function big enough to trip cognitive, cyclomatic, length
        // and live span at once trips them BECAUSE it is big: they are
        // one fact wearing four names. Counting each would let a
        // single unit reach the threshold alone, which is the opposite
        // of a tension. Tested and unimported, it says nothing.
        let mut big = String::from("def sprawl(a):\n");
        for i in 1..40 {
            let _ = writeln!(big, "    if a == {i} and a != {i}:\n        return {i}");
        }
        let mut agg = Agg::new();
        agg.add_file(&facts("prod.py", &big));
        agg.add_file(&facts(
            "test_prod.py",
            "def test_sprawl_works():\n    assert sprawl(1) == 1\n",
        ));
        let gates = agg
            .violations()
            .filter(|v| METRICS[v.metric].rung <= 2)
            .count();
        assert!(gates >= 3, "the fixture must trip several gates: {gates}");
        let mut out = String::new();
        let costs = Costs::read(&agg);
        render_tensions(&mut agg, &costs, &mut out);
        assert!(
            out.is_empty(),
            "one big TESTED function is one fact, not a tension:\n{out}"
        );
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
        let out = render_full(&mut agg, 5);
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
        let out = render_full(&mut agg, 3);
        assert!(out.contains("gates (rungs 0-2)"), "verdict block rendered");

        let mut clean = Agg::new();
        clean.add_file(&facts("b.py", "def f(x):\n    return x + 1\n"));
        assert!(render_full(&mut clean, 3).contains("gates (rungs 0-2)      clean"));
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
        let out = render_full(&mut agg, 5);
        assert!(out.contains("1 low-confidence excluded"));
    }

    /// The summary is the default view, so it has to be readable when
    /// nobody is watching a terminal.
    #[test]
    fn the_summary_carries_no_escapes_when_styling_is_off() {
        let mut agg = Agg::complete();
        agg.add_file(&facts("a.py", &TWIN.replace("NAME", "mess")));
        let out = render(&mut agg, ink::Ink::none());
        assert!(
            !out.contains('\u{1b}'),
            "escape leaked into plain output:\n{out}"
        );
        assert!(out.contains("gates —"), "{out}");
    }

    /// The whole point of the ranking: a unit far outside its budget must
    /// outrank one barely past it, even though the second carries the far
    /// larger raw number. Sorting on the value alone gets this backwards,
    /// because 400 lines and 8 levels of nesting are not comparable until
    /// each is divided by what it was allowed.
    #[test]
    fn distance_from_budget_ranks_above_raw_value() {
        let mut agg = Agg::complete();
        // Nesting far past its budget, against a function whose length is
        // an order of magnitude bigger a number and barely past its own.
        let deep = "def deep(x):\n".to_string()
            + &(1..=9)
                .map(|i| format!("{}if x > {i}:\n", "    ".repeat(i)))
                .collect::<String>()
            + &format!("{}return x\n", "    ".repeat(10));
        agg.add_file(&facts("deep.py", &deep));
        agg.add_file(&facts(
            "wide.py",
            &format!("def wide(x):\n{}    return x\n", "    x += 1\n".repeat(82)),
        ));
        let out = render(&mut agg, ink::Ink::none());
        let at = |needle: &str| out.find(needle);
        let (deep_at, wide_at) = (at("deep.py").expect("deep listed"), at("wide.py"));
        assert!(
            wide_at.is_none_or(|w| deep_at < w),
            "the deeper overage must lead, whatever the raw values:\n{out}"
        );
    }

    /// A band is violated from one side at a time, and the report has to
    /// name that side. Formatting it by editing the band's label produced
    /// `0%2%-60%`, and scoring it against the ceiling it never approached
    /// produced a severity of zero.
    #[test]
    fn a_band_violated_from_below_reads_from_below() {
        let o = Offender {
            value: 0.0,
            path: "a.py".to_string(),
            line: 1,
            name: String::new(),
            band: (Some(0.02), Some(0.60)),
            local: Local::default(),
        };
        let m = METRICS
            .iter()
            .position(|d| d.name == "comment ratio")
            .expect("metric exists");
        assert_eq!(o.over(m), "0%<2%");
        assert!(o.severity().is_some_and(|s| s > 1.0), "{:?}", o.severity());
    }

    /// A finding against a ceiling, for exercising the ranked list
    /// without depending on which budget a language happens to carry.
    fn over((path, line, name): Body, value: f32, hi: f32) -> Offender {
        Offender {
            value,
            path: path.to_string(),
            line,
            name: name.to_string(),
            band: (None, Some(hi)),
            local: Local::default(),
        }
    }

    fn metric(name: &str) -> usize {
        METRICS
            .iter()
            .position(|d| d.name == name)
            .unwrap_or_else(|| panic!("no metric named {name}"))
    }

    /// The one finding a metric produced for a named body.
    fn finding<'a>(agg: &'a Agg, m: usize, name: &str) -> &'a Offender {
        agg.offenders[m]
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("no {} finding on {name}", METRICS[m].name))
    }

    /// A finding states a global rule; the reader is standing in one
    /// file. What the REST of that file did with the same budget is read
    /// off the measurements the file already produced — the alternative
    /// is parsing every file twice.
    ///
    /// `params` because its default ceiling is 5, so a signature says
    /// exactly what a body will measure without depending on which
    /// budget the gold calibration happens to pin for Python.
    #[test]
    fn a_finding_carries_what_the_rest_of_its_file_did() {
        let m = metric("params");
        let mut agg = Agg::new();
        agg.add_file(&facts(
            "kin.py",
            "def load_all(a, b, c, d, e, f, g):\n    return 1\n\n\
             def load_one(a, b, c):\n    return 2\n\n\
             def tiny(a):\n    return 3\n",
        ));
        agg.add_file(&facts(
            "trivial.py",
            "def emit_all(a, b, c, d, e, f, g):\n    return 1\n\n\
             def emit_one(a):\n    return 2\n",
        ));
        agg.add_file(&facts(
            "crowded.py",
            "def walk_all(a, b, c, d, e, f, g):\n    return 1\n\n\
             def scan_all(a, b, c, d, e, f, g):\n    return 2\n",
        ));

        let alone = &finding(&agg, m, "load_all").local;
        assert_eq!(alone.over, 0, "nothing else in kin.py is over");
        assert_eq!(alone.clean, 3.0, "the largest peer that stayed inside");
        let (peer, at) = alone.kin.as_ref().expect("load_one claims the same job");
        assert_eq!((&**peer, *at), ("load_one", 3.0));

        // A peer must be doing comparable work to be worth naming: the
        // largest CLEAN value in a file is otherwise routinely a
        // one-argument shim, and "emit_one does the same at 1" is not
        // advice about a seven-argument entry point.
        let trivial = &finding(&agg, m, "emit_all").local;
        assert!(
            trivial.kin.is_none(),
            "a peer under half the budget does not claim the same job: {:?}",
            trivial.kin
        );

        let crowded = &finding(&agg, m, "walk_all").local;
        assert_eq!(crowded.over, 1, "scan_all is over the same budget");
        assert!(
            crowded.kin.is_none(),
            "a peer that is itself over budget is no example"
        );
    }

    /// One clause per entry, chosen most useful first. All three answer
    /// the same question — what does this file already do — so a line
    /// each would put the entry back where the grouping found it.
    #[test]
    fn the_file_says_the_most_useful_thing_it_knows_and_only_that() {
        let m = metric("params");
        let mut at = over(("a.py", 3, "load_all"), 7.0, 5.0);
        assert_eq!(
            neighbours(m, &at),
            None,
            "a lone body in a lone file has no neighbourhood to report"
        );

        at.local = Local {
            peers: 9,
            over: 3,
            clean: 4.0,
            kin: None,
        };
        let crowded = "params — 4 of 10 bodies here are over it";
        assert_eq!(neighbours(m, &at).as_deref(), Some(crowded));

        at.local.over = 0;
        let alone = "params — alone here, the other 9 peak at 4";
        assert_eq!(neighbours(m, &at).as_deref(), Some(alone));

        at.local.over = 3;
        at.local.kin = Some(("load_one".into(), 3.0));
        let named = "params — load_one does the same job at 3";
        assert_eq!(
            neighbours(m, &at).as_deref(),
            Some(named),
            "a clause that names a target outranks one that counts a crowd"
        );

        // Under a FLOOR the largest clean peer is the furthest from the
        // finding, so naming it answers nothing: "the other 9 peak at 8
        // asserts" is no help to a body that has none.
        let floor = Offender {
            value: 0.0,
            path: "a.py".to_string(),
            line: 3,
            name: "test_it".to_string(),
            band: (Some(1.0), None),
            local: Local {
                peers: 9,
                over: 0,
                clean: 8.0,
                kin: None,
            },
        };
        let inside = "test asserts — alone here, the other 9 stay inside";
        assert_eq!(
            neighbours(metric("test asserts"), &floor).as_deref(),
            Some(inside)
        );
    }

    /// The body-shape metrics state one fact between them, so a list
    /// capped at one row per metric spent its five rows naming five
    /// bodies for that one fact. Three claims, each of which the
    /// grouping has to keep true: a body appears ONCE however many
    /// metrics it trips, the entry names all of them, and the ranking
    /// is still by distance — a body 2x past on five budgets is not
    /// more urgent than one 13x past on four, which is what ranking by
    /// the count produced on the reference tree.
    #[test]
    fn one_body_is_one_entry_naming_every_budget_it_broke() {
        let mut agg = Agg::complete();
        for (name, value, hi) in [
            ("cognitive", 55.0, 18.0),
            ("cyclomatic", 30.0, 12.0),
            ("depth", 5.0, 3.0),
            ("length", 287.0, 76.0),
            ("live span", 257.0, 48.0),
        ] {
            agg.offenders[metric(name)].push(over(("wide.py", 399, "build_cell"), value, hi));
        }
        // A count against a budget of none: the policy section's, not
        // this list's, however many rankable findings share the body.
        agg.offenders[metric("abbreviated")].push(over(("wide.py", 399, "build_cell"), 4.0, 0.0));
        agg.offenders[metric("live span")].push(over(("deep.py", 43, "main"), 626.0, 48.0));

        let bodies = ranked_units(&agg, GATES, SHOW_RANKED);
        assert_eq!(bodies.len(), 2, "one entry per body");
        assert_eq!(bodies[0].at.name, "main", "13x outranks 5x on five budgets");
        assert_eq!(bodies[1].findings.len(), 5, "the sixth has no distance");

        let block = carried_lines(&bodies[1], "gate", 0, (ink::Ink::none(), ""));
        assert_eq!(block.matches("wide.py").count(), 1, "named once:\n{block}");
        assert!(block.contains("5 gates"), "how many budgets:\n{block}");
        assert!(!block.contains("abbreviated"), "policy count:\n{block}");
        // Named, or counted in the tail — nothing the entry stands for
        // may go unmentioned, whatever the width allows.
        let named = ["cognitive", "cyclomatic", "depth", "length", "live span"]
            .iter()
            .filter(|name| block.contains(*name))
            .count();
        assert!(named >= 4, "too few named:\n{block}");
        assert!(
            named == 5 || block.contains(&format!("+{} more", 5 - named)),
            "unnamed and uncounted:\n{block}"
        );
    }

    /// The report computed all three of these for rung 7 and printed
    /// the result as an integer, so `config.py` could carry the
    /// second-worst body in the tree while 17 of 39 modules imported it
    /// and neither fact ever reached the other. They also stack, and a
    /// clause per fact would put the entry back where the grouping
    /// found it — so the rarest wins, and a repeat too small to be a
    /// fact about the file says nothing at all.
    #[test]
    fn a_cost_clause_names_the_rarest_fact_that_is_true() {
        let mut agg = Agg::complete();
        agg.offenders[metric("cognitive")].push(over(("hot.py", 10, "f"), 55.0, 18.0));
        let bodies = ranked_units(&agg, GATES, SHOW_RANKED);
        // Each row drops the fact above it, so the clause has to fall
        // through to the next-rarest rather than go silent.
        let costs = |heavy: &[(&str, u32)], untested: &[&str], repeat: u32| Costs {
            heavy: heavy.iter().map(|(p, n)| (p.to_string(), *n)).collect(),
            untested: untested.iter().map(|u| u.to_string()).collect(),
            repeats: HashMap::from([((metric("cognitive"), "hot.py".to_string()), repeat)]),
        };
        let clause = |c: Costs| c.of(&bodies[0]).unwrap_or_else(|| "nothing".to_string());
        assert_eq!(
            clause(costs(&[("hot.py", 17)], &["hot.py:10  f"], 8)),
            "17 files import this one"
        );
        assert_eq!(clause(costs(&[], &["hot.py:10  f"], 8)), "no test names it");
        assert_eq!(
            clause(costs(&[], &[], 8)),
            "8 cognitive findings in this one file"
        );
        assert_eq!(
            clause(costs(&[], &[], REPEATED - 1)),
            "nothing",
            "a handful is not a house style"
        );
    }

    /// Six copies of one generated locale file each read `demeter
    /// 35>2`, and with the per-metric cap gone they filled the gold
    /// Lua corpus's top five with one claim stated five times — the
    /// thing the grouping was built to stop, arriving from the other
    /// direction. A body whose claim is already on the list is
    /// collapsed into it, and the entry says how many it stands for so
    /// nothing goes quiet. The numbers must match to the digit: three
    /// meridian bodies over the same three budgets at different values
    /// are three functions to open.
    #[test]
    fn bodies_making_the_identical_claim_are_one_entry_that_counts_them() {
        let mut agg = Agg::complete();
        for path in ["en-us.lua", "es-419.lua", "ja-jp.lua"] {
            agg.offenders[metric("demeter")].push(over((path, 1, ""), 35.0, 2.0));
        }
        agg.offenders[metric("demeter")].push(over(("zh-cn.lua", 1, ""), 34.0, 2.0));

        let bodies = ranked_units(&agg, GATES, SHOW_RANKED);
        assert_eq!(bodies.len(), 2, "one entry per distinct claim");
        assert_eq!(bodies[0].alike, 2, "and it stands for the copies");
        assert_eq!(bodies[1].alike, 0, "a different value is a different body");
        let costs = Costs {
            heavy: HashMap::new(),
            untested: std::collections::HashSet::new(),
            repeats: HashMap::new(),
        };
        assert_eq!(
            costs.of(&bodies[0]).as_deref(),
            Some("2 more bodies carry these same numbers")
        );
    }

    /// `varies` named the problem and withheld the answer, and it did
    /// so on exactly the rows a reader needs the number for: the
    /// metrics that fire most are the ones calibrated per language.
    /// Both expectations are pinned to `calibration.toml` on purpose —
    /// if a recalibration moves `depth` or `length` this test says so,
    /// the way the README's numbers are pinned.
    #[test]
    fn a_mixed_run_names_the_budget_of_every_language_it_judged_by() {
        // Calibrated budgets, not the compiled defaults: the point of
        // the label is that gold pinned these languages differently.
        let mut agg = Agg::configured(
            crate::config::Layers::flat(LangBudgets::calibrated()),
            true,
            Wants::ALL,
        );
        for (lang, files) in [
            (Lang::Python, 40),
            (Lang::Rust, 30),
            (Lang::TypeScript, 20),
            (Lang::Go, 10),
        ] {
            agg.files_by_lang[lang as usize] = files;
        }
        // Most files first, and languages sharing a band named together
        // — the label is a budget, not a list of languages.
        assert_eq!(agg.budget_label(metric("depth")), "<=3 py/rs · <=4 ts/go");
        // Four distinct bands is more than a column can hold, so the
        // tail is counted rather than dropped.
        assert_eq!(
            agg.budget_label(metric("length")),
            "<=76 py · <=93 rs · <=106 ts +1"
        );
    }

    /// A zero budget makes the "ratio" a bare count, so those findings are
    /// reported apart from the ranked ones. Mixing them let a count of 121
    /// outrank a genuine 14x overage.
    #[test]
    fn a_zero_budget_finding_never_enters_the_ranked_list() {
        let mut agg = Agg::complete();
        agg.add_file(&facts(
            "f.py",
            "def g(flag=True, other=False):\n    return flag\n",
        ));
        let out = render(&mut agg, ink::Ink::none());
        let ranked_end = out.find("policy —").unwrap_or(out.len());
        assert!(
            !out[..ranked_end].contains("flag params"),
            "a policy finding reached the ranked section:\n{out}"
        );
    }
}
