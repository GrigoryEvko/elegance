//! Metric registry and measurement: pure functions over facts. Budgets are
//! `[lo, hi]` bands — most metrics cap only the high side, comment ratio is
//! two-sided (too few comments is obscurity, too many is noise).

use crate::facts::{BodyShape, CtrlFact, FileFacts, UnitFacts};
use crate::sem::Sem;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    Int,
    Pct,
}

pub struct MetricDef {
    pub name: &'static str,
    /// Quality-ladder rung: 0 token hygiene, 1 expression shape, 2 function
    /// shape, 3 interface shape, 4 class/module cohesion, 5 dependency
    /// graph, 6 evolution/history, 7 tensions. Exactness falls and context
    /// grows as rungs rise, so the verdict type derives from the rung.
    pub rung: u8,
    pub lo: Option<f32>,
    pub hi: Option<f32>,
    pub fmt: Fmt,
    /// How gold-corpus calibration treats this metric.
    pub calib: Calib,
}

/// One-sided budgets pin to gold p99; bands to gold p05/p95; policy
/// metrics (flag params, asserts) encode taste, not statistics — no
/// percentile may legitimize them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Calib {
    P99,
    Band,
    Policy,
}

/// Effective `[lo, hi]` bands per metric. All budget checks go through
/// this — the static table only supplies defaults.
#[derive(Clone, Copy, PartialEq)]
pub struct Budgets(pub [(Option<f32>, Option<f32>); N]);

impl Budgets {
    pub fn defaults() -> Budgets {
        Budgets(std::array::from_fn(|i| (METRICS[i].lo, METRICS[i].hi)))
    }

    pub fn violates(&self, m: usize, v: f32) -> bool {
        let (lo, hi) = self.0[m];
        hi.is_some_and(|h| v > h) || lo.is_some_and(|l| v < l)
    }

    pub fn label(&self, m: usize) -> String {
        band_label(m, self.0[m])
    }
}

/// Human label for one metric's `[lo, hi]` band.
pub fn band_label(m: usize, (lo, hi): (Option<f32>, Option<f32>)) -> String {
    let f = |v: f32| match METRICS[m].fmt {
        Fmt::Int => format!("{v:.0}"),
        Fmt::Pct => format!("{:.0}%", v * 100.0),
    };
    match (lo, hi) {
        (None, Some(h)) => format!("<={}", f(h)),
        (Some(l), None) => format!(">={}", f(l)),
        (Some(l), Some(h)) => format!("{}-{}", f(l), f(h)),
        (None, None) => String::new(),
    }
}

/// Per-language budgets: defaults, refined by the compiled-in gold
/// calibration snapshot, then repo config overrides on top (uniformly
/// across languages). One number per metric across languages is provably
/// wrong — doc-comment culture alone shifts comment bands by 25 points.
#[derive(Clone, Copy)]
pub struct LangBudgets(pub [Budgets; crate::lang::LANGS.len()]);

impl LangBudgets {
    pub fn defaults() -> LangBudgets {
        LangBudgets([Budgets::defaults(); crate::lang::LANGS.len()])
    }

    pub fn for_lang(&self, lang: crate::lang::Lang) -> &Budgets {
        &self.0[lang as usize]
    }

    /// Calibration snapshot baked in at build time; regenerate with
    /// `elegance calibrate` and rebuild.
    pub fn calibrated() -> LangBudgets {
        use std::sync::OnceLock;
        static CAL: OnceLock<LangBudgets> = OnceLock::new();
        *CAL.get_or_init(|| {
            let mut lb = LangBudgets([Budgets::defaults(); crate::lang::LANGS.len()]);
            let text = include_str!("../../calibration.toml");
            let Ok(parsed) = toml::from_str::<CalibrationFile>(text) else {
                return lb;
            };
            for lang in crate::lang::LANGS {
                let Some(table) = parsed.0.get(lang.name()) else {
                    continue;
                };
                let budgets = &mut lb.0[lang as usize];
                for (name, o) in table {
                    if let Some(m) = METRICS.iter().position(|d| d.name == name) {
                        budgets.0[m] = (o.lo.or(budgets.0[m].0), o.hi.or(budgets.0[m].1));
                    }
                }
            }
            lb
        })
    }
}

#[derive(serde::Deserialize)]
struct CalibrationFile(
    std::collections::HashMap<String, std::collections::HashMap<String, CalibEntry>>,
);

#[derive(serde::Deserialize)]
struct CalibEntry {
    lo: Option<f32>,
    hi: Option<f32>,
    /// Gold coverage share for the rate metrics (asserts, public docs).
    rate: Option<f32>,
}

/// The metrics rendered as coverage RATES rather than per-unit
/// findings — each one a claim the gold corpus fails most of the time,
/// where a wall of findings would only teach readers to scroll past.
pub const RATE_METRICS: [usize; 2] = [ASSERTS, PUBLIC_DOCS];

/// Does this budget rest on the gold corpus, or on nothing but the
/// compiled-in default? Three things put a metric in the second
/// group, and a reader deserves to tell them apart: the corpus held
/// fewer than the 200 samples a percentile needs (Go declares 29
/// interfaces in all of gold), the metric is a POLICY that no
/// percentile may legitimize, or its gold p99 was zero and pinning it
/// would gate everything the day extraction improves.
pub fn is_pinned(lang: crate::lang::Lang, m: usize) -> bool {
    use std::sync::OnceLock;
    type Pinned = [[bool; N]; crate::lang::LANGS.len()];
    static PINNED: OnceLock<Pinned> = OnceLock::new();
    PINNED.get_or_init(|| {
        let mut out = [[false; N]; crate::lang::LANGS.len()];
        let text = include_str!("../../calibration.toml");
        let Ok(parsed) = toml::from_str::<CalibrationFile>(text) else {
            return out;
        };
        for lang in crate::lang::LANGS {
            let Some(table) = parsed.0.get(lang.name()) else {
                continue;
            };
            for (name, entry) in table {
                let slot = METRICS.iter().position(|d| d.name == name);
                // A `rate` entry records coverage, not a budget.
                if let Some(m) = slot.filter(|_| entry.lo.is_some() || entry.hi.is_some()) {
                    out[lang as usize][m] = true;
                }
            }
        }
        out
    })[lang as usize][m]
}

/// Gold's own coverage rate for a rate metric, baked from calibration.
pub fn gold_rate(lang: crate::lang::Lang, m: usize) -> Option<f32> {
    use std::sync::OnceLock;
    type Rates = [[Option<f32>; N]; crate::lang::LANGS.len()];
    static RATES: OnceLock<Rates> = OnceLock::new();
    RATES.get_or_init(|| {
        let mut out = [[None; N]; crate::lang::LANGS.len()];
        let text = include_str!("../../calibration.toml");
        let Ok(parsed) = toml::from_str::<CalibrationFile>(text) else {
            return out;
        };
        for lang in crate::lang::LANGS {
            let Some(table) = parsed.0.get(lang.name()) else {
                continue;
            };
            for (name, o) in table {
                let slot = METRICS.iter().position(|d| d.name == name);
                if let (Some(m), Some(r)) = (slot, o.rate) {
                    out[lang as usize][m] = Some(r);
                }
            }
        }
        out
    })[lang as usize][m]
}

pub const COGNITIVE: usize = 0;
pub const CYCLOMATIC: usize = 1;
pub const DEPTH: usize = 2;
pub const LENGTH: usize = 3;
pub const PARAMS: usize = 4;
pub const FLAG_PARAMS: usize = 5;
pub const EXPR_DEPTH: usize = 6;
pub const DEMETER: usize = 7;
pub const NEGATIONS: usize = 8;
pub const LIVE_SPAN: usize = 9;
pub const MAGIC_NUMBERS: usize = 10;
pub const SPOOKY: usize = 11;
pub const PASSTHROUGH: usize = 12;
pub const KW_OPACITY: usize = 13;
pub const AND_NAME: usize = 14;
pub const GENERIC_NAME: usize = 15;
pub const FEATURE_ENVY: usize = 16;
pub const SWALLOWED: usize = 17;
pub const BROAD_CATCH: usize = 18;
pub const UNWRAPS: usize = 19;
pub const LYING_NAME: usize = 20;
pub const TEST_ASSERTS: usize = 21;
pub const LAZY_TEST_NAME: usize = 22;
pub const ASSERTS: usize = 23;
pub const PUBLIC_DOCS: usize = 24;
pub const ECHO_COMMENTS: usize = 25;
pub const COMMENT_RATIO: usize = 26;
pub const UNTYPED_PARAMS: usize = 27;
pub const LOOSE_TYPES: usize = 28;
pub const CASTS: usize = 29;
pub const SUPPRESSIONS: usize = 30;
pub const CONFUSABLE: usize = 31;
pub const TERSE_NAME: usize = 32;
pub const ABBREVIATED: usize = 33;
pub const VACUOUS_ASSERTS: usize = 34;
pub const LOST_CONTEXT: usize = 35;
pub const SECRETS: usize = 36;
pub const BLOCKING_IN_ASYNC: usize = 37;
pub const UNMANAGED: usize = 38;
pub const DROPPED_TASKS: usize = 39;
pub const WILDCARD_MATCH: usize = 40;
pub const STRINGLY_ID: usize = 41;
pub const LOOP_DEPTH: usize = 42;
pub const ALLOC_IN_LOOP: usize = 43;
pub const CONDITIONAL_HOOK: usize = 44;
pub const RETURN_ARITY: usize = 45;
pub const INTERFACE_WIDTH: usize = 46;
pub const REPURPOSED: usize = 47;
pub const UNAWAITED: usize = 48;
pub const MAGIC_STRINGS: usize = 49;
pub const SLEEPY_TEST: usize = 50;
pub const SKIPPED_TESTS: usize = 51;
pub const BOOL_TRAPS: usize = 52;
pub const COMMENTED_CODE: usize = 53;
pub const COHESION: usize = 54;
pub const SQL_BUILT: usize = 55;
pub const SHELLED_OUT: usize = 56;
pub const CEREMONY: usize = 57;

#[rustfmt::skip]
pub const METRICS: &[MetricDef] = &[
    MetricDef { name: "cognitive",     rung: 2, lo: None,       hi: Some(15.0), fmt: Fmt::Int, calib: Calib::P99 },
    MetricDef { name: "cyclomatic",    rung: 2, lo: None,       hi: Some(10.0), fmt: Fmt::Int, calib: Calib::P99 },
    MetricDef { name: "depth",         rung: 2, lo: None,       hi: Some(4.0),  fmt: Fmt::Int, calib: Calib::P99 },
    MetricDef { name: "length",        rung: 2, lo: None,       hi: Some(60.0), fmt: Fmt::Int, calib: Calib::P99 },
    MetricDef { name: "params",        rung: 2, lo: None,       hi: Some(5.0),  fmt: Fmt::Int, calib: Calib::P99 },
    MetricDef { name: "flag params",   rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    MetricDef { name: "expr depth",    rung: 1, lo: None,       hi: Some(10.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Attribute chains >=3 data links (Lieberherr: only talk to friends);
    // fluent call chains exempt, self forgives one link. Calibrated, not
    // policy: 16% of TigerBeetle "violates" hi=0 — systems code reaches
    // through explicit state paths, and a gate the gold fails is wrong.
    MetricDef { name: "demeter",       rung: 1, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::P99 },
    // Double negatives, negated negative names, De Morgan candidates
    // (K&P: say what you mean).
    MetricDef { name: "negations",     rung: 1, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // Longest local live span (McConnell ch. 13: a live variable is a
    // mental register the reader must hold).
    MetricDef { name: "live span",     rung: 2, lo: None,       hi: Some(40.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Unnamed non-trivial numbers (K&P, McConnell ch. 12).
    MetricDef { name: "magic numbers", rung: 0, lo: None,       hi: Some(3.0),  fmt: Fmt::Int, calib: Calib::P99 },
    // Action at a distance: eval/exec, computed attrs, metaclasses,
    // transmute, mutable defaults (Dijkstra: text must predict the run).
    // Rung 3, not 1. Every remaining gold firing was adjudicated a TRUE
    // positive — click's __import__(f"cmd_{name}"), starlette's
    // globals()[f"_{name}"] and getattr(self, handler_name), attrs'
    // eval(bytecode), vscode's eval of a source expression. The detector
    // is right and dynamic dispatch is still a legitimate design choice,
    // so this reports rather than blocks: 7.5% of admired Python and 4.1%
    // of admired JavaScript cannot be a build failure. Guarded lookups
    // (3-arg getattr), declared __getattr__ proxies, TypeScript's
    // mandated Error-subclass setPrototypeOf, and test files are exempt.
    // Promote back to rung 1 if every language reads under 1%.
    MetricDef { name: "spooky",        rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // A layer that only forwards adds interface without abstraction
    // (Ousterhout's shallow wrapper, Fowler's Middle Man).
    MetricDef { name: "pass-through",  rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // Public **kwargs: an interface that reveals nothing about its contract.
    MetricDef { name: "kw opacity",    rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // A name with "and" in it confesses two responsibilities — temporal
    // cohesion, the worst rung of Constantine & Yourdon's ladder.
    MetricDef { name: "and name",      rung: 4, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // A name built entirely from junk vocabulary carries no theory (Naur;
    // Ward Cunningham's expectation test).
    MetricDef { name: "generic name",  rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // A method living in another object's data belongs on that object
    // (Fowler). Suspicion: visitors and serializers legitimately envy.
    MetricDef { name: "feature envy",  rung: 4, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // Zen: errors should never pass silently — a silent handler is a
    // violation, gated.
    MetricDef { name: "swallowed",     rung: 2, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // Bare/Exception-wide catches: suspicion (top-level handlers are legit).
    MetricDef { name: "broad catch",   rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // unwrap/expect outside tests: panics where errors belonged.
    MetricDef { name: "unwraps",       rung: 3, lo: None,       hi: Some(2.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // The name is a contract: is_/has_ must return bool; get_ must not
    // take &mut self (Cunningham's expectation test, typed langs only).
    MetricDef { name: "lying name",    rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // Beck's first rule is "passes the tests" — a test that asserts
    // nothing passes vacuously.
    MetricDef { name: "test asserts",  rung: 3, lo: Some(1.0),  hi: None,       fmt: Fmt::Int, calib: Calib::Policy },
    // test_1/test_foo say nothing; a test name should state a behavior.
    MetricDef { name: "lazy test name",rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // Emitted only for complex units (cognitive >= ASSERT_WORTHY), and
    // rendered as a COVERAGE RATE, never per-unit findings: admired code
    // fails "every complex unit asserts" 89.6% of the time, and a
    // suspicion the gold corpus fails nine times in ten is a
    // distributional fact wearing the wrong rung. No budget — the rate
    // beside gold's rate is the entire verdict.
    MetricDef { name: "asserts",       rung: 5, lo: None,  hi: None,       fmt: Fmt::Int, calib: Calib::Policy },
    // Contract docs on the public surface only (Ousterhout: interface
    // comments are part of the interface; internal coverage is vanity).
    // A rate for the same reason as `asserts`: gold fails the per-unit
    // claim 80.6% of the time, so the honest verdict is "12% documented,
    // admired same-language reads 19%" — not 3,429 findings.
    MetricDef { name: "public docs",   rung: 5, lo: None,  hi: None,       fmt: Fmt::Int, calib: Calib::Policy },
    // Comments restating adjacent code (K&P: "don't just echo the code").
    // Rung 3, not 0: the gold corpus violates it in ALL EIGHT languages,
    // and a gate the admired corpus fails is measuring taste — the same
    // argument that moved `demeter` off Policy.
    //
    // Four precision repairs (closing-delimiter labels, first-line target
    // clamping, banner-vs-banner comparison, per-pack doc markers) took
    // the file rates from c 51/go 38/ts 18/zig 18/py 14/rs 6 percent to
    // c 42/go 26/ts 15/zig 14/py 8/rs 4. The residue is NOT a defect: C
    // and Go have no `///` convention, so `/* Free the vector set object
    // */` above freeVectorSetObject() is simultaneously the only
    // documentation and a literal restatement. That is a real property of
    // those ecosystems, which is exactly why this reports rather than
    // gates. Promote back to rung 0 only if every language reads under 5%.
    MetricDef { name: "echo comments", rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::Policy },
    // Two-sided in principle; one-sided in practice. Gold p05 is 0% in
    // every language — files without a single comment are ordinary in
    // admired code — so calibration writes lo = 0.00 and the "too few
    // comments is obscurity" flank can never fire. Kept for the day a
    // corpus disagrees; the generated file says so at each entry.
    MetricDef { name: "comment ratio", rung: 3, lo: Some(0.02), hi: Some(0.60), fmt: Fmt::Pct, calib: Calib::Band },
    // A parameter with no declared type is surface a refactor cannot be
    // checked against: the moment a caller changes shape, nothing fails
    // until runtime. Emitted only where the language HAS type syntax, so
    // JavaScript is silent rather than uniformly 100%. Calibrated, not
    // policy — Python's own gold corpus decides what its bar is, and a
    // gate the admired corpus fails is measuring taste.
    MetricDef { name: "untyped params", rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Worse than untyped, because it reads as a decision: `Any`, `any`,
    // `interface{}`, `anytype`, `void *` annotate without asserting.
    // TypeScript's `unknown` is deliberately NOT here — it is the safe
    // alternative that forces a narrowing, and flagging it would punish
    // the fix.
    MetricDef { name: "loose types",    rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A cast is where the compiler stops checking and starts believing.
    // Some are unavoidable (C has no other conversion; Rust's numeric
    // `as` is idiomatic), which is why this is calibrated per language
    // rather than decreed — the finding is a unit that casts far more
    // than its own ecosystem's tail does.
    MetricDef { name: "casts",          rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Type safety asserted by comment. Policy, not percentile: unlike a
    // cast this has no legitimate density — every one is a checker told
    // to stop looking, with no record of what it would have said.
    //
    // Gold files carrying at least one: py 16.5%, ts 1.3%, c 0%. Python
    // is an order of magnitude above TypeScript because its ecosystem
    // types libraries it does not own, and `# type: ignore` is how a
    // stub's gaps get papered over. That is a real finding about
    // gradually-typed Python, not a reason to soften the rule — which is
    // why it reports at rung 3 and never gates.
    MetricDef { name: "suppressions",   rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // Adjacent parameters of the same declared type are swappable at
    // every call site, and nothing — compiler, test, or reviewer reading
    // the call — will notice. The remedy is a newtype per role, or a
    // parameter object once the run gets long.
    //
    // Measures ADJACENCY, not totals: reordering a signature so the
    // same-typed parameters are not neighbours is itself a real fix, and
    // a total would not reward it.
    //
    // Gold p99 is 2 nearly everywhere (go 3, c 4), so the calibrated
    // budget fires at three in a row. That means the textbook
    // `copy(src: Path, dst: Path)` does NOT fire — a run of two is
    // ubiquitous in admired code, and gating on it would be noise. The
    // corpus decides where the bar is, not the anecdote.
    MetricDef { name: "confusable",     rung: 3, lo: None, hi: Some(2.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A name's length should scale with its scope (K&R; Uncle Bob):
    // `i` across three lines is clearer than `loop_index`, and `i`
    // across ninety is a reader holding a register with no label. The
    // value is that span, so the finding says how far the name has to
    // carry. Loop counters stay cheap because they die quickly.
    //
    // Rung 3, not 1. `live span` already gates at rung 2 on the very
    // same fact; gating again here would fail a build twice for one
    // variable, and what this adds on top — that the name is also too
    // short — is a review comment, not a violation. It fired eight times
    // on this repository and every one was true (`u` across 43 lines of
    // for_each), which is exactly the sort of finding a human should
    // read and then decide about.
    MetricDef { name: "terse name",     rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Names built from crushed vowels: `usr`, `cfg`, `mgr`, `bufr`. The
    // list is deliberately short and excludes abbreviations that have
    // become words in their own right — id, url, http, db, io, api, cpu
    // — because those are vocabulary, not compression.
    MetricDef { name: "abbreviated",    rung: 0, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // An assertion whose subject is a literal passes whatever the code
    // under test did. Beck's first rule is "passes the tests", and a
    // test that is green by construction is not passing anything. This
    // has no legitimate density, so it is policy rather than percentile
    // — unlike `test asserts`, which merely counts them.
    MetricDef { name: "vacuous asserts", rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // A handler that binds the error, raises a new one, and never
    // mentions the original throws away the stack that explains WHY.
    // The report then says only that something failed at the top, which
    // is the difference between a five-minute fix and an afternoon.
    // Python has `raise ... from err` (PEP 3134) and JS has `{ cause }`
    // for exactly this; languages without exceptions read zero.
    MetricDef { name: "lost context",   rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // A credential in the source is in the artifact and in the history,
    // so rotating it is a release rather than a config change. Rung 0:
    // this is token hygiene in the most literal sense, it gates, and
    // precision is bought by requiring the VALUE to carry a real key's
    // entropy rather than trusting a credential-shaped name.
    MetricDef { name: "secrets",        rung: 0, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // A blocking call inside an async unit stalls the whole executor,
    // not just its own task — the one bug where a single line silently
    // caps a server's throughput at one request. Only the unambiguous
    // forms count: every runtime ships its own sleep BECAUSE the
    // standard one parks the thread, and Node's *Sync family is named
    // after the problem.
    MetricDef { name: "blocking async", rung: 2, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // A resource opened outside a scope guard is closed only if every
    // path remembers to, and an early return or a raise is a path that
    // did not. `with`, `use` and `defer` exist so the block's exit does
    // it instead of the author.
    MetricDef { name: "unmanaged",      rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A spawned task whose handle is discarded: nothing can await it,
    // nothing observes its panic, and the runtime may drop it at
    // shutdown in the middle of a write.
    MetricDef { name: "dropped tasks",  rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A catch-all arm trades the compiler's help for silence: add a
    // variant and this match keeps compiling, handling the new case as
    // though it were every old case it never anticipated. Exhaustive
    // matching is most of what an ML-family type system is FOR, and a
    // wildcard opts out of it one construct at a time.
    //
    // Calibrated, because a match over an open domain — integers, HTTP
    // codes, bytes — needs a catch-all and always will. The finding is
    // a unit that reaches for one far more than its ecosystem does.
    MetricDef { name: "wildcard match", rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A parameter that names an identity but is typed as text: any
    // string fits, including another entity's id, a slug, or "". The
    // remedy is a newtype, which costs one line and makes the mistake
    // unrepresentable.
    MetricDef { name: "stringly id",    rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Mechanical sympathy, tier one: EXACT. Three nested loops is cubic
    // in whatever they range over — that is arithmetic, not a guess.
    //
    // Rung 3, not 2, and the distinction matters. The measurement is
    // exact but the VERDICT is not: a matrix multiply is three loops and
    // correct, and failing that build would be the tool being wrong
    // about code that is right. It fired twice on this repository —
    // surface_use at 3 and collect_clumps at 4 — and both are true;
    // collect_clumps really does enumerate parameter subsets in
    // quartic time, bounded only by a guard that stops at eight
    // parameters. That is worth a human reading it, which is what
    // rung 3 means, and is not worth stopping a build over.
    //
    // What this deliberately does NOT do is judge memory access patterns
    // or claim a loop should be vectorised. Both need types, alignment,
    // aliasing and the target ISA; a syntax tree has none of those, and a
    // tool that guessed would be wrong exactly where it mattered most.
    MetricDef { name: "loop depth",     rung: 3, lo: None, hi: Some(2.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Tier two: a NAME-based approximation, and labelled as one. Only
    // the names that mean nothing else — to_string, to_owned, to_vec,
    // deepcopy — each of which exists because the alternative is
    // borrowing. One inside a loop is a copy per iteration. `clone` is
    // absent on purpose: it is often the only way to satisfy the borrow
    // checker, and flagging it would be noise.
    MetricDef { name: "alloc in loop",  rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A hook reached through a branch. React identifies a hook by the
    // ORDER it is called in, so the first time the condition flips,
    // every hook after it renumbers and the component reads state that
    // belongs to a different hook — a corruption, not a style
    // question, and the reason eslint-plugin-react-hooks exists.
    //
    // Rung 2 and Policy: this has no legitimate density. It is not a
    // percentile to calibrate but a rule the framework itself states,
    // and admired code obeys it — gold reads 0.0%.
    MetricDef { name: "conditional hook", rung: 2, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // Four values travelling together are a struct in hiding — the
    // `params` argument pointed at the OTHER end of the signature, and
    // the remedy is the same Introduce Parameter Object. Only languages
    // that declare multi-value results play: a JS array or an OCaml
    // tuple is already one value, and Go's `(value, error)` idiom sets
    // that language's own budget through calibration (gold p99: go 3,
    // rs 2, py 2, ts/tsx 1).
    //
    // Rung 3, not 2, and the reason is TS: Go and Rust DECLARE every
    // width, so their budgets price width alone — but a TS tuple
    // annotation is optional, so its p99 of 1 rides on how often gold
    // annotates at all, and a declared `[value, setter]` pair is
    // legitimate style. One rung must fit the least-certain language.
    MetricDef { name: "returns",       rung: 3, lo: None,       hi: Some(3.0),  fmt: Fmt::Int, calib: Calib::P99 },
    // The bigger the interface, the weaker the abstraction — Go's own
    // proverb, and the Interface Segregation Principle in one number.
    // An implementer owes every method whether or not any caller
    // wanted them together, so width is a tax on every implementation
    // that will ever exist. Methods only: a TS props shape is a
    // record, and an embedded interface is composition — the cure for
    // width, never billed as the disease.
    MetricDef { name: "interface width", rung: 3, lo: None, hi: Some(12.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A straight-line `x = ...` whose new value never mentions the old
    // gives the same name a SECOND meaning — Fowler's Split Variable —
    // and every earlier read the reader remembers is silently wrong.
    // This is the one def-use insight worth having at syntax cost:
    // what a data-flow graph would call a killed definition, judged
    // only where no types or aliasing are needed to see it. Exempt by
    // construction: collecting updates (the value mentions the name),
    // compound operators, conditional overrides, loop refills, and
    // try-sheltered fills.
    MetricDef { name: "repurposed",    rung: 3, lo: None,       hi: Some(0.0),  fmt: Fmt::Int, calib: Calib::P99 },
    // A statement-position call to a SAME-FILE async unit, no await,
    // result discarded. In Python the coroutine is created and never
    // runs; in Rust the future is dropped unpolled; in TS the promise
    // floats with nobody to catch its rejection. Same-file,
    // unambiguous-name evidence only — a cross-file callee or a name
    // with a sync twin is never guessed at.
    //
    // Rung 2 and Policy: like the conditional hook, this is a rule the
    // runtimes themselves state (Python warns "coroutine was never
    // awaited" at runtime; the evidence here arrives at read time).
    MetricDef { name: "unawaited coroutine", rung: 2, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // `pointless async` — an async unit that never awaits — was built,
    // measured, and REJECTED. Admired code violates it 21.6% of the
    // time in Python, 17.4% in TypeScript, 18.2% in TSX and 20.5% in
    // JavaScript: three to four times the suspicion ceiling, in every
    // language at once, which is the signature of a distributional
    // fact rather than a defect.
    //
    // The samples say why, and every reason is legitimate: `__aiter__`
    // MUST be async because the async-iterator protocol says so;
    // `aiter_bytes`/`aiter_text` are async generators, where `yield`
    // is the point and no await is required; httpx's `aread` exists to
    // mirror `read` across an API boundary; and test frameworks accept
    // async bodies uniformly whether or not a given test awaits. The
    // remainder is interface conformance, which needs types to see.
    //
    // Same argument that turned `asserts` and `public docs` into rates
    // and moved `demeter` off Policy. Do not re-propose without new
    // evidence — the measurement is cheap to repeat and it said no.
    //
    // The other half of `magic numbers`: the same non-trivial string
    // written out again and again is a constant nobody named, and
    // clone detection deliberately cannot see it (duplicated DATA is
    // content, not logic), so nothing else in the tool owns this.
    MetricDef { name: "magic strings", rung: 4, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // A test that sleeps waits for a duration instead of a condition,
    // and the duration is a guess about a machine it will not run on.
    // Inside a test the async exemption REVERSES: `await
    // asyncio.sleep(...)` is the right way to yield an executor and
    // the wrong way to wait for a result, so every flavour counts.
    MetricDef { name: "sleepy test",   rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // The suppression that wears a test's name: the suite still
    // reports green and nothing records what the test would have
    // said — a sibling of `suppressions`, where a checker was told to
    // stop looking. Unconditional skips only: `skipif(platform)` and a
    // guarded `t.Skip()` are stated judgment, and the test still runs
    // where it applies.
    MetricDef { name: "skipped tests", rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // `move(x, true, false)` — the CALL-SITE complement of `flag
    // params`, and the only version that can see a third party's
    // signature, since the declaration lives in someone else's
    // repository. Two is the threshold, not one: a lone `force` flag
    // reads fine, and two is where nothing says which is which and
    // swapping them still type-checks. A keyword argument is exempt —
    // naming it at the call site IS the remedy.
    MetricDef { name: "bool traps",    rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Code someone commented out instead of deleting — which the
    // version control system was already remembering for them. Judged
    // by PARSING it: a block of two or more non-doc comment lines that
    // reads as valid source with two or more statements. Doc comments
    // are exempt whatever they contain, since an example in a
    // docstring is the point of the docstring.
    MetricDef { name: "commented code", rung: 3, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::P99 },
    // Hitz & Montazeri's LCOM4: how many disconnected groups a class's
    // methods fall into, where two are connected when they touch a
    // member in common or one calls the other. One group is cohesive;
    // more means the class is several objects sharing a name, and the
    // remedy is Extract Class. Rung 4 is what this rung is FOR — it
    // held only `and name` and `feature envy` before.
    //
    // Methods touching NO member are excluded: a helper that reads no
    // state is a free function living in a class, and counting it as
    // its own island would call every class with a helper incoherent.
    //
    // The known blind spot, stated rather than hidden: a data holder
    // with one accessor per field reads as many groups and is a
    // legitimate design. regex's RegexTest — twelve independent
    // fields, an accessor each — sits in the tail beside vscode's
    // CommandCenter, which registers 192 commands in one class. Both
    // are TRUE readings of the same number; only a person can say
    // which one wanted fixing, which is exactly what a suspicion is.
    MetricDef { name: "cohesion",      rung: 4, lo: None, hi: Some(1.0), fmt: Fmt::Int, calib: Calib::P99 },
    // An SQL statement ASSEMBLED from values rather than written: an
    // f-string, a template literal, a Sprintf. The oldest
    // vulnerability there is, and the one whose remedy — a parameter
    // marker — this deliberately cannot see, because a parameterized
    // query carries no interpolation at all. A fully literal query is
    // silent whatever it says.
    //
    // Anchored at the START of the string: a log line mentioning
    // "select" is prose, and only a string that begins as a statement
    // is one.
    MetricDef { name: "built query",   rung: 2, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // The same hole at a bigger sink: a command ASSEMBLED and handed
    // to a shell, where the shell will re-parse whatever was spliced
    // in. `shell=True` with a literal command is a style choice and
    // stays silent — nothing untrusted reaches the parser — and the
    // remedy, an argument LIST, needs no shell and carries no
    // interpolation, so the fix makes the finding disappear.
    MetricDef { name: "shelled out",   rung: 2, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
    // A declaration that asserts nothing, dressed as though it asserts
    // something: nullary, body is one bare literal, and three or more
    // lines of documentation above it.
    //
    // The CONJUNCTION carries the whole signal. Bare content-free-ness
    // is worthless as a measure — gold Rust runs 51 such declarations
    // per thousand and one AI corpus runs 100, both ABOVE the corpus
    // this was built from at 61. The doc gate is what separates: at
    // three lines the human rate over 512,927 declarations in twelve
    // languages is 11, and at ten lines it is 1.
    //
    // Override points are excluded, and that exclusion is the metric.
    // A trait default carrying nine lines of documentation is
    // documented at length so implementors know when to replace it;
    // counting those produced 79 false positives on admired code, and
    // dropping them cost 6 real findings out of 3,187.
    MetricDef { name: "ceremony",      rung: 2, lo: None, hi: Some(0.0), fmt: Fmt::Int, calib: Calib::Policy },
];

/// A declaration that asserts nothing, carrying documentation that says
/// it does.
///
/// Every clause is load-bearing and was measured against 512,927 human
/// declarations. Nullary, because a function taking arguments does
/// something with them. A bare literal body, because that is the whole
/// of "asserts nothing". Three doc lines, because bare content-free-ness
/// is COMMONER in admired code than in the corpus this was built from,
/// and only the documentation separates them. Not an override point,
/// because a trait default is documented for implementors on purpose.
/// Not a test, because a stub returning `true` is how a fixture is
/// written.
fn ceremony(u: &UnitFacts) -> u32 {
    let documented_nothing = u.params.is_empty()
        && !u.is_method
        && u.doc_lines >= 3
        && u.body != BodyShape::Real
        && !u.is_override
        && !u.is_test;
    documented_nothing as u32
}

/// Cognitive complexity at which a unit is expected to state invariants.
const ASSERT_WORTHY: u32 = 10;

/// A live span this long makes a one- or two-character name a register
/// the reader has to hold with no label on it. Below it, terse is fine.
const TERSE_SPAN: u16 = 12;

/// Crushed vowels. Deliberately short, and deliberately excluding the
/// abbreviations that became words — id, url, http, db, io, api, cpu,
/// max, min, len — because those are vocabulary, not compression.
const CRUSHED: &[&str] = &[
    "usr", "cfg", "mgr", "bufr", "buf", "ctx", "obj", "val", "str", "cnt", "idx", "tmp", "cmd",
    "msg", "req", "res", "resp", "err", "attr", "arg", "params", "opts", "prev", "curr", "elem",
    "src", "dst", "dest", "num", "pos", "ptr", "ref", "func", "impl", "init", "calc", "conv",
];

/// Vocabulary that names nothing: a unit named entirely from this list
/// fails Ward Cunningham's test ("each routine is pretty much what you
/// expected"). Conventional idioms (main, new, init, run, get, set) are
/// deliberately absent — they carry convention, which is meaning.
const JUNK_WORDS: &[&str] = &[
    "util", "utils", "helper", "helpers", "manager", "managers", "handler", "handlers", "handle",
    "misc", "common", "stuff", "thing", "things", "tmp", "temp", "foo", "bar", "baz", "data",
    "info", "obj", "val", "impl", "generic", "process", "func", "function", "method", "wrapper",
];

/// Verbs that mean the same operation. A codebase that offers both
/// `get_user` and `fetch_user` makes a reader learn two words for one
/// idea, and makes a searcher find half the call sites.
const SYNONYMS: &[&[&str]] = &[
    &["get", "fetch", "retrieve", "load", "read"],
    &["set", "put", "store", "save", "write"],
    &["make", "create", "build", "construct"],
    &["delete", "remove", "destroy", "drop"],
    &["check", "validate", "verify", "ensure"],
    &["convert", "transform", "translate"],
    &["find", "search", "lookup", "locate"],
    &["start", "begin", "launch", "spawn"],
    &["stop", "halt", "cancel", "abort"],
];

/// A name's leading verb and the object it acts on, when the verb is one
/// this codebase might also spell another way. The verb is returned as
/// the canonical spelling FROM the table, so it is `'static`.
pub fn split_synonym(name: &str) -> Option<(&'static str, String)> {
    let words = name_words(name);
    let (head, rest) = words.split_first()?;
    if rest.is_empty() {
        return None;
    }
    let verb = SYNONYMS
        .iter()
        .flat_map(|family| family.iter())
        .find(|v| **v == head)?;
    Some((verb, rest.join("_")))
}

/// How a name is spelled. A codebase that uses one of these for its
/// functions is one a reader can predict; a codebase that uses three has
/// made every name a small guess.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Snake,
    Camel,
    Pascal,
    Screaming,
    /// Single words, prose labels, operators — nothing to be consistent
    /// about, so they are counted separately and excluded from entropy.
    Neutral,
}

pub const CASES: usize = 4;

/// Which spelling this name uses. Only names with internal structure
/// carry a style: `run` is neither snake nor camel.
pub fn case_of(name: &str) -> Case {
    let has_underscore = name.contains('_');
    let upper = name.chars().filter(|c| c.is_uppercase()).count();
    let lower = name.chars().filter(|c| c.is_lowercase()).count();
    let leads_upper = name.starts_with(char::is_uppercase);
    match () {
        _ if has_underscore && upper > 0 && lower == 0 => Case::Screaming,
        _ if has_underscore && lower > 0 => Case::Snake,
        _ if upper > 0 && lower > 0 && !leads_upper => Case::Camel,
        _ if upper > 0 && lower > 0 && leads_upper => Case::Pascal,
        _ => Case::Neutral,
    }
}

/// Normalized Shannon entropy of a spelling distribution: 0 when one
/// style is used throughout, 1 when every style is equally likely.
/// This is the number that separates a house style from a habit.
pub fn idiom_entropy(counts: &[u32; CASES]) -> f64 {
    let total: u32 = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let used = counts.iter().filter(|n| **n > 0).count();
    if used < 2 {
        return 0.0;
    }
    let h: f64 = counts
        .iter()
        .filter(|n| **n > 0)
        .map(|n| {
            let p = *n as f64 / total as f64;
            -p * p.log2()
        })
        .sum();
    h / (CASES as f64).log2()
}

/// Lowercased words of an identifier: snake, camel, whitespace (Zig test
/// labels are prose) and dot splits — a dot joins two names in every
/// language that writes one, and gtest's `args_test.basic` is three
/// words rather than one.
fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    for chunk in name.split(|c: char| matches!(c, '_' | '-' | '.') || c.is_whitespace()) {
        let mut start = 0;
        let mut prev_lower = false;
        for (i, c) in chunk.char_indices() {
            if c.is_uppercase() && prev_lower {
                words.push(chunk[start..i].to_ascii_lowercase());
                start = i;
            }
            prev_lower = c.is_lowercase();
        }
        if start < chunk.len() {
            words.push(chunk[start..].to_ascii_lowercase());
        }
    }
    words.retain(|w| !w.is_empty());
    words
}

pub const N: usize = METRICS.len();

/// Emit every (metric, value, line, unit label) for one file.
pub fn for_each(facts: &FileFacts, mut f: impl FnMut(usize, f32, u32, &str)) {
    for u in &facts.units {
        unit_metrics(u, facts, &mut f);
    }
    file_metrics(facts, &mut f);
}

/// Everything one unit is measured by.
fn unit_metrics(u: &UnitFacts, facts: &FileFacts, f: &mut impl FnMut(usize, f32, u32, &str)) {
    let (cog, cyc) = complexity(u);
    f(CEREMONY, ceremony(u) as f32, u.line, &u.qualname);
    f(COGNITIVE, cog as f32, u.line, &u.qualname);
    f(CYCLOMATIC, cyc as f32, u.line, &u.qualname);
    f(DEPTH, u.max_vis_depth as f32, u.line, &u.qualname);
    f(LOOP_DEPTH, u.max_loop_depth as f32, u.line, &u.qualname);
    f(ALLOC_IN_LOOP, u.allocs_in_loop as f32, u.line, &u.qualname);
    f(
        CONDITIONAL_HOOK,
        u.conditional_hooks as f32,
        u.line,
        &u.qualname,
    );
    f(EXPR_DEPTH, u.max_expr_depth as f32, u.line, &u.qualname);
    f(DEMETER, u.demeter as f32, u.line, &u.qualname);
    f(NEGATIONS, u.negations as f32, u.line, &u.qualname);
    // Test bodies are made of literals — expected values, fixtures,
    // status codes. Judging them by production budgets made 88% of
    // this gating metric's firings noise (same guard as UNWRAPS).
    if !u.is_test && !facts.is_test_file {
        f(MAGIC_NUMBERS, u.magic_numbers as f32, u.line, &u.qualname);
    }
    // A file's top level is code too, and in shell it is the WHOLE
    // program: a provisioning script that sets REGION at line 40 and
    // resets it at line 300 deploys to the wrong place, and that was
    // invisible while this metric only spoke inside functions. Module
    // scope has no parameters and closes no handlers, so it reaches
    // none of the other unit_shape metrics — this one it does.
    if u.is_module && !u.is_test && !facts.is_test_file {
        f(REPURPOSED, u.repurposed as f32, u.line, &u.qualname);
    }
    if !u.is_module {
        unit_shape(u, facts, f);
        names_and_contracts(u, cog, f);
    }
}

/// What a unit's SHAPE is measured by — everything a module scope has
/// no answer to (it declares no parameters and closes no handlers).
fn unit_shape(u: &UnitFacts, facts: &FileFacts, f: &mut impl FnMut(usize, f32, u32, &str)) {
    {
        {
            f(LENGTH, u.lines as f32, u.line, &u.qualname);
            f(LIVE_SPAN, u.max_live_span as f32, u.line, &u.qualname);
            f(PARAMS, u.params.len() as f32, u.line, &u.qualname);
            f(FLAG_PARAMS, u.flag_params() as f32, u.line, &u.qualname);
            f(RETURN_ARITY, u.return_arity as f32, u.line, &u.qualname);
            // Untyped surface only means something where the language has
            // type syntax at all: JavaScript would read a uniform 100%.
            if facts.lang.pack().types_declared {
                f(
                    UNTYPED_PARAMS,
                    u.untyped_params() as f32,
                    u.line,
                    &u.qualname,
                );
                f(LOOSE_TYPES, u.loose_params() as f32, u.line, &u.qualname);
                f(CONFUSABLE, u.confusable_run() as f32, u.line, &u.qualname);
                f(STRINGLY_ID, u.stringly_ids() as f32, u.line, &u.qualname);
            }
            f(
                WILDCARD_MATCH,
                u.wildcard_matches as f32,
                u.line,
                &u.qualname,
            );
            f(
                PASSTHROUGH,
                u.is_passthrough as u32 as f32,
                u.line,
                &u.qualname,
            );
            f(SWALLOWED, u.swallowed as f32, u.line, &u.qualname);
            f(BROAD_CATCH, u.broad_catch as f32, u.line, &u.qualname);
            f(LOST_CONTEXT, u.lost_context as f32, u.line, &u.qualname);
            f(UNAWAITED, u.unawaited as f32, u.line, &u.qualname);
            if !u.is_test {
                f(UNWRAPS, u.unwraps as f32, u.line, &u.qualname);
                f(CASTS, u.casts as f32, u.line, &u.qualname);
                // A test runs scenarios in sequence, refilling one
                // variable per scenario — the idiom of the genre, not
                // a second meaning. Gold said so loudly: every top
                // violator unguarded was a test body (test_deque 14,
                // TestFlagCompletion 11, listpackTest 82).
                f(REPURPOSED, u.repurposed as f32, u.line, &u.qualname);
                // A table test enumerates the truth table on purpose —
                // vscode's pickRunningLocation calls its subject 64
                // times across every boolean combination, which is the
                // CORRECT way to test two booleans. Same exemption as
                // magic numbers and unwraps, for the same reason.
                f(BOOL_TRAPS, u.bool_traps as f32, u.line, &u.qualname);
                // A test named `test_do_not_block_on_background_tasks`
                // sleeps inside async on purpose: the blocking IS the
                // subject. Same exemption as unwraps and magic numbers.
                f(
                    BLOCKING_IN_ASYNC,
                    u.blocking_calls as f32,
                    u.line,
                    &u.qualname,
                );
                // Tests open scratch files and spawn throwaway tasks by
                // the hundred; the lifetime that matters is production's.
                f(UNMANAGED, u.unmanaged as f32, u.line, &u.qualname);
                f(DROPPED_TASKS, u.dropped_tasks as f32, u.line, &u.qualname);
            }
        }
    }
}

/// Everything the FILE is measured by, rather than any one unit.
fn file_metrics(facts: &FileFacts, f: &mut impl FnMut(usize, f32, u32, &str)) {
    // Ratio over non-blank lines: a file of code with no commentary scores 0,
    // a wall of comments approaches 1.
    if facts.lines > 0 {
        let denom = (facts.lines - facts.blank_lines).max(1);
        let ratio = facts.comment_lines as f32 / denom as f32;
        f(COMMENT_RATIO, ratio, 1, "");
        let first_secret = facts.secrets.iter().min().copied().unwrap_or(1);
        f(SECRETS, facts.secrets.len() as f32, first_secret, "");
        let first_suppression = facts.suppressions.iter().min().copied().unwrap_or(1);
        f(
            SUPPRESSIONS,
            facts.suppressions.len() as f32,
            first_suppression,
            "",
        );
        let first_sql = facts.sql_built.first().copied().unwrap_or(1);
        f(SQL_BUILT, facts.sql_built.len() as f32, first_sql, "");
        let first_shell = facts.shelled_out.first().copied().unwrap_or(1);
        f(SHELLED_OUT, facts.shelled_out.len() as f32, first_shell, "");
        let first_dead = facts.commented_code.first().copied().unwrap_or(1);
        f(
            COMMENTED_CODE,
            facts.commented_code.len() as f32,
            first_dead,
            "",
        );
        let first_skip = facts.skipped_tests.iter().min().copied().unwrap_or(1);
        f(
            SKIPPED_TESTS,
            facts.skipped_tests.len() as f32,
            first_skip,
            "",
        );
        let first_magic = facts.magic_strings.first().copied().unwrap_or(1);
        f(
            MAGIC_STRINGS,
            facts.magic_strings.len() as f32,
            first_magic,
            "",
        );
        let first_echo = facts.echo_comments.iter().min().copied().unwrap_or(1);
        f(
            ECHO_COMMENTS,
            facts.echo_comments.len() as f32,
            first_echo,
            "",
        );
        // Tests exercise dynamic machinery on purpose — metaclasses,
        // setattr, monkeypatched fixtures are the point of the test.
        if !facts.is_test_file {
            let first_spooky = facts.spooky_lines.iter().min().copied().unwrap_or(1);
            f(SPOOKY, facts.spooky_lines.len() as f32, first_spooky, "");
        }
    }
    for i in &facts.interfaces {
        f(INTERFACE_WIDTH, i.methods as f32, i.line, &i.name);
    }
    // Tests group by fixture, not by state: a test class's methods
    // share a subject rather than a field, and judging that as
    // incoherence would flag every well-organized suite.
    if !facts.is_test_file {
        for c in &facts.classes {
            f(COHESION, c.groups as f32, c.line, &c.name);
        }
    }
}

/// What a unit's NAME promises and whether the unit keeps it: honest
/// vocabulary, honest receiver, honest predicate, honest test, honest
/// public surface. All of these read `name_words` once.
/// The identifier-convention metrics: conjunctions, junk vocabulary,
/// crushed vowels. A declared test never reaches here — its name is
/// PROSE, a sentence stating a behavior, where "and" is grammar and
/// identifier conventions do not apply. Judging tests as identifiers
/// flagged every well-named test: 36 of 36 and-name findings on this
/// repository were tests. Same reasoning collect_spellings applies.
fn identifier_names(u: &UnitFacts, words: &[String], f: &mut impl FnMut(usize, f32, u32, &str)) {
    let conjoined = words.iter().any(|w| w == "and");
    f(AND_NAME, conjoined as u32 as f32, u.line, &u.qualname);
    let generic = !words.is_empty() && words.iter().all(|w| JUNK_WORDS.contains(&w.as_str()));
    f(GENERIC_NAME, generic as u32 as f32, u.line, &u.qualname);
    let crushed = words
        .iter()
        .filter(|w| CRUSHED.contains(&w.as_str()))
        .count();
    f(ABBREVIATED, crushed as f32, u.line, &u.qualname);
}

fn names_and_contracts(u: &UnitFacts, cog: u32, f: &mut impl FnMut(usize, f32, u32, &str)) {
    let words = name_words(&u.name);
    // Terse-name stays judged for tests: it is about a VARIABLE's
    // name, and live span judges tests too.
    if !u.named_test {
        identifier_names(u, &words, f);
    }
    // A short name is only a problem once it has to carry a long way.
    let terse = u.max_live_var.chars().count() <= 2 && u.max_live_span >= TERSE_SPAN;
    f(
        TERSE_NAME,
        if terse { u.max_live_span as f32 } else { 0.0 },
        u.line,
        &u.qualname,
    );
    // Envy fires only when a foreign receiver clearly dominates.
    let envious = u.is_method && u.envy_count >= 4 && u.envy_count > u.self_accesses;
    f(
        FEATURE_ENVY,
        if envious { u.envy_count as f32 } else { 0.0 },
        u.line,
        &u.qualname,
    );
    let head = words.first().map(String::as_str);
    let lying_predicate = matches!(head, Some("is" | "has" | "can" | "should"))
        && !u.returns.is_empty()
        && !u.returns.contains("bool");
    let mutating_getter = head == Some("get") && u.mut_receiver;
    f(
        LYING_NAME,
        (lying_predicate || mutating_getter) as u32 as f32,
        u.line,
        &u.qualname,
    );
    // Assertions arrive as statements (Python `assert`) or as calls
    // matched by the pack's asserty hook (assert!, std.debug.assert).
    let asserts =
        u.assert_calls as u32 + u.ctrl.iter().filter(|c| c.sem == Sem::Assert).count() as u32;
    // Test-quality signals judge declared tests only (#[test], test_-named,
    // `test` blocks) — test-file helpers are exempt.
    if u.named_test {
        f(TEST_ASSERTS, asserts as f32, u.line, &u.qualname);
        f(SLEEPY_TEST, u.sleep_calls as f32, u.line, &u.qualname);
        f(
            VACUOUS_ASSERTS,
            u.vacuous_asserts as f32,
            u.line,
            &u.qualname,
        );
        f(
            LAZY_TEST_NAME,
            (words.len() < 3) as u32 as f32,
            u.line,
            &u.qualname,
        );
    }
    if cog >= ASSERT_WORTHY {
        f(ASSERTS, asserts as f32, u.line, &u.qualname);
    }
    if u.is_public {
        let kw = u.params.iter().any(|p| p.kw_splat);
        f(KW_OPACITY, kw as u32 as f32, u.line, &u.qualname);
        f(
            PUBLIC_DOCS,
            (u.doc_lines > 0) as u32 as f32,
            u.line,
            &u.qualname,
        );
    }
}

/// One control event's contribution to (cognitive, cyclomatic).
///
/// Cognitive follows SonarSource semantics: structural constructs cost
/// 1 + nesting, `elif`/`else`/filters cost a flat 1, boolean operators 1 per
/// new sequence. Cyclomatic counts every decision point.
pub fn event_score(c: &CtrlFact) -> (u32, u32) {
    let cog = if c.sem.cognitive_nested() {
        1 + c.cog_depth as u32
    } else if c.sem.cognitive_flat() || (c.sem == Sem::BoolOp && c.new_seq) {
        1
    } else {
        0
    };
    (cog, c.sem.cyclomatic() as u32)
}

/// (cognitive, cyclomatic) totals; direct recursion adds 1 cognitive,
/// cyclomatic starts at 1.
pub fn complexity(u: &UnitFacts) -> (u32, u32) {
    let mut cog = u.self_recursive as u32;
    let mut cyc = 1;
    for c in &u.ctrl {
        let (dc, dy) = event_score(c);
        cog += dc;
        cyc += dy;
    }
    (cog, cyc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::extract;
    use crate::lang::Lang;
    use std::path::Path;

    fn unit_metrics(source: &str) -> (u32, u32, u16) {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let facts = extract(pack, &mut parser, Path::new("t.py"), source);
        let u = &facts.units[1];
        let (cog, cyc) = complexity(u);
        (cog, cyc, u.max_vis_depth)
    }

    #[test]
    fn metric_indices_match_registry_names() {
        // The consts and the METRICS table must never drift: a silent
        // mismatch would misattribute every value to the wrong metric.
        const PAIRS: &[(usize, &str)] = &[
            (CEREMONY, "ceremony"),
            (COGNITIVE, "cognitive"),
            (CYCLOMATIC, "cyclomatic"),
            (DEPTH, "depth"),
            (LENGTH, "length"),
            (PARAMS, "params"),
            (FLAG_PARAMS, "flag params"),
            (EXPR_DEPTH, "expr depth"),
            (DEMETER, "demeter"),
            (NEGATIONS, "negations"),
            (LIVE_SPAN, "live span"),
            (MAGIC_NUMBERS, "magic numbers"),
            (SPOOKY, "spooky"),
            (PASSTHROUGH, "pass-through"),
            (KW_OPACITY, "kw opacity"),
            (AND_NAME, "and name"),
            (GENERIC_NAME, "generic name"),
            (FEATURE_ENVY, "feature envy"),
            (SWALLOWED, "swallowed"),
            (BROAD_CATCH, "broad catch"),
            (UNWRAPS, "unwraps"),
            (LYING_NAME, "lying name"),
            (TEST_ASSERTS, "test asserts"),
            (LAZY_TEST_NAME, "lazy test name"),
            (ASSERTS, "asserts"),
            (PUBLIC_DOCS, "public docs"),
            (ECHO_COMMENTS, "echo comments"),
            (COMMENT_RATIO, "comment ratio"),
            (UNTYPED_PARAMS, "untyped params"),
            (LOOSE_TYPES, "loose types"),
            (CASTS, "casts"),
            (SUPPRESSIONS, "suppressions"),
            (CONFUSABLE, "confusable"),
            (TERSE_NAME, "terse name"),
            (ABBREVIATED, "abbreviated"),
            (VACUOUS_ASSERTS, "vacuous asserts"),
            (LOST_CONTEXT, "lost context"),
            (SECRETS, "secrets"),
            (BLOCKING_IN_ASYNC, "blocking async"),
            (UNMANAGED, "unmanaged"),
            (DROPPED_TASKS, "dropped tasks"),
            (WILDCARD_MATCH, "wildcard match"),
            (STRINGLY_ID, "stringly id"),
            (LOOP_DEPTH, "loop depth"),
            (ALLOC_IN_LOOP, "alloc in loop"),
            (CONDITIONAL_HOOK, "conditional hook"),
            (RETURN_ARITY, "returns"),
            (INTERFACE_WIDTH, "interface width"),
            (REPURPOSED, "repurposed"),
            (UNAWAITED, "unawaited coroutine"),
            (MAGIC_STRINGS, "magic strings"),
            (SLEEPY_TEST, "sleepy test"),
            (SKIPPED_TESTS, "skipped tests"),
            (BOOL_TRAPS, "bool traps"),
            (COMMENTED_CODE, "commented code"),
            (COHESION, "cohesion"),
            (SQL_BUILT, "built query"),
            (SHELLED_OUT, "shelled out"),
        ];
        for (idx, name) in PAIRS {
            assert_eq!(METRICS[*idx].name, *name, "index {idx}");
        }
        assert_eq!(N, 58);
    }

    #[test]
    fn a_budget_says_whether_the_corpus_or_the_default_set_it() {
        use crate::lang::Lang;
        // Pinned: 190k TypeScript units decide this one.
        assert!(is_pinned(Lang::TypeScript, COGNITIVE));
        // Default, and the distinction is the whole point: Go declares
        // 29 interfaces in all of gold, two orders below the sample
        // floor a percentile needs, so the number comes from this file
        // rather than from admired code.
        assert!(!is_pinned(Lang::Go, INTERFACE_WIDTH));
        assert!(is_pinned(Lang::TypeScript, INTERFACE_WIDTH));
        // A policy is never pinned — no percentile may legitimize it.
        for lang in crate::lang::LANGS {
            assert!(!is_pinned(lang, SECRETS), "{lang:?} pinned a policy");
        }
        // A `rate` entry records coverage, not a budget, and must not
        // read as one.
        assert!(!is_pinned(Lang::Rust, PUBLIC_DOCS));
        // CUDA is pinned now, and the fact that it took a change to
        // CALIBRATION rather than to the corpus is the point. The units
        // were always there; what blocked them was that a repository
        // fetched for CUDA also donated its build tooling to Python and
        // its host code to C++, moving 46 budgets in languages nobody
        // was editing. Scoping ended that, and this asserts the payoff
        // did not quietly regress.
        for m in [COGNITIVE, LENGTH, PARAMS, MAGIC_NUMBERS] {
            assert!(
                is_pinned(Lang::Cuda, m),
                "CUDA lost a pinned budget — the corpus or the scoping rule regressed"
            );
        }
    }

    #[test]
    fn the_readme_cannot_drift_from_the_calibration() {
        // The README quotes gold numbers in prose, and stale prose
        // about fresh data teaches readers to distrust both. Every
        // quoted number must be the compiled calibration's number.
        use crate::lang::Lang;
        // Newline-folded: prose wraps wherever the formatter likes.
        let readme = include_str!("../../README.md").replace('\n', " ");
        let readme = readme.as_str();
        let cal = LangBudgets::calibrated();
        let hi = |lang: Lang, m: usize| cal.for_lang(lang).0[m].1.expect("calibrated");
        let trio = format!(
            "cognitive p99 lands at py {:.0}, rs {:.0}, ts {:.0}",
            hi(Lang::Python, COGNITIVE),
            hi(Lang::Rust, COGNITIVE),
            hi(Lang::TypeScript, COGNITIVE),
        );
        assert!(readme.contains(&trio), "README drifted: {trio:?} missing");
        let ml = format!(
            "cognitive p99 = {:.0} and length p99 = {:.0}",
            hi(Lang::OCaml, COGNITIVE),
            hi(Lang::OCaml, LENGTH),
        );
        assert!(readme.contains(&ml), "README drifted: {ml:?} missing");
        let against = format!(
            "Python's {:.0}/{:.0}, Rust's {:.0}/{:.0} and TypeScript's {:.0}/{:.0}",
            hi(Lang::Python, COGNITIVE),
            hi(Lang::Python, LENGTH),
            hi(Lang::Rust, COGNITIVE),
            hi(Lang::Rust, LENGTH),
            hi(Lang::TypeScript, COGNITIVE),
            hi(Lang::TypeScript, LENGTH),
        );
        assert!(
            readme.contains(&against),
            "README drifted: {against:?} missing"
        );
        let comments = format!(
            "comment ceiling to {:.0}%",
            hi(Lang::Rust, COMMENT_RATIO) * 100.0
        );
        assert!(
            readme.contains(&comments),
            "README drifted: {comments:?} missing"
        );
        let cpp = format!(
            "cognitive p99 = {:.0} and length p99 = {:.0}, against C's {:.0} and {:.0}",
            hi(Lang::Cpp, COGNITIVE),
            hi(Lang::Cpp, LENGTH),
            hi(Lang::C, COGNITIVE),
            hi(Lang::C, LENGTH),
        );
        assert!(readme.contains(&cpp), "README drifted: {cpp:?} missing");
        // The prose does not just quote those four numbers, it draws a
        // conclusion from them — so the conclusion is pinned too. The
        // first draft of this sentence called C++ the loosest of the
        // eleven and this loop is what caught it.
        assert!(
            hi(Lang::Cpp, COGNITIVE) * 2.0 < hi(Lang::C, COGNITIVE),
            "README claims C++ branches less than half as hard as C; it no longer does"
        );
        assert!(
            hi(Lang::Cpp, LENGTH) * 3.0 < hi(Lang::C, LENGTH) * 2.0,
            "README claims C++ units are under two thirds of C's length; they no longer are"
        );
    }

    #[test]
    fn the_readme_cuda_numbers_cannot_drift_either() {
        // This paragraph went stale within a DAY of being written: a
        // two-point [cu] length move left the prose quoting 247 against
        // a calibration saying 249. Unpinned quoted numbers are exactly
        // the failure the drift tests exist for, so it gets its own.
        use crate::lang::Lang;
        let readme = include_str!("../../README.md").replace('\n', " ");
        let cal = LangBudgets::calibrated();
        let hi = |lang: Lang, m: usize| cal.for_lang(lang).0[m].1.expect("calibrated");
        let cu = format!(
            "params p99 = {:.0} against C++'s {:.0}, magic numbers {:.0} against {:.0}, and length {:.0} against {:.0}",
            hi(Lang::Cuda, PARAMS),
            hi(Lang::Cpp, PARAMS),
            hi(Lang::Cuda, MAGIC_NUMBERS),
            hi(Lang::Cpp, MAGIC_NUMBERS),
            hi(Lang::Cuda, LENGTH),
            hi(Lang::Cpp, LENGTH),
        );
        assert!(readme.contains(&cu), "README drifted: {cu:?} missing");
    }

    #[test]
    fn the_readme_perl_era_numbers_cannot_drift_either() {
        // Four languages added at once, each with a quoted number the
        // prose draws a conclusion from. Same rule as everywhere else:
        // a number in the README is pinned to the calibration or it is
        // a claim nobody is checking.
        use crate::lang::Lang;
        let readme = include_str!("../../README.md").replace('\n', " ");
        let cal = LangBudgets::calibrated();
        let hi = |lang: Lang, m: usize| cal.for_lang(lang).0[m].1.expect("calibrated");
        let ruby = format!(
            "tightest function length in the corpus at {:.0}",
            hi(Lang::Ruby, LENGTH)
        );
        assert!(readme.contains(&ruby), "README drifted: {ruby:?} missing");
        let against = format!(
            "against OCaml's {:.0} and Python's {:.0}",
            hi(Lang::OCaml, LENGTH),
            hi(Lang::Python, LENGTH)
        );
        assert!(
            readme.contains(&against),
            "README drifted: {against:?} missing"
        );
        // The prose calls Ruby's the tightest; that is a claim about
        // every other language, so it is checked against every other
        // language rather than against the two it names.
        let tightest = crate::lang::LANGS
            .iter()
            .filter(|l| **l != Lang::Ruby)
            .all(|l| {
                cal.for_lang(*l).0[LENGTH]
                    .1
                    .is_none_or(|v| v > hi(Lang::Ruby, LENGTH))
            });
        assert!(
            tightest,
            "README calls Ruby's function length the tightest in the corpus; it no longer is"
        );
        let lua = format!("Lua reads cognitive p99 = {:.0}", hi(Lang::Lua, COGNITIVE));
        assert!(readme.contains(&lua), "README drifted: {lua:?} missing");
    }

    #[test]
    fn baked_calibration_actually_loads() {
        // calibrated() silently falls back to defaults on a parse failure;
        // this pins that the compiled-in snapshot really was applied.
        let cal = LangBudgets::calibrated();
        let def = Budgets::defaults();
        assert!(
            crate::lang::LANGS.iter().any(|l| *cal.for_lang(*l) != def),
            "calibration.toml parsed to defaults — snapshot broken"
        );
    }

    #[test]
    fn test_quality_flags_assertless_and_lazy_tests() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let facts = extract(
            pack,
            &mut parser,
            Path::new("test_thing.py"),
            "def test_1():\n    run()\n\ndef test_rejects_expired_token():\n    assert check() is False\n\ndef helper():\n    pass\n",
        );
        let mut hits: Vec<(usize, String, f32)> = Vec::new();
        for_each(&facts, |m, v, _, name| {
            if m == TEST_ASSERTS || m == LAZY_TEST_NAME {
                hits.push((m, name.to_string(), v));
            }
        });
        // test_1: 0 asserts (violates lo), lazy name; the good test passes
        // both; helper in a test file is test-support, not a named test.
        assert_eq!(
            hits,
            [
                (TEST_ASSERTS, "test_1".into(), 0.0),
                (LAZY_TEST_NAME, "test_1".into(), 1.0),
                (TEST_ASSERTS, "test_rejects_expired_token".into(), 1.0),
                (LAZY_TEST_NAME, "test_rejects_expired_token".into(), 0.0),
            ]
        );
    }

    #[test]
    fn lying_names_break_their_contract() {
        let pack = Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let facts = extract(
            pack,
            &mut parser,
            Path::new("t.rs"),
            "impl W {\n    fn is_ready(&self) -> usize { 1 }\n    fn is_done(&self) -> bool { true }\n    fn get_len(&mut self) -> usize { 0 }\n    fn get_cap(&self) -> usize { 0 }\n}\n",
        );
        let mut liars = Vec::new();
        for_each(&facts, |m, v, _, name| {
            if m == LYING_NAME && v > 0.0 {
                liars.push(name.to_string());
            }
        });
        assert_eq!(liars, ["W::is_ready", "W::get_len"]);

        // Go's `result` field feeds the same contract check.
        let pack = Lang::Go.pack();
        let mut parser = pack.make_parser();
        let facts = extract(
            pack,
            &mut parser,
            Path::new("t.go"),
            "package main\n\nfunc IsReady() int { return 1 }\n\nfunc IsDone() bool { return true }\n",
        );
        let mut liars = Vec::new();
        for_each(&facts, |m, v, _, name| {
            if m == LYING_NAME && v > 0.0 {
                liars.push(name.to_string());
            }
        });
        assert_eq!(liars, ["IsReady"]);
    }

    #[test]
    fn name_quality_flags_conjunctions_and_junk_vocabulary() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let facts = extract(
            pack,
            &mut parser,
            Path::new("t.py"),
            "def sync_and_upload():\n    pass\n\ndef process_data():\n    pass\n\ndef process_payment():\n    pass\n",
        );
        let mut flagged: Vec<(usize, String)> = Vec::new();
        for_each(&facts, |m, v, _, name| {
            if (m == AND_NAME || m == GENERIC_NAME) && v > 0.0 {
                flagged.push((m, name.to_string()));
            }
        });
        assert_eq!(
            flagged,
            [
                (AND_NAME, "sync_and_upload".into()),
                (GENERIC_NAME, "process_data".into())
            ]
        );

        // A declared test's name is a sentence: "and" is grammar there,
        // not a confession of two responsibilities.
        let mut parser = pack.make_parser();
        let tests = extract(
            pack,
            &mut parser,
            Path::new("tests/test_io.py"),
            "def test_parses_and_validates_the_header():\n    assert parse(b'x')\n",
        );
        let mut prose_hits = Vec::new();
        for_each(&tests, |m, v, _, name| {
            if (m == AND_NAME || m == GENERIC_NAME || m == ABBREVIATED) && v > 0.0 {
                prose_hits.push(name.to_string());
            }
        });
        assert!(prose_hits.is_empty(), "test prose flagged: {prose_hits:?}");
    }

    #[test]
    fn a_name_must_grow_with_the_distance_it_carries() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let mut spans = |src: &str| {
            let facts = extract(pack, &mut parser, Path::new("t.py"), src);
            let mut vals = Vec::new();
            for_each(&facts, |m, v, _, _| {
                if m == TERSE_NAME {
                    vals.push(v);
                }
            });
            vals
        };
        // `i` across three lines is clearer than `loop_index`.
        let brief = "def f(xs):\n    i = 0\n    i += 1\n    return i\n";
        assert_eq!(spans(brief), [0.0], "terse and short-lived is fine");

        // The same name across twenty is a register with no label.
        let mut far = String::from("def f(xs):\n    n = 0\n");
        for k in 0..20 {
            far.push_str(&format!("    step_{k}()\n"));
        }
        far.push_str("    return n\n");
        assert_eq!(spans(&far), [21.0], "the value is how far it carries");
    }

    #[test]
    fn crushed_vowels_are_flagged_but_real_words_are_not() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let facts = extract(
            pack,
            &mut parser,
            Path::new("t.py"),
            "def load_usr_cfg():\n    pass\n\ndef parse_url_id():\n    pass\n",
        );
        let mut hits = Vec::new();
        for_each(&facts, |m, v, _, name| {
            if m == ABBREVIATED && v > 0.0 {
                hits.push((name.to_string(), v));
            }
        });
        assert_eq!(
            hits,
            [("load_usr_cfg".to_string(), 2.0)],
            "url and id became words in their own right"
        );
    }

    #[test]
    fn synonym_split_finds_the_verb_and_its_object() {
        assert_eq!(
            split_synonym("fetch_user_profile"),
            Some(("fetch", "user_profile".to_string()))
        );
        assert_eq!(split_synonym("getUser"), Some(("get", "user".to_string())));
        // A bare verb acts on nothing nameable, and a non-synonym verb
        // has no other spelling to drift from.
        assert_eq!(split_synonym("get"), None);
        assert_eq!(split_synonym("render_page"), None);
    }

    #[test]
    fn an_assertion_on_a_literal_is_green_by_construction() {
        let vacuous = |lang: Lang, path: &str, src: &str| {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let facts = extract(pack, &mut parser, Path::new(path), src);
            let mut vals = Vec::new();
            for_each(&facts, |m, v, _, _| {
                if m == VACUOUS_ASSERTS {
                    vals.push(v);
                }
            });
            vals
        };
        assert_eq!(
            vacuous(
                Lang::Python,
                "test_x.py",
                "def test_placeholder():\n    assert True\n\ndef test_real():\n    assert compute() == 4\n",
            ),
            [1.0, 0.0],
            "a literal subject passes whatever the code did; a comparison does not"
        );
        assert_eq!(
            vacuous(
                Lang::Rust,
                "t.rs",
                "#[test]\nfn placeholder() {\n    assert!(true);\n}\n\n#[test]\nfn real() {\n    assert_eq!(compute(), 4);\n}\n",
            ),
            [1.0, 0.0],
            "a literal in SECOND position is an expected value, not a vacuous check"
        );
    }

    #[test]
    fn a_handler_that_forgets_the_cause_is_flagged() {
        let lost = |lang: Lang, path: &str, src: &str| {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let facts = extract(pack, &mut parser, Path::new(path), src);
            facts.units.iter().map(|u| u.lost_context).sum::<u16>()
        };
        assert_eq!(
            lost(
                Lang::Python,
                "a.py",
                "def f():\n    try:\n        g()\n    except ValueError as err:\n        raise Wrapped('nope')\n",
            ),
            1
        );
        assert_eq!(
            lost(
                Lang::Python,
                "a.py",
                "def f():\n    try:\n        g()\n    except ValueError as err:\n        raise Wrapped('nope') from err\n",
            ),
            0,
            "PEP 3134 chaining keeps the cause"
        );
        assert_eq!(
            lost(
                Lang::TypeScript,
                "a.ts",
                "function f() {\n  try { g(); } catch (e) { throw new Error('nope'); }\n}\n",
            ),
            1
        );
        assert_eq!(
            lost(
                Lang::TypeScript,
                "a.ts",
                "function f() {\n  try { g(); } catch (e) { throw new Error('nope', { cause: e }); }\n}\n",
            ),
            0,
            "ES2022 cause keeps the chain"
        );
    }

    #[test]
    fn a_credential_needs_both_a_promising_name_and_real_entropy() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let mut secrets = |src: &str| {
            let facts = extract(pack, &mut parser, Path::new("a.py"), src);
            facts.secrets.clone()
        };
        assert_eq!(
            secrets("API_KEY = \"sk9f3kd0asdf8812jjd\"\n"),
            [1],
            "a key-shaped value under a key-shaped name"
        );
        // The name promises a credential but the value is a field label.
        assert!(secrets("PASSWORD_FIELD = \"password\"\n").is_empty());
        // The value has entropy but the name promises nothing.
        assert!(secrets("BUILD_ID = \"sk9f3kd0asdf8812jjd\"\n").is_empty());
        // The shapes of code that reads a secret from somewhere else.
        assert!(secrets("API_KEY = os.environ[\"API_KEY\"]\n").is_empty());
        assert!(secrets("API_KEY = f\"{prefix}0000\"\n").is_empty());
        assert!(secrets("API_KEY = \"your-api-key-here-000\"\n").is_empty());
    }

    #[test]
    fn secrets_are_found_in_every_language_that_can_bind_one() {
        let secrets = |lang: Lang, path: &str, src: &str| {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            extract(pack, &mut parser, Path::new(path), src)
                .secrets
                .len()
        };
        // The same hardcoded password, wherever it is spelled. The
        // detector was structurally DEAD in Rust, Zig and C: nothing
        // anchored their binding kinds, so an identical leak fired in
        // Python and vanished in Rust.
        assert_eq!(
            secrets(Lang::Python, "a.py", "password = \"hunter2x9k2m44aa\"\n"),
            1
        );
        assert_eq!(
            secrets(
                Lang::Rust,
                "a.rs",
                "fn f() -> usize {\n    let password = \"hunter2x9k2m44aa\";\n    password.len()\n}\n",
            ),
            1,
            "rust"
        );
        assert_eq!(
            secrets(
                Lang::Zig,
                "a.zig",
                "const password = \"hunter2x9k2m44aa\";\n"
            ),
            1,
            "zig"
        );
        assert_eq!(
            secrets(
                Lang::C,
                "a.c",
                "static const char *password = \"hunter2x9k2m44aa\";\n",
            ),
            1,
            "c"
        );
        assert_eq!(
            secrets(
                Lang::Go,
                "a.go",
                "package p\n\nvar password = \"hunter2x9k2m44aa\"\n",
            ),
            1,
            "go var form"
        );
        assert_eq!(
            secrets(
                Lang::TypeScript,
                "a.ts",
                "const password = 'hunter2x9k2m44aa';\n"
            ),
            1,
            "ts"
        );
    }

    /// Join a vendor prefix to its payload at RUN time, so no source
    /// line ever holds a complete provider-shaped token. See the
    /// fixtures below for why that matters.
    fn vendor(prefix: &str, payload: &str) -> String {
        format!("{prefix}{payload}")
    }

    #[test]
    fn a_connection_string_carries_its_password_in_a_named_position() {
        // The name promises nothing — DATABASE_URL is honestly a URL —
        // but the scheme defines where the credential sits, which is
        // the vendor-prefix argument in another spelling.
        assert_eq!(
            secrets(&format!(
                "DATABASE_URL = \"postgres://admin:{}@db:5432/prod\"\n",
                vendor("s3cr3t", "9xKfQ2")
            )),
            1
        );
        assert_eq!(
            secrets("DATABASE_URL = \"postgres://admin@db:5432/prod\"\n"),
            0,
            "no password, no credential"
        );
        assert_eq!(
            secrets("DATABASE_URL = \"postgres://admin:${DB_PASS}@db/prod\"\n"),
            0,
            "an interpolated password is the remedy, not the smell"
        );
        assert_eq!(
            secrets("DATABASE_URL = \"postgres://user:password@localhost/db\"\n"),
            0,
            "a lowercase word is documentation"
        );
        assert_eq!(
            secrets("DATABASE_URL = \"postgres://user:changeme123@localhost/db\"\n"),
            0,
            "the placeholder rules still govern"
        );
        assert_eq!(
            secrets("HOMEPAGE = \"https://example.com/a:b@c\"\n"),
            0,
            "a colon and an at-sign in a PATH are not userinfo"
        );
        assert_eq!(
            secrets("REDIS_URL = \"redis://:a1@localhost\"\n"),
            0,
            "too short to be worth a release to rotate"
        );
        // Prose that quotes a URL is not a connection string: an
        // authority holds no whitespace. A tweet in trpc's gold corpus
        // parsed its own words as userinfo before this rule.
        assert_eq!(
            secrets(
                "QUOTE = \"impressed by @alexdotjs http://trpc.io: end-to-end safety is awesome in 2024\"\n"
            ),
            0,
            "a sentence quoting a URL is a sentence"
        );
    }

    /// Total `ceremony` findings in one source, in any language.
    fn ceremony_in(lang: crate::lang::Lang, name: &str, src: &str) -> u32 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(name), src);
        let mut total = 0.0f32;
        super::for_each(&f, |m, v, _, _| {
            if m == CEREMONY {
                total += v;
            }
        });
        total as u32
    }

    #[test]
    fn ceremony_needs_the_documentation_and_spares_the_override() {
        use crate::lang::Lang;
        const DOC: &str = "/// Whether the fast path is available.\n///\n/// Three lines.\n";

        // FIRES: nullary, one bare literal, documented at length.
        assert_eq!(
            ceremony_in(
                Lang::Rust,
                "a.rs",
                &format!("{DOC}pub fn ready() -> bool {{ true }}\n")
            ),
            1,
        );
        assert_eq!(
            ceremony_in(
                Lang::Python,
                "a.py",
                "def ready():\n    \"\"\"Whether ready.\n\n    Three lines.\n    \"\"\"\n    return True\n"
            ),
            1,
        );

        // SILENT — and each of these is why the rule has the shape it
        // has, measured against 512,927 human declarations.

        // Bare content-free-ness is COMMONER in admired code than in the
        // corpus this was built from. Only the documentation separates.
        assert_eq!(
            ceremony_in(Lang::Rust, "a.rs", "pub fn ready() -> bool { true }\n"),
            0,
            "an undocumented stub is not the defect",
        );

        // A trait default is documented at length so implementors know
        // when to replace it. This shape produced all 79 gold false
        // positives.
        assert_eq!(
            ceremony_in(
                Lang::Rust,
                "a.rs",
                &format!("trait Bounded {{\n    {DOC}    fn min_len(&self) -> usize {{ 1 }}\n}}\n"),
            ),
            0,
            "a documented trait default is documented on purpose",
        );

        // A body that does anything at all.
        assert_eq!(
            ceremony_in(
                Lang::Rust,
                "a.rs",
                &format!("{DOC}pub fn ready() -> bool {{ check() }}\n")
            ),
            0,
        );

        // A function taking arguments does something with them.
        assert_eq!(
            ceremony_in(
                Lang::Rust,
                "a.rs",
                &format!("{DOC}pub fn ready(x: u32) -> bool {{ true }}\n")
            ),
            0,
        );

        // A stub returning `true` is how a fixture is written.
        assert_eq!(
            ceremony_in(
                Lang::Rust,
                "crate/tests/a.rs",
                &format!("{DOC}pub fn ready() -> bool {{ true }}\n")
            ),
            0,
            "test code declares stubs for a living",
        );
    }

    fn secrets(src: &str) -> usize {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new("a.py"), src)
            .secrets
            .len()
    }

    #[test]
    fn aws_keys_fire_paths_do_not_and_vendor_prefixes_need_no_name() {
        // AWS secret keys are base64 WITH slashes; the old blanket '/'
        // exclusion made the #1 cloud provider's format invisible.
        assert_eq!(
            secrets("AWS_SECRET_ACCESS_KEY = \"wJalrXUtnFEMI/K7MDENG/bPxRfiCYZQRSTKEYQQ\"\n"),
            1,
            "base64-with-slashes under a promising name"
        );
        // A path is lowercase words, not a key — even under the name.
        assert_eq!(secrets("PRIVATE_KEY_PATH = \"keys/prod/signing2\"\n"), 0);
        // Vendor-prefixed values identify THEMSELVES: no name needed.
        // Every fixture below is ASSEMBLED rather than written out, and
        // that is not squeamishness — a literal carrying a provider's
        // exact shape trips the scanners that read THIS repository,
        // starting with GitHub's own push protection. A tool that
        // detects credentials has no business shipping decoys that make
        // everyone else's detector cry wolf. The assembled string is
        // byte-identical, so the test judges exactly what it claims to.
        assert_eq!(
            secrets(&format!(
                "BUILD_ID = \"{}\"\n",
                vendor("ghp_", "16C7e42F292c6912E7710c838347Ae178B4a")
            )),
            1,
            "github token under an innocent name"
        );
        assert_eq!(
            secrets(&format!(
                "REGION_TAG = \"{}\"\n",
                vendor("AKIA", "J4KF7EXAMPLEQQZQ")
            )),
            0,
            "the canonical doc key is a placeholder"
        );
        assert_eq!(
            secrets(&format!(
                "REGION_TAG = \"{}\"\n",
                vendor("AKIA", "J4KF0PQRSTUVWZQQ")
            )),
            1,
            "a real-shaped AWS key id fires nameless"
        );
        assert_eq!(
            secrets(&format!(
                "pem = \"{}\"\n",
                vendor(
                    "-----BEGIN RSA PRIVATE KEY-----",
                    "MIIEcm9vdDpwYXNzd29yZAo3q1x9v2k4m8pQrs11"
                )
            )),
            1,
            "a PEM block CARRYING key material is a private key anywhere"
        );
        // The bare armor line is a marker, not a key: curl's examples
        // build a placeholder PEM from separate string literals.
        assert_eq!(secrets("pem = \"-----BEGIN PRIVATE KEY-----\"\n"), 0);
        // Vendor placeholders from docs stay silent.
        assert_eq!(secrets("t = \"ghp_exampleexample00\"\n"), 0);
        // A twenty-char value without a digit is a NAME for a secret:
        // localStorage keys and component names, straight off the gold
        // corpus's false positives.
        assert_eq!(secrets("OAI_API_KEY = \"excalidraw-oai-api-key\"\n"), 0);
        assert_eq!(
            secrets("ONE_TIME_PASSWORD_FIELD = \"OneTimePasswordField\"\n"),
            0
        );
        // A slug is short words wearing separators, whatever its name
        // promises — playwright maps "libsecret-1.so.0" to the package
        // id "libsecret-1-0", and four such rows read as credentials.
        assert_eq!(secrets("libsecret_key = \"libsecret-1-0\"\n"), 0);
        assert_eq!(secrets("secret_pkg = \"production-db-primary\"\n"), 0);
        // A UUID-format credential still fires: one group is twelve.
        assert_eq!(
            secrets("API_KEY = \"550e8400-e29b-41d4-a716-446655440000\"\n"),
            1
        );
    }

    #[test]
    fn client_sdk_configs_publish_their_keys_by_design() {
        let secrets = |src: &str| {
            let pack = Lang::TypeScript.pack();
            let mut parser = pack.make_parser();
            extract(pack, &mut parser, Path::new("cfg.ts"), src)
                .secrets
                .len()
        };
        // Algolia search and Firebase web configs ship apiKey to every
        // browser on purpose; the appId/indexName siblings are the shape
        // that says so. Flagging every docs site teaches readers to
        // ignore the metric.
        assert_eq!(
            secrets(
                "const cfg = { appId: 'BTGPSR4MOE', apiKey: 'ed8b3896f8e3e2b421e4c38834b915a8', indexName: 'trpc' };\n"
            ),
            0,
            "publishable client config"
        );
        // The same key WITHOUT the client-config shape is a finding.
        assert_eq!(
            secrets("const cfg = { apiKey: 'ed8b3896f8e3e2b421e4c38834b915a8' };\n"),
            1,
            "a lone apiKey pair stays a secret"
        );
    }

    fn blocking(lang: Lang, path: &str, src: &str) -> u16 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let facts = extract(pack, &mut parser, Path::new(path), src);
        facts.units.iter().map(|u| u.blocking_calls).sum()
    }

    #[test]
    fn a_blocking_call_only_counts_inside_an_async_unit() {
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "import time\n\nasync def handler():\n    time.sleep(1)\n",
            ),
            1
        );
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "import time\n\ndef worker():\n    time.sleep(1)\n",
            ),
            0,
            "a synchronous worker is entitled to park its own thread"
        );
        assert_eq!(
            blocking(
                Lang::Rust,
                "a.rs",
                "async fn handler() {\n    std::thread::sleep(d);\n}\n\nfn worker() {\n    std::thread::sleep(d);\n}\n",
            ),
            1
        );
        assert_eq!(
            blocking(
                Lang::TypeScript,
                "a.ts",
                "export async function handler() {\n  const d = fs.readFileSync(p);\n  return d;\n}\n",
            ),
            1,
            "Node's Sync family is named after the problem"
        );
    }

    #[test]
    fn sync_io_is_judged_by_its_qualifier_never_its_receiver() {
        // The sync-HTTP ecosystems, by qualified spelling. A client
        // held in a VARIABLE is never judged: no name decides whether
        // `client.get` is httpx.Client or httpx.AsyncClient.
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "async def fetch(url):\n    return requests.get(url)\n",
            ),
            1
        );
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "async def fetch(url):\n    resp = await client.get(url)\n    return resp\n",
            ),
            0,
            "a variable receiver is undecidable and stays unjudged"
        );
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "def fetch(url):\n    return requests.get(url)\n",
            ),
            0,
            "sync code is entitled to sync HTTP"
        );
        // Bare open() blocks inside async; aiofiles.open (two
        // segments) and path.open() (variable receiver) stay silent.
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "async def load(p):\n    f = open(p)\n    g = aiofiles.open(p)\n    h = p.open()\n    return f, g, h\n",
            ),
            1
        );
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "async def load(url):\n    return urllib.request.urlopen(url)\n",
            ),
            1,
            "urlopen names nothing else in the ecosystem"
        );
        // Rust: std::fs spells its qualifier; tokio::fs spells ITS
        // qualifier and is the remedy — the asyncio.sleep lesson.
        assert_eq!(
            blocking(
                Lang::Rust,
                "a.rs",
                "async fn load() {\n    let a = std::fs::read_to_string(p);\n    let b = tokio::fs::read_to_string(p).await;\n}\n",
            ),
            1
        );
        // tokio's blocking_* family exists FOR sync contexts and
        // panics inside a runtime.
        assert_eq!(
            blocking(
                Lang::Rust,
                "a.rs",
                "async fn drain(rx: &mut Receiver<u8>) {\n    let v = rx.blocking_recv();\n    drop(v);\n}\n",
            ),
            1
        );
    }

    #[test]
    fn the_runtimes_own_sleep_is_the_fix_not_the_bug() {
        // Judging the trailing name alone flagged every `sleep` — which
        // condemned exactly the correct pattern: 26/26 findings on a
        // production FastAPI backend were `await asyncio.sleep(...)`.
        assert_eq!(
            blocking(
                Lang::Python,
                "a.py",
                "import asyncio\n\nasync def poller():\n    await asyncio.sleep(0.1)\n",
            ),
            0,
            "asyncio.sleep yields"
        );
        assert_eq!(
            blocking(
                Lang::Rust,
                "a.rs",
                "async fn poll_loop() {\n    tokio::time::sleep(std::time::Duration::from_millis(5)).await;\n}\n",
            ),
            0,
            "tokio::time::sleep is the runtime's own sleep"
        );
        assert_eq!(
            blocking(
                Lang::TypeScript,
                "a.ts",
                "const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));\n\nexport async function retryLoop() {\n  await sleep(100);\n}\n",
            ),
            0,
            "a promise-based sleep helper parks nothing"
        );
    }

    #[test]
    fn a_resource_needs_a_guard_only_where_guards_are_the_idiom() {
        let counts = |lang: Lang, src: &str| {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new("a"), src);
            let unmanaged: u16 = f.units.iter().map(|u| u.unmanaged).sum();
            let dropped: u16 = f.units.iter().map(|u| u.dropped_tasks).sum();
            (unmanaged, dropped)
        };
        assert_eq!(
            counts(
                Lang::Python,
                "def f(p):\n    h = open(p)\n    return h.read()\n"
            ),
            (1, 0)
        );
        assert_eq!(
            counts(
                Lang::Python,
                "def f(p):\n    with open(p) as h:\n        return h.read()\n"
            ),
            (0, 0),
            "the block's exit closes it however the block ends"
        );
        assert_eq!(
            counts(
                Lang::Rust,
                "fn f(p: &str) {\n    let h = File::open(p);\n}\n"
            ),
            (0, 0),
            "RAII: a File closes when it drops, so there is no guard to omit"
        );
        assert_eq!(
            counts(
                Lang::Rust,
                "fn f() {\n    tokio::spawn(work());\n    let h = tokio::spawn(other());\n}\n"
            ),
            (0, 1),
            "the bare statement drops its handle; the binding keeps it"
        );
    }

    #[test]
    fn the_type_system_is_opted_out_of_one_construct_at_a_time() {
        let pack = Lang::Rust.pack();
        let flagged = |src: &str, want: usize| {
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new("a.rs"), src);
            let mut hits = Vec::new();
            for_each(&f, |m, v, _, name| {
                if m == want && v > 0.0 {
                    hits.push(name.to_string());
                }
            });
            hits
        };
        assert_eq!(
            flagged(
                "fn exhaustive(k: Kind) -> u8 {\n    match k { Kind::A => 1, Kind::B => 2, Kind::C => 3 }\n}\n\nfn lazy(k: Kind) -> u8 {\n    match k { Kind::A => 1, Kind::B => 2, _ => 0 }\n}\n",
                WILDCARD_MATCH,
            ),
            ["lazy"],
            "adding a variant breaks the exhaustive one and silently passes the other"
        );
        assert_eq!(
            flagged(
                "fn fetch(user_id: &str, order_id: OrderId, n: u32) -> u8 { 0 }\n",
                STRINGLY_ID,
            ),
            ["fetch"],
            "a newtype makes the mixup unrepresentable; &str does not"
        );
    }

    #[test]
    fn loop_nesting_is_arithmetic_and_copies_inside_loops_are_named() {
        let pack = Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("a.rs"),
            "fn cubic(g: &Grid, names: &[String]) -> usize {\n    let mut n = 0;\n    for a in g.rows() {\n        for b in a.cells() {\n            for c in b.parts() {\n                n += c;\n            }\n        }\n    }\n    for name in names {\n        let owned = name.to_string();\n        n += owned.len();\n    }\n    n\n}\n\nfn flat(xs: &[u8]) -> usize {\n    let mut n = 0;\n    for x in xs {\n        n += *x as usize;\n    }\n    n\n}\n",
        );
        let depths: Vec<u16> = f.units[1..].iter().map(|u| u.max_loop_depth).collect();
        let allocs: Vec<u16> = f.units[1..].iter().map(|u| u.allocs_in_loop).collect();
        assert_eq!(depths, [3, 1], "three nested loops is cubic; one is linear");
        assert_eq!(allocs, [1, 0], "one copy per iteration, and none");
    }

    #[test]
    fn spelling_is_classified_and_a_single_style_scores_zero_entropy() {
        use Case::*;
        for (name, want) in [
            ("parse_header", Snake),
            ("parseHeader", Camel),
            ("ParseHeader", Pascal),
            ("MAX_RETRIES", Screaming),
            // A lowercase word carries no evidence of a style; a
            // capitalized one does, since snake would have left it down.
            ("run", Neutral),
            ("Run", Pascal),
        ] {
            assert_eq!(case_of(name), want, "{name}");
        }
        assert_eq!(idiom_entropy(&[100, 0, 0, 0]), 0.0, "one style throughout");
        assert_eq!(idiom_entropy(&[0, 0, 0, 0]), 0.0, "nothing measured");
        assert_eq!(
            idiom_entropy(&[25, 25, 25, 25]),
            1.0,
            "every spelling equally likely"
        );
        let mostly = idiom_entropy(&[90, 10, 0, 0]);
        assert!(mostly > 0.0 && mostly < 0.5, "a dominant style with a tail");
    }

    fn hooks(lang: Lang, path: &str, src: &str) -> u16 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(path), src);
        f.units.iter().map(|u| u.conditional_hooks).sum()
    }

    fn arity(lang: Lang, path: &str, src: &str) -> u16 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(path), src);
        f.units.iter().map(|u| u.return_arity).max().unwrap_or(0)
    }

    fn repurposed(lang: Lang, path: &str, src: &str) -> u16 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(path), src);
        f.units.iter().map(|u| u.repurposed).sum()
    }

    #[test]
    fn a_types_own_attributes_are_not_rewrites_of_each_other() {
        // A class body declares attributes OF A TYPE, and a type body
        // opens no unit, so every class in a file shares the module's
        // live map. click's `name = "integer"` and `name = "boolean"`
        // are two classes' own attributes, not one rewriting the other
        // — ten such pairs in one file before this landed.
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "class IntType:\n    name = \"integer\"\n\nclass BoolType:\n    name = \"boolean\"\n"
            ),
            0,
            "same attribute name, different types"
        );
        // A method body is a scope of its own, so the rule stops there
        // and an ordinary rewrite inside one still fires.
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "class T:\n    def run(self):\n        v = first()\n        emit(v)\n        v = second()\n        return v\n"
            ),
            1
        );
    }

    #[test]
    fn a_declaration_is_not_a_rewrite_however_the_grammar_spells_it() {
        // Zig spells a declaration and a write with the SAME node kind,
        // so POSITION decides: a fresh binding states `var` or `const`
        // first, and its name therefore cannot start where the node
        // starts. Six sibling-scope `const run = ...` declarations in
        // ghostty read as five repurposings until this landed.
        assert_eq!(
            repurposed(
                Lang::Zig,
                "a.zig",
                "fn f(b: *Build) void {\n    {\n        const run = b.addRun();\n        run.step();\n    }\n    {\n        const run = b.addRun();\n        run.step();\n    }\n}\n"
            ),
            0,
            "sibling-scope declarations are new bindings, not rewrites"
        );
        // And the rule stays narrow: a bare write in the same grammar,
        // same kind, still fires.
        assert_eq!(
            repurposed(
                Lang::Zig,
                "a.zig",
                "fn f() void {\n    var label = short();\n    emit(label);\n    label = full();\n}\n"
            ),
            1,
            "the bare write still fires"
        );
    }

    #[test]
    fn repurposing_fires_on_a_straight_line_second_meaning() {
        // The finding: the same name, a second meaning, in a straight
        // line — every earlier read the reader remembers is now wrong.
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "def f(row):\n    total = subtotal(row)\n    report(total)\n    total = grand(row)\n    return total\n"
            ),
            1
        );
        assert_eq!(
            repurposed(
                Lang::Go,
                "a.go",
                "func f() int {\n\tx := load()\n\tuse(x)\n\tx = fallback()\n\treturn x\n}\n"
            ),
            1
        );
        assert_eq!(
            repurposed(
                Lang::Rust,
                "a.rs",
                "fn f() -> u32 {\n    let mut acc = seed();\n    use_it(acc);\n    acc = other();\n    acc\n}\n"
            ),
            1
        );
        // Shell: a plain refill fires the same way.
        assert_eq!(
            repurposed(
                Lang::Shell,
                "a.sh",
                "deploy() {\n  local region=\"eu\"\n  push \"$region\"\n  region=\"us\"\n  push \"$region\"\n}\n"
            ),
            1
        );
        // And at a script's TOP LEVEL, which is the whole program in
        // shell and where a reset region deploys to the wrong place.
        assert_eq!(
            repurposed(
                Lang::Shell,
                "deploy.sh",
                "REGION=\"us-east-1\"\napply \"$REGION\"\nREGION=\"eu-north-1\"\napply \"$REGION\"\n"
            ),
            1,
            "a script IS its module scope"
        );
    }

    #[test]
    fn repurposing_spares_collectors_overrides_and_sheltered_fills() {
        // A collecting update mentions the old value: one meaning,
        // transformed. Compound operators are the same statement.
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "def f(items):\n    s = start()\n    s = s.strip()\n    s += tail()\n    return s\n"
            ),
            0
        );
        // A conditional override CHOOSES a value; the meaning holds.
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "def f(fast):\n    timeout = DEFAULT\n    if fast:\n        timeout = 1\n    return timeout\n"
            ),
            0
        );
        // A try-sheltered fill: the old value survives the handler
        // path, so it is a guard, not a second meaning.
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "def f(p):\n    data = None\n    try:\n        data = fetch(p)\n    except OSError:\n        log(p)\n    return data\n"
            ),
            0
        );
        // A loop body refills its variable per iteration by design.
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "def f(rows):\n    out = None\n    for r in rows:\n        out = judge(r)\n    return out\n"
            ),
            0
        );
        // A swap writes two targets; state mutation through a member
        // is not a rebinding; a first assignment has no old meaning.
        assert_eq!(
            repurposed(
                Lang::Go,
                "a.go",
                "func f() {\n\ta, b := 1, 2\n\ta, b = b, a\n\tuse(a, b)\n}\n"
            ),
            0
        );
        assert_eq!(
            repurposed(
                Lang::Python,
                "a.py",
                "def f(self, v):\n    self.mode = 1\n    self.mode = v\n"
            ),
            0
        );
        // A deref target writes THROUGH the pointer; nothing is
        // rebound. This repository's own set_switch was the first
        // false positive: `let slot = match ...; *slot = true`.
        assert_eq!(
            repurposed(
                Lang::Rust,
                "a.rs",
                "fn f(flag: bool, args: &mut Args) {\n    let slot = match flag {\n        true => &mut args.a,\n        false => &mut args.b,\n    };\n    *slot = true;\n}\n"
            ),
            0
        );
        // A shadowing `let` is a NEW binding — Rust's own remedy for
        // repurposing — and judging it without scopes would flag
        // sibling blocks.
        assert_eq!(
            repurposed(
                Lang::Rust,
                "a.rs",
                "fn f() {\n    let x = 1;\n    use_it(x);\n    let x = other();\n    use_it(x);\n}\n"
            ),
            0
        );
        // Shell `+=` appends: a collecting update in one token.
        assert_eq!(
            repurposed(
                Lang::Shell,
                "a.sh",
                "deploy() {\n  local opts=\"-v\"\n  opts+=\" --force\"\n  push \"$opts\"\n}\n"
            ),
            0
        );
    }

    fn shelled(lang: Lang, path: &str, src: &str) -> usize {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), src)
            .shelled_out
            .len()
    }

    #[test]
    fn an_argument_list_needs_no_shell_and_stays_silent() {
        // A value spliced into a command the shell will re-parse.
        assert_eq!(
            shelled(
                Lang::Python,
                "a.py",
                "def backup(path):\n    subprocess.run(f\"tar czf out.tgz {path}\", shell=True)\n"
            ),
            1
        );
        assert_eq!(
            shelled(
                Lang::Python,
                "a.py",
                "def backup(path):\n    os.system(f\"rm -rf {path}\")\n"
            ),
            1
        );
        assert_eq!(
            shelled(
                Lang::TypeScript,
                "a.ts",
                "export function backup(p: string) {\n  exec(`tar czf out.tgz ${p}`);\n}\n"
            ),
            1
        );
        // A QUERY IS NOT A COMMAND. `exec` is in the shell list for
        // Node's child_process and is also how half the world runs SQL,
        // so `db.exec(f"SELECT ...")` was reported as a shelled-out
        // command as well as a built query — two different accusations
        // about two different attack surfaces, one of them wrong. Found
        // by the editor hook firing twice on one line.
        assert_eq!(
            shelled(
                Lang::Python,
                "a.py",
                "def lookup(db, t):\n    return db.exec(f\"SELECT * FROM {t}\")\n"
            ),
            0,
            "a SQL verb is not a command; built query still reports the line"
        );
        assert_eq!(
            shelled(
                Lang::TypeScript,
                "a.ts",
                "export function lookup(db: Db, t: string) {\n  return db.exec(`INSERT INTO ${t} VALUES (1)`);\n}\n"
            ),
            0
        );
        // THE REMEDY: an argument list needs no shell, so nothing is
        // re-parsed and there is no interpolation to see.
        assert_eq!(
            shelled(
                Lang::Python,
                "a.py",
                "def backup(path):\n    subprocess.run([\"tar\", \"czf\", \"out.tgz\", path])\n"
            ),
            0
        );
        // shell=True with a LITERAL command is a style choice:
        // nothing untrusted reaches the parser.
        assert_eq!(
            shelled(
                Lang::Python,
                "a.py",
                "def sync():\n    subprocess.run(\"git fetch --all\", shell=True)\n"
            ),
            0
        );
        // execFile takes a program and a list; it never reaches one.
        assert_eq!(
            shelled(
                Lang::TypeScript,
                "a.ts",
                "export function backup(p: string) {\n  execFile('tar', ['czf', 'out.tgz', p]);\n}\n"
            ),
            0
        );
        // A LIST is the remedy even when one element is built, and
        // even when the method is called exec: vscode's git wrapper
        // spells it exec(['stash', 'list', `--format=${F}`, '-z']).
        assert_eq!(
            shelled(
                Lang::TypeScript,
                "a.ts",
                "export function stashes(g: Git, f: string) {\n  return g.exec(['stash', 'list', `--format=${f}`, '-z']);\n}\n"
            ),
            0,
            "an argument list reaches no shell, whatever the method is called"
        );
    }

    fn built(lang: Lang, path: &str, src: &str) -> usize {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), src)
            .sql_built
            .len()
    }

    #[test]
    fn a_parameterized_query_is_the_remedy_and_stays_silent() {
        // A statement assembled from a value: the oldest vulnerability
        // there is.
        assert_eq!(
            built(
                Lang::Python,
                "a.py",
                "def find(db, table):\n    return db.execute(f\"SELECT * FROM {table} WHERE id = 1\")\n"
            ),
            1
        );
        assert_eq!(
            built(
                Lang::TypeScript,
                "a.ts",
                "export function find(db: DB, id: string) {\n  return db.query(`SELECT * FROM users WHERE id = ${id}`);\n}\n"
            ),
            1
        );
        assert_eq!(
            built(
                Lang::Go,
                "a.go",
                "func find(db *DB, t string) *Rows {\n\treturn db.Query(fmt.Sprintf(\"SELECT * FROM %s\", t))\n}\n"
            ),
            1
        );
        // THE REMEDY. A parameterized query carries no interpolation
        // at all, so the metric cannot see it — which is the point.
        assert_eq!(
            built(
                Lang::Python,
                "a.py",
                "def find(db, id):\n    return db.execute(\"SELECT * FROM users WHERE id = %s\", (id,))\n"
            ),
            0
        );
        assert_eq!(
            built(
                Lang::TypeScript,
                "a.ts",
                "export function find(db: DB, id: string) {\n  return db.query('SELECT * FROM users WHERE id = $1', [id]);\n}\n"
            ),
            0
        );
        // A fully literal query is safe whatever it says.
        assert_eq!(
            built(
                Lang::Python,
                "a.py",
                "def all_users(db):\n    return db.execute(\"SELECT * FROM users\")\n"
            ),
            0
        );
        // Anchored: prose that mentions a verb is prose. An
        // interpolated log line is not a statement.
        assert_eq!(
            built(
                Lang::Python,
                "a.py",
                "def log_it(n):\n    print(f\"about to select {n} rows and update the cache\")\n"
            ),
            0
        );
        // Tests build queries to exercise the builder.
        assert_eq!(
            built(
                Lang::Python,
                "tests/test_a.py",
                "def test_builds(table):\n    assert q(f\"SELECT * FROM {table}\") == 1\n"
            ),
            0
        );
    }

    fn groups(lang: Lang, path: &str, src: &str) -> Vec<(String, u16)> {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), src)
            .classes
            .iter()
            .map(|c| (c.name.to_string(), c.groups))
            .collect()
    }

    #[test]
    fn methods_that_share_a_field_or_call_each_other_are_one_object() {
        // Two methods over one field: one object.
        assert_eq!(
            groups(
                Lang::Python,
                "a.py",
                "class Store:\n    def get(self, k):\n        return self.items[k]\n    def put(self, k, v):\n        self.items[k] = v\n"
            ),
            [("Store".to_string(), 1)]
        );
        // Two methods over two fields, never speaking: two objects
        // sharing a name, and Extract Class is the remedy.
        assert_eq!(
            groups(
                Lang::Python,
                "a.py",
                "class Both:\n    def read(self):\n        return self.cache\n    def log(self, m):\n        self.sink.write(m)\n"
            ),
            [("Both".to_string(), 2)]
        );
        // A call through the receiver connects them: that is what a
        // self-call looks like to a syntax tree, and regex's LookSet
        // read as eleven groups until this counted.
        assert_eq!(
            groups(
                Lang::Rust,
                "a.rs",
                "impl LookSet {\n    fn len(self) -> usize {\n        self.bits.count_ones() as usize\n    }\n    fn is_empty(self) -> bool {\n        self.len() == 0\n    }\n}\n"
            ),
            [("LookSet".to_string(), 1)]
        );
        // Rust spells `self` with its own node kind, so every
        // self.field in the language was invisible before this.
        assert_eq!(
            groups(
                Lang::Rust,
                "a.rs",
                "impl Store {\n    fn get(&self) -> u8 {\n        self.items[0]\n    }\n    fn put(&mut self, v: u8) {\n        self.items.push(v);\n    }\n}\n"
            ),
            [("Store".to_string(), 1)]
        );
        // A helper touching no state is a free function living in a
        // class, not an island of its own.
        assert_eq!(
            groups(
                Lang::Python,
                "a.py",
                "class Store:\n    def get(self, k):\n        return self.items[k]\n    def put(self, k, v):\n        self.items[k] = v\n    def slug(self, s):\n        return s.lower()\n"
            ),
            [("Store".to_string(), 1)]
        );
        // One stateful method is trivially cohesive — no question to
        // ask, so no finding to make.
        assert_eq!(
            groups(
                Lang::Python,
                "a.py",
                "class One:\n    def get(self, k):\n        return self.items[k]\n"
            ),
            []
        );
    }

    fn commented(lang: Lang, path: &str, src: &str) -> usize {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), src)
            .commented_code
            .len()
    }

    #[test]
    fn prose_that_happens_to_parse_is_still_prose() {
        // The smell: statements someone commented out instead of
        // deleting, which version control was already remembering.
        assert_eq!(
            commented(
                Lang::Rust,
                "a.rs",
                "fn f(&mut self) {\n    // let lower = self.ranges[i - 1].upper();\n    // let upper = self.ranges[i].lower();\n    // self.ranges.push(I::create(lower, upper));\n    self.ranges.push(other());\n}\n"
            ),
            1
        );
        // One line is a note, and a single statement is as often an
        // example as an abandonment.
        assert_eq!(
            commented(
                Lang::Rust,
                "a.rs",
                "fn f(&mut self) {\n    // self.ranges.push(other());\n    self.ranges.push(other());\n}\n"
            ),
            0
        );
        // Prose is the common case and must never parse as code.
        assert_eq!(
            commented(
                Lang::Rust,
                "a.rs",
                "fn f() {\n    // The target may be within a namespace, so the\n    // symbol has to be resolved before it can be used.\n    g();\n}\n"
            ),
            0
        );
        // A doc comment is DOCUMENTATION whatever it holds: an example
        // in a docstring is the point of the docstring.
        assert_eq!(
            commented(
                Lang::Rust,
                "a.rs",
                "/// Usage:\n/// let x = build();\n/// let y = x.run();\nfn build() {}\n"
            ),
            0
        );
        assert_eq!(
            commented(
                Lang::Python,
                "a.py",
                "def f(runner, cmd):\n    # result = runner.invoke(cmd, \"-a c\")\n    # assert result.output == \"ok\"\n    result = runner.invoke(cmd)\n    return result\n"
            ),
            1
        );
    }

    fn traps(lang: Lang, path: &str, src: &str) -> u16 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(path), src);
        f.units.iter().map(|u| u.bool_traps).sum()
    }

    #[test]
    fn a_named_argument_is_not_a_trap_however_many_booleans_follow() {
        // Nothing at the call site says which is which, and swapping
        // them still type-checks.
        assert_eq!(
            traps(
                Lang::Python,
                "a.py",
                "def f():\n    move(node, True, False)\n"
            ),
            1
        );
        assert_eq!(
            traps(
                Lang::TypeScript,
                "a.ts",
                "function f() {\n  move(node, true, false);\n}\n"
            ),
            1
        );
        // One is often a legitimate `force` flag and reads fine.
        assert_eq!(
            traps(Lang::Python, "a.py", "def f():\n    move(node, True)\n"),
            0
        );
        // Naming it at the call site IS the remedy this metric asks
        // for, so the remedy must not read as the disease.
        assert_eq!(
            traps(
                Lang::Python,
                "a.py",
                "def f():\n    move(node, strict=True, dry_run=False)\n"
            ),
            0
        );
        // A variable carries its meaning in its name.
        assert_eq!(
            traps(
                Lang::Python,
                "a.py",
                "def f(strict, dry_run):\n    move(node, strict, dry_run)\n"
            ),
            0
        );
    }

    fn skips(lang: Lang, path: &str, src: &str) -> usize {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), src)
            .skipped_tests
            .len()
    }

    #[test]
    fn only_an_unconditional_skip_is_a_suppression() {
        // The suite reports green and nothing records what the test
        // would have said — a suppression wearing a test's name.
        assert_eq!(
            skips(
                Lang::Rust,
                "a.rs",
                "#[test]\n#[ignore]\nfn slow_path() {\n    assert!(check());\n}\n"
            ),
            1
        );
        assert_eq!(
            skips(
                Lang::TypeScript,
                "a.test.ts",
                "it.skip('handles retries', () => {\n  expect(f()).toBe(1);\n});\nxit('other', () => {});\n"
            ),
            2
        );
        assert_eq!(
            skips(
                Lang::Go,
                "a_test.go",
                "func TestSlow(t *testing.T) {\n\tt.Skip(\"flaky\")\n\tcheck(t)\n}\n"
            ),
            1
        );
        assert_eq!(
            skips(
                Lang::Python,
                "tests/test_a.py",
                "@pytest.mark.skip(reason=\"broken\")\ndef test_slow():\n    assert check()\n"
            ),
            1
        );
        // A CONDITIONAL skip is stated judgment: the test still runs
        // where it applies, and nothing was silenced.
        assert_eq!(
            skips(
                Lang::Python,
                "tests/test_a.py",
                "@pytest.mark.skipif(sys.platform == 'win32', reason=\"posix only\")\ndef test_slow():\n    assert check()\n"
            ),
            0
        );
        assert_eq!(
            skips(
                Lang::Go,
                "a_test.go",
                "func TestSlow(t *testing.T) {\n\tif runtime.GOOS == \"windows\" {\n\t\tt.Skip(\"posix only\")\n\t}\n\tcheck(t)\n}\n"
            ),
            0,
            "a guarded skip is a platform decision"
        );
        // Rust spells the conditional form inside the attribute, and
        // rayon carries dozens: the test runs wherever the predicate
        // is false, so nothing was silenced.
        assert_eq!(
            skips(
                Lang::Rust,
                "a.rs",
                "#[test]\n#[cfg_attr(not(panic = \"unwind\"), ignore)]\nfn slow_path() {\n    assert!(check());\n}\n"
            ),
            0,
            "a cfg_attr ignore is conditional"
        );
        // An ALIAS hides the condition behind a name; rich binds five.
        assert_eq!(
            skips(
                Lang::Python,
                "tests/test_a.py",
                "skip_py38 = pytest.mark.skipif(sys.version_info < (3, 9), reason=\"3.9+\")\n\n@skip_py38\ndef test_slow():\n    assert check()\n"
            ),
            0,
            "an alias for skipif is still conditional"
        );
        // `.only` silences its SIBLINGS, not itself — a different
        // claim, and one CI usually catches.
        assert_eq!(
            skips(
                Lang::TypeScript,
                "a.test.ts",
                "it.only('handles retries', () => {\n  expect(f()).toBe(1);\n});\n"
            ),
            0
        );
    }

    fn sleeps(lang: Lang, path: &str, src: &str) -> u16 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(path), src);
        f.units
            .iter()
            .filter(|u| u.named_test)
            .map(|u| u.sleep_calls)
            .sum()
    }

    #[test]
    fn a_sleep_is_judged_in_tests_where_every_flavour_is_timing() {
        // A test that sleeps waits for a duration instead of a
        // condition, and the duration is a guess about a machine it
        // will not run on.
        assert_eq!(
            sleeps(
                Lang::Python,
                "tests/test_a.py",
                "def test_settles():\n    trigger()\n    time.sleep(0.25)\n    assert settled()\n"
            ),
            1
        );
        // The reversal: asyncio.sleep is the RIGHT way to yield an
        // executor, which is why `blocking async` exempts it — and the
        // WRONG way to wait for a result, which is why this does not.
        assert_eq!(
            sleeps(
                Lang::Python,
                "tests/test_a.py",
                "async def test_settles():\n    trigger()\n    await asyncio.sleep(0.25)\n    assert settled()\n"
            ),
            1,
            "every flavour of sleep is timing dependence in a test"
        );
        // Production sleeping is another metric's jurisdiction.
        assert_eq!(
            sleeps(
                Lang::Python,
                "a.py",
                "def poll_until_ready():\n    while not ready():\n        time.sleep(0.25)\n"
            ),
            0
        );
        // A helper in a test file declares no test, so it is exempt —
        // the same line the whole test-quality family draws.
        assert_eq!(
            sleeps(
                Lang::Python,
                "tests/test_a.py",
                "def wait_for_port():\n    time.sleep(0.25)\n"
            ),
            0
        );
    }

    fn magic_strings(lang: Lang, path: &str, src: &str) -> usize {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), src)
            .magic_strings
            .len()
    }

    #[test]
    fn a_repeated_literal_is_only_magic_when_nothing_named_it() {
        // The smell: one decision spelled out in three separate
        // functions, where a name would have said it once.
        assert_eq!(
            magic_strings(
                Lang::Python,
                "a.py",
                "def a():\n    emit(\"connection refused\")\n\ndef b():\n    emit(\"connection refused\")\n\ndef c():\n    emit(\"connection refused\")\n"
            ),
            1
        );
        // A TABLE is content, not logic — the same reason clone
        // detection refuses duplicated data. gold's worst offender was
        // a TextMate grammar repeating one include 186 times inside a
        // single object literal.
        assert_eq!(
            magic_strings(
                Lang::Python,
                "a.py",
                "def build():\n    return [\n        {\"include\": \"ever_present\"},\n        {\"include\": \"ever_present\"},\n        {\"include\": \"ever_present\"},\n    ]\n"
            ),
            0,
            "one unit repeating a literal is a table"
        );
        // A named constant IS the remedy, so it must not read as the
        // disease — however many places then use the name.
        assert_eq!(
            magic_strings(
                Lang::Python,
                "a.py",
                "REFUSED = \"connection refused\"\n\ndef a():\n    emit(REFUSED)\n\ndef b():\n    emit(REFUSED)\n\ndef c():\n    emit(REFUSED)\n"
            ),
            0
        );
        // Short literals are punctuation, flags and format fragments.
        assert_eq!(
            magic_strings(
                Lang::Python,
                "a.py",
                "def a():\n    j(\", \")\n\ndef b():\n    j(\", \")\n\ndef c():\n    j(\", \")\n"
            ),
            0
        );
        // An interpolated literal is a computation; two of them
        // sharing text are not one constant.
        assert_eq!(
            magic_strings(
                Lang::Python,
                "a.py",
                "def a(x):\n    emit(f\"connection refused {x}\")\n\ndef b(x):\n    emit(f\"connection refused {x}\")\n\ndef c(x):\n    emit(f\"connection refused {x}\")\n"
            ),
            0
        );
        // Tests restate their subject by design.
        assert_eq!(
            magic_strings(
                Lang::Python,
                "tests/test_a.py",
                "def test_a():\n    assert e() == \"connection refused\"\n\ndef test_b():\n    assert e() == \"connection refused\"\n\ndef test_c():\n    assert e() == \"connection refused\"\n"
            ),
            0
        );
    }

    fn unawaited(lang: Lang, path: &str, src: &str) -> u16 {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(path), src);
        f.units.iter().map(|u| u.unawaited).sum()
    }

    #[test]
    fn an_unawaited_coroutine_is_judged_same_file_and_unambiguous_only() {
        // The bug: work() builds a coroutine and throws it away — the
        // body never runs, and Python only warns at runtime.
        assert_eq!(
            unawaited(
                Lang::Python,
                "a.py",
                "async def work():\n    pass\n\nasync def main():\n    work()\n"
            ),
            1
        );
        // Awaited, assigned, passed, or returned: someone owns the
        // coroutine, and ownership is not this metric's business.
        assert_eq!(
            unawaited(
                Lang::Python,
                "a.py",
                "async def work():\n    pass\n\nasync def main():\n    await work()\n    t = work()\n    await asyncio.gather(work(), work())\n    return work()\n"
            ),
            0
        );
        // A cross-file callee is never guessed; a sync callee returns
        // a value the caller may legitimately ignore.
        assert_eq!(
            unawaited(
                Lang::Python,
                "a.py",
                "def log_it():\n    pass\n\nasync def main():\n    fetch_remote()\n    log_it()\n"
            ),
            0
        );
        // A name with a sync twin is ambiguous: no claim without types.
        assert_eq!(
            unawaited(
                Lang::Python,
                "a.py",
                "async def flush():\n    pass\n\nclass Sink:\n    def flush(self):\n        pass\n\nasync def main():\n    flush()\n"
            ),
            0,
            "a sync twin makes the name undecidable"
        );
        // Rust: a statement-position call drops the future unpolled;
        // .await is ownership taken.
        assert_eq!(
            unawaited(
                Lang::Rust,
                "a.rs",
                "async fn tick() {}\n\nasync fn run() {\n    tick();\n    tick().await;\n}\n"
            ),
            1
        );
        // TS/JS are declared dead, and the reason is semantic: a
        // promise is EAGERLY scheduled — persist() runs, only its
        // rejection goes unobserved. That weaker claim belongs to
        // no-floating-promises, and gold showed admired code making
        // this exact call on purpose, 144 times, for telemetry.
        assert_eq!(
            unawaited(
                Lang::TypeScript,
                "a.ts",
                "async function persist(): Promise<void> {}\n\nexport async function save() {\n  persist();\n}\n"
            ),
            0,
            "a floating promise still RAN; that is another tool's claim"
        );
    }

    fn widths(lang: Lang, path: &str, src: &str) -> Vec<(String, u16)> {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(path), src);
        f.interfaces
            .iter()
            .map(|i| (i.name.to_string(), i.methods))
            .collect()
    }

    #[test]
    fn interface_width_counts_methods_not_data_fields_or_embeds() {
        // Go: two interfaces in one grouped declaration, each its own
        // finding; the embedded io.Reader is composition and costs 0.
        assert_eq!(
            widths(
                Lang::Go,
                "a.go",
                "type (\n\tStore interface {\n\t\tGet(k string) ([]byte, error)\n\t\tPut(k string, v []byte) error\n\t\tio.Reader\n\t}\n\tCloser interface {\n\t\tClose() error\n\t}\n)\n"
            ),
            [("Store".to_string(), 2), ("Closer".to_string(), 1)]
        );
        // Rust: defaulted methods still bind implementers; associated
        // types and consts are not methods.
        assert_eq!(
            widths(
                Lang::Rust,
                "a.rs",
                "trait T {\n    fn a(&self);\n    fn b(&self) {}\n    type Item;\n    const N: u8;\n}\n"
            ),
            [("T".to_string(), 2)]
        );
        // TS: a props shape full of data fields — even function-typed
        // ones — is a record, not a contract, and reads width 0.
        assert_eq!(
            widths(
                Lang::TypeScript,
                "a.ts",
                "interface Props {\n  title: string;\n  count: number;\n  onClick: (e: Event) => void;\n  render(): void;\n}\n"
            ),
            [("Props".to_string(), 1)],
            "one method signature; three data fields cost nothing"
        );
        // `type X = { ... }` is the same declaration in a newer
        // keyword, and a contract does not narrow because its author
        // preferred one spelling — 2,144 of them in gold TypeScript.
        assert_eq!(
            widths(
                Lang::TypeScript,
                "a.ts",
                "type Store = {\n  get(k: string): Uint8Array;\n  put(k: string, v: Uint8Array): void;\n  size: number;\n};\n"
            ),
            [("Store".to_string(), 2)],
            "two methods; the data field costs nothing"
        );
        // A type alias that is not an object declares no contract.
        assert_eq!(
            widths(Lang::TypeScript, "a.ts", "type Id = string | number;\n"),
            [],
            "a union alias is not an interface"
        );
    }

    #[test]
    fn return_arity_prices_the_declared_width_not_the_error_idiom() {
        // The idiom every Go signature carries: two values, one of them
        // the error. Grouped names widen it the same as listed types.
        assert_eq!(
            arity(
                Lang::Go,
                "a.go",
                "func f() ([]byte, error) { return nil, nil }\n"
            ),
            2
        );
        assert_eq!(
            arity(Lang::Go, "a.go", "func g() (a, b int) { return }\n"),
            2,
            "grouped result names are each a value the caller places"
        );
        // The finding: a result a caller can only destructure by
        // position, where a swapped pair type-checks.
        assert_eq!(
            arity(
                Lang::Go,
                "a.go",
                "func f() (int, int, string, bool, error) { return 0, 0, \"\", false, nil }\n"
            ),
            5
        );
        assert_eq!(
            arity(Lang::Rust, "a.rs", "fn f() -> (u8, u8, u8) { (0, 0, 0) }\n"),
            3
        );
        // An array VALUE without a tuple type stays one value — JS has
        // no way to state the intent, so the JS pack answers 0.
        assert_eq!(
            arity(
                Lang::JavaScript,
                "a.js",
                "function f() { return [a, b, c, d]; }\n"
            ),
            0
        );
        assert_eq!(
            arity(Lang::Python, "a.py", "def f():\n    return a, b, c\n"),
            3
        );
        assert_eq!(
            arity(
                Lang::Python,
                "a.py",
                "def f():\n    def inner():\n        return a, b, c, d\n    return inner\n"
            ),
            4,
            "the nested def's four-wide return belongs to inner, and inner reads 4"
        );
        assert_eq!(
            arity(Lang::Python, "a.py", "def f():\n    return\n"),
            0,
            "a bare return ships nothing"
        );
    }

    #[test]
    fn one_generic_level_unwraps_the_value_but_never_a_collection() {
        // The fallible wrapper is not one of the values a caller
        // destructures: Result<(A,B,C,D), E> ships four, not one.
        assert_eq!(
            arity(
                Lang::Rust,
                "a.rs",
                "fn f() -> Result<(u8, u8, u8, u8), Error> { todo!() }\n"
            ),
            4
        );
        assert_eq!(
            arity(
                Lang::Rust,
                "a.rs",
                "fn f() -> std::result::Result<(u8, u8), Error> { todo!() }\n"
            ),
            2,
            "the same wrapper, however it is spelled"
        );
        assert_eq!(
            arity(
                Lang::Rust,
                "a.rs",
                "fn f() -> Vec<(u8, u8, u8)> { vec![] }\n"
            ),
            1,
            "a Vec OF tuples is one value, however many it holds"
        );
        assert_eq!(
            arity(
                Lang::TypeScript,
                "a.ts",
                "function f(): [number, string] { return [0, '']; }\n"
            ),
            2
        );
        // Without this the entire async half of TypeScript read as 1.
        assert_eq!(
            arity(
                Lang::TypeScript,
                "a.ts",
                "async function f(): Promise<[number, string, boolean, Error]> { return x; }\n"
            ),
            4
        );
        assert_eq!(
            arity(
                Lang::TypeScript,
                "a.ts",
                "function f(): Array<[number, string]> { return []; }\n"
            ),
            1,
            "a list OF tuples is one value"
        );
        // Python's annotation is read beside its return sites, widest
        // wins; a variadic tuple is a sequence, which is one value.
        assert_eq!(
            arity(
                Lang::Python,
                "a.py",
                "def f() -> tuple[int, str, bool]:\n    return t\n"
            ),
            3
        );
        assert_eq!(
            arity(
                Lang::Python,
                "a.py",
                "def f() -> Tuple[int, str]:\n    return t\n"
            ),
            2
        );
        assert_eq!(
            arity(
                Lang::Python,
                "a.py",
                "def f() -> tuple[int, ...]:\n    return t\n"
            ),
            1,
            "a variadic tuple is a sequence, however long it runs"
        );
    }

    #[test]
    fn a_hook_is_identified_by_call_order_so_a_branch_renumbers_it() {
        // The bug: the first time `ready` flips, every hook after this
        // one renumbers and reads another hook's state.
        const REACT: &str = "import { useState, useEffect } from 'react';\n";
        assert_eq!(
            hooks(
                Lang::Tsx,
                "P.tsx",
                &format!(
                    "{REACT}export function Panel({{ ready }}: {{ ready: boolean }}) {{\n  if (ready) {{\n    const [n] = useState(0);\n    return <b>{{n}}</b>;\n  }}\n  return null;\n}}\n"
                ),
            ),
            1
        );
        // The same code in a file that is not React's is not React's
        // problem: a `useX()` helper elsewhere is a function with a name.
        assert_eq!(
            hooks(
                Lang::Tsx,
                "P.tsx",
                "export function Panel({ ready }: { ready: boolean }) {\n  if (ready) {\n    const [n] = useState(0);\n    return <b>{n}</b>;\n  }\n  return null;\n}\n",
            ),
            0,
            "no react import, no rules-of-hooks question"
        );
        // A lowercase factory is not a component or a hook, so the rule
        // does not reach it — tRPC's createTRPCNext is the shape.
        assert_eq!(
            hooks(
                Lang::Tsx,
                "P.tsx",
                &format!(
                    "{REACT}export function createHooks(opts: Opts) {{\n  if (opts.ssr) {{\n    const [n] = useState(0);\n    return n;\n  }}\n  return null;\n}}\n"
                ),
            ),
            0,
            "a factory returns hooks; it is not one"
        );
        // Unconditional hooks at the top of a component are the rule
        // being followed, not broken.
        assert_eq!(
            hooks(
                Lang::Tsx,
                "P.tsx",
                &format!(
                    "{REACT}export function Panel({{ id }}: {{ id: string }}) {{\n  const [u, setU] = useState(null);\n  useEffect(() => {{\n    if (id) {{\n      load(id).then(setU);\n    }}\n  }}, [id]);\n  return <b>{{u}}</b>;\n}}\n"
                ),
            ),
            0,
            "a branch INSIDE an effect body is ordinary logic, not a conditional hook"
        );
        // A capital after `use` is what makes a hook: `used()` and
        // `useful()` are ordinary functions.
        assert_eq!(
            hooks(
                Lang::JavaScript,
                "a.js",
                "function f(x) {\n  if (x) {\n    return used(x) + useful(x);\n  }\n  return 0;\n}\n",
            ),
            0
        );
        // Solid tracks dependencies at run time, not by call order, so
        // a conditional createSignal is legal and stays silent.
        assert_eq!(
            hooks(
                Lang::Tsx,
                "S.tsx",
                "export function C(props) {\n  if (props.on) {\n    const [n] = createSignal(0);\n    return <b>{n()}</b>;\n  }\n  return null;\n}\n",
            ),
            0,
            "Solid's primitives are not order-identified"
        );
    }

    #[test]
    fn restraint_where_the_adjacent_pattern_is_correct() {
        // copy(src, dst) is the textbook confusable pair — and a run of
        // two is ubiquitous in admired code, so the budget starts at
        // three. Named parameters on a public fn state a contract, so
        // kw-opacity has nothing to say.
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.py"),
            "def copy(src: str, dst: str) -> None:\n    write(dst, read(src))\n",
        );
        let mut fired = Vec::new();
        for_each(&f, |m, v, _, _| {
            if (m == CONFUSABLE && v > 2.0) || (m == KW_OPACITY && v > 0.0) {
                fired.push(METRICS[m].name);
            }
        });
        assert!(fired.is_empty(), "{fired:?}");
    }

    /// Where each metric's restraint case lives: the test asserting it
    /// stays SILENT on the adjacent correct pattern. Seven confirmed
    /// detector bugs shipped under a green suite, because every test
    /// asserted FIRING and almost none asserted silence — asyncio.sleep
    /// beside time.sleep, AKIA beside password_field.
    const RESTRAINT: &[(&str, &str)] = &[
        ("cognitive", "flat_function_costs_nothing"),
        ("cyclomatic", "flat_function_costs_nothing"),
        ("depth", "flat_function_costs_nothing"),
        ("length", "flat_function_costs_nothing"),
        ("params", "methods_skip_receiver_and_detect_flags"),
        ("flag params", "methods_skip_receiver_and_detect_flags"),
        ("expr depth", "expression_height_flags_one_liners"),
        ("demeter", "demeter_flags_data_chains_not_fluent_calls"),
        (
            "negations",
            "double_negation_is_coercion_not_inverted_logic",
        ),
        ("live span", "live_span_measures_definition_to_last_use"),
        ("magic numbers", "magic_numbers_require_a_name"),
        ("spooky", "spooky_constructs_are_counted_with_lines"),
        (
            "pass-through",
            "passthrough_forwards_own_params_adapters_do_not",
        ),
        (
            "kw opacity",
            "restraint_where_the_adjacent_pattern_is_correct",
        ),
        (
            "and name",
            "name_quality_flags_conjunctions_and_junk_vocabulary",
        ),
        (
            "generic name",
            "name_quality_flags_conjunctions_and_junk_vocabulary",
        ),
        (
            "feature envy",
            "feature_envy_needs_a_dominant_foreign_receiver",
        ),
        ("swallowed", "an_empty_err_check_swallows_the_error_in_go"),
        (
            "broad catch",
            "error_discipline_flags_silent_and_broad_handlers",
        ),
        (
            "unwraps",
            "error_discipline_flags_silent_and_broad_handlers",
        ),
        ("lying name", "lying_names_break_their_contract"),
        (
            "test asserts",
            "a_test_is_declared_only_by_evidence_its_context_supports",
        ),
        (
            "lazy test name",
            "test_quality_flags_assertless_and_lazy_tests",
        ),
        ("asserts", "assert_density_emitted_only_for_complex_units"),
        (
            "public docs",
            "public_surface_and_contract_docs_per_language",
        ),
        ("echo comments", "echo_comments_flag_restated_code_only"),
        ("comment ratio", "(two-sided band, calibrated per language)"),
        (
            "untyped params",
            "escape_hatches_are_found_through_their_generics",
        ),
        (
            "loose types",
            "escape_hatches_are_found_through_their_generics",
        ),
        (
            "casts",
            "every_language_spells_its_own_cast_and_they_all_count",
        ),
        (
            "suppressions",
            "every_language_spells_its_own_cast_and_they_all_count",
        ),
        (
            "confusable",
            "restraint_where_the_adjacent_pattern_is_correct",
        ),
        (
            "terse name",
            "a_name_must_grow_with_the_distance_it_carries",
        ),
        (
            "abbreviated",
            "crushed_vowels_are_flagged_but_real_words_are_not",
        ),
        (
            "vacuous asserts",
            "an_assertion_on_a_literal_is_green_by_construction",
        ),
        (
            "lost context",
            "a_handler_that_forgets_the_cause_is_flagged",
        ),
        (
            "ceremony",
            "ceremony_needs_the_documentation_and_spares_the_override",
        ),
        (
            "secrets",
            "aws_keys_fire_paths_do_not_and_vendor_prefixes_need_no_name",
        ),
        (
            "blocking async",
            "the_runtimes_own_sleep_is_the_fix_not_the_bug",
        ),
        (
            "unmanaged",
            "a_resource_needs_a_guard_only_where_guards_are_the_idiom",
        ),
        (
            "dropped tasks",
            "a_resource_needs_a_guard_only_where_guards_are_the_idiom",
        ),
        (
            "wildcard match",
            "the_type_system_is_opted_out_of_one_construct_at_a_time",
        ),
        (
            "stringly id",
            "the_type_system_is_opted_out_of_one_construct_at_a_time",
        ),
        (
            "loop depth",
            "loop_nesting_is_arithmetic_and_copies_inside_loops_are_named",
        ),
        (
            "alloc in loop",
            "loop_nesting_is_arithmetic_and_copies_inside_loops_are_named",
        ),
        (
            "conditional hook",
            "a_hook_is_identified_by_call_order_so_a_branch_renumbers_it",
        ),
        (
            "returns",
            "return_arity_prices_the_declared_width_not_the_error_idiom",
        ),
        (
            "interface width",
            "interface_width_counts_methods_not_data_fields_or_embeds",
        ),
        (
            "repurposed",
            "repurposing_spares_collectors_overrides_and_sheltered_fills",
        ),
        (
            "unawaited coroutine",
            "an_unawaited_coroutine_is_judged_same_file_and_unambiguous_only",
        ),
        (
            "magic strings",
            "a_repeated_literal_is_only_magic_when_nothing_named_it",
        ),
        (
            "sleepy test",
            "a_sleep_is_judged_in_tests_where_every_flavour_is_timing",
        ),
        (
            "skipped tests",
            "only_an_unconditional_skip_is_a_suppression",
        ),
        (
            "bool traps",
            "a_named_argument_is_not_a_trap_however_many_booleans_follow",
        ),
        (
            "commented code",
            "prose_that_happens_to_parse_is_still_prose",
        ),
        (
            "cohesion",
            "methods_that_share_a_field_or_call_each_other_are_one_object",
        ),
        (
            "built query",
            "a_parameterized_query_is_the_remedy_and_stays_silent",
        ),
        (
            "shelled out",
            "an_argument_list_needs_no_shell_and_stays_silent",
        ),
    ];

    #[test]
    fn every_metric_names_its_restraint_test() {
        for def in METRICS {
            assert!(
                RESTRAINT.iter().any(|(name, _)| *name == def.name),
                "metric {:?} has no named restraint test — add the silence case first",
                def.name
            );
        }
        assert_eq!(
            RESTRAINT.len(),
            METRICS.len(),
            "stale restraint entries for retired metrics"
        );
    }

    #[test]
    fn known_complexity_values() {
        // if:+1  for:+2  if:+3  and:+1  elif:+1  else:+1  recursion:+1  = 10
        // cyclomatic: if, for, if, and, elif = 5 decisions + 1 = 6
        let (cog, cyc, depth) = unit_metrics(
            "def f(a, b):\n    if a:\n        for x in b:\n            if x and a:\n                return 1\n    elif b:\n        return 2\n    else:\n        return f(a, b)\n",
        );
        assert_eq!(cog, 10);
        assert_eq!(cyc, 6);
        assert_eq!(depth, 3);
    }

    #[test]
    fn flat_function_costs_nothing() {
        let (cog, cyc, depth) = unit_metrics("def f(x):\n    return x + 1\n");
        assert_eq!((cog, cyc, depth), (0, 1, 0));
    }

    #[test]
    fn comprehension_filter_is_a_flat_decision() {
        let (cog, cyc, depth) = unit_metrics("def f(xs):\n    return [x for x in xs if x]\n");
        assert_eq!((cog, cyc, depth), (1, 2, 0));
    }

    #[test]
    fn try_nests_visually_but_not_cognitively() {
        // try adds a visual level; except costs 1 + nesting (0 here).
        let (cog, _, depth) =
            unit_metrics("def f():\n    try:\n        g()\n    except ValueError:\n        pass\n");
        assert_eq!(cog, 1);
        assert_eq!(depth, 1);
    }

    #[test]
    fn asserts_are_invariants_not_decisions() {
        // NASA/TigerStyle: assertions must not inflate complexity.
        let (cog, cyc, _) = unit_metrics("def f(x):\n    assert x > 0\n    return x\n");
        assert_eq!((cog, cyc), (0, 1));
    }

    #[test]
    fn assert_density_emitted_only_for_complex_units() {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let complex = "def f(xs):\n    t = 0\n    for x in xs:\n        if x > 0:\n            if x > 1:\n                if x > 2:\n                    if x > 3:\n                        t += x\n    return t\n";
        let simple = "def g(x):\n    return x + 1\n";
        let mut emitted = |src: &str| {
            let facts = extract(pack, &mut parser, Path::new("t.py"), src);
            let mut vals = Vec::new();
            for_each(&facts, |m, v, _, _| {
                if m == ASSERTS {
                    vals.push(v);
                }
            });
            vals
        };
        assert_eq!(emitted(complex), [0.0], "complex unit without asserts");
        assert!(emitted(simple).is_empty(), "simple unit exempt");
    }

    #[test]
    fn declared_tests_are_judged_whatever_their_style() {
        // Zig `test "label"` blocks: the prose label names the unit and
        // the block is a declared test — judged like test_-named functions.
        let pack = Lang::Zig.pack();
        let mut parser = pack.make_parser();
        let facts = extract(
            pack,
            &mut parser,
            Path::new("t.zig"),
            "test \"rejects expired token\" {\n    try std.testing.expect(true);\n}\n\ntest \"ok\" {\n    run();\n}\n",
        );
        assert_eq!(&*facts.units[1].name, "rejects expired token");
        let mut hits: Vec<(usize, String, f32)> = Vec::new();
        for_each(&facts, |m, v, _, name| {
            if m == TEST_ASSERTS || m == LAZY_TEST_NAME {
                hits.push((m, name.to_string(), v));
            }
        });
        assert_eq!(
            hits,
            [
                (TEST_ASSERTS, "rejects expired token".into(), 1.0),
                (LAZY_TEST_NAME, "rejects expired token".into(), 0.0),
                (TEST_ASSERTS, "ok".into(), 0.0),
                (LAZY_TEST_NAME, "ok".into(), 1.0),
            ]
        );
    }

    #[test]
    fn go_assertion_helpers_are_recognized() {
        // Go's stdlib has no assert; without the hook every Go test read
        // as assertionless (1393/1393 of esbuild's).
        let pack = Lang::Go.pack();
        let mut parser = pack.make_parser();
        let facts = extract(
            pack,
            &mut parser,
            Path::new("x_test.go"),
            "package p\n\nfunc TestThing(t *T) {\n    assertEqual(t, 1, 2)\n    assert.Equal(t, 3, 4)\n    t.Helper()\n}\n",
        );
        let mut vals = Vec::new();
        for_each(&facts, |m, v, _, _| {
            if m == TEST_ASSERTS {
                vals.push(v);
            }
        });
        assert_eq!(vals, [2.0], "own helper and testify package, not t.Helper");
    }

    #[test]
    fn expect_helpers_need_the_capital() {
        // `expectPrinted(...)` is a Go table-test assertion; a production
        // `expectedValue(...)` is not, and the only thing separating them
        // is the capital after the prefix.
        let pack = Lang::Go.pack();
        let mut parser = pack.make_parser();
        let src = "package p\n\nfunc TestA(t *T) {\n    expectPrinted(t, 1)\n}\n\nfunc TestB(t *T) {\n    expectedValue(1)\n}\n";
        let facts = extract(pack, &mut parser, Path::new("x_test.go"), src);
        let mut vals = Vec::new();
        for_each(&facts, |m, v, _, _| {
            if m == TEST_ASSERTS {
                vals.push(v);
            }
        });
        assert_eq!(
            vals,
            [1.0, 0.0],
            "expectPrinted asserts, expectedValue does not"
        );
    }

    #[test]
    fn test_bodies_are_exempt_from_magic_numbers() {
        // Expected values ARE the test; judging them by production
        // budgets made 88% of this gating metric's firings noise. The
        // fixture lives in a test FILE: a test_-named function in
        // production code earns no exemption.
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let src = "def check(code):\n    return code\n\ndef test_statuses():\n    assert check(404) == 404\n    assert check(418) == 418\n    assert check(451) == 451\n";
        let mut names = Vec::new();
        let facts = extract(pack, &mut parser, Path::new("test_thing.py"), src);
        for_each(&facts, |m, v, _, name| {
            if m == MAGIC_NUMBERS && v > 0.0 {
                names.push(name.to_string());
            }
        });
        assert!(names.is_empty(), "test literals exempt, got {names:?}");
    }

    #[test]
    fn assert_density_counts_call_style_asserts() {
        // Zig/Rust assert as a call, not a statement; both forms must count.
        let pack = Lang::Zig.pack();
        let mut parser = pack.make_parser();
        let src = "fn busy(a: i64, b: i64) i64 {
    assert(a > 0);
    std.debug.assert(b > 0);
    var t: i64 = 0;
    if (a > 1) { if (b > 1) { if (a > b) { if (a > 2) { t += a; } } } }
    if (b > 2) { if (a > 3) { t -= b; } }
    return t;
}
";
        let facts = extract(pack, &mut parser, Path::new("t.zig"), src);
        let mut vals = Vec::new();
        for_each(&facts, |m, v, _, _| {
            if m == ASSERTS {
                vals.push(v);
            }
        });
        assert_eq!(vals, [2.0]);
    }
}
