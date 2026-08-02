//! Language-agnostic facts about one source file — the waist of the system.
//! Packs produce facts; metrics consume them; neither sees the other.

mod extract;

pub use extract::{extract, is_leaked_credential};

use std::path::PathBuf;

use crate::lang::Lang;
use crate::sem::Sem;

pub struct FileFacts {
    pub path: PathBuf,
    pub lang: Lang,
    pub lines: u32,
    pub blank_lines: u32,
    /// Lines occupied by comments and doc strings/comments.
    pub comment_lines: u32,
    pub parse_errors: u32,
    /// Extraction stopped at MAX_TREE_DEPTH. The tree is a generated
    /// blob or a left-nested chain, not code anyone reads, and the
    /// facts below the cut are missing.
    pub too_deep: bool,
    /// Path matches the ecosystem's test-file conventions.
    pub is_test_file: bool,
    /// Named-node count of the whole tree (clone-coverage denominator).
    pub mass: u32,
    /// Measured units: index 0 is the synthetic `<module>` scope, then every
    /// named function/method in source order (nested defs are separate units).
    pub units: Vec<UnitFacts>,
    /// Normalized subtree fingerprints large enough to be clone candidates.
    pub clone_sites: Vec<CloneSite>,
    /// Lines of comments that merely restate their adjacent code.
    pub echo_comments: Vec<u32>,
    /// Lines carrying a type-checker suppression (`@ts-ignore`,
    /// `# type: ignore`). Type safety asserted by comment: the checker
    /// was told to stop looking, and nothing records what it would have
    /// said.
    pub suppressions: Vec<u32>,
    /// Lines binding a credential-shaped name to a literal with real
    /// entropy — a secret compiled into the artifact and committed to
    /// history, where rotating it means a release.
    pub secrets: Vec<u32>,
    /// Lines that switch a test off unconditionally — `#[ignore]`,
    /// `it.skip(...)`, `t.Skip()`. A suppression wearing a test's
    /// name: the suite still reports green, and nothing records what
    /// the test would have said. A CONDITIONAL skip is absent by
    /// design — `skipif(platform)` is stated judgment.
    pub skipped_tests: Vec<u32>,
    /// The same non-trivial string literal, written out again and
    /// again in one file: a constant nobody named. Clone detection
    /// cannot see these — duplicated DATA is content, not logic — so
    /// nothing else in the tool owns this smell.
    pub magic_strings: Vec<u32>,
    /// Lines where the text stops predicting the run: eval/exec, computed
    /// attribute access, metaclasses, transmute, mutable defaults.
    pub spooky_lines: Vec<u32>,
    /// Names referenced inside this file's test units — the join key for
    /// untested-complexity analysis (name association, not coverage).
    pub test_refs: Vec<Box<str>>,
    /// Normalized case-label sets of match/switch constructs (>=3 arms).
    /// The same set dispatched in many places means every new variant
    /// forces N edits — the polymorphism smell (Fowler), invisible to
    /// clone detection because surrounding code differs.
    pub switch_sigs: Vec<LabelSet>,
    /// Key sets of anonymous record literals (>=3 keys). The same set
    /// built in many places is a type the language was never told
    /// about: nothing checks a typo in a key, and adding a field means
    /// finding every construction site by hand.
    pub record_shapes: Vec<LabelSet>,
    /// Imports as written (`a.b`, `./util`, `crate::x::y`) with the
    /// local names each one binds — the raw material of the dependency
    /// graph and of interface-utilization analysis. Resolution against
    /// the scanned file set happens at aggregation time.
    pub imports: Vec<ImportFact>,
    /// The module's declared surface: names of public units and types.
    /// Parnas: a module is a decision-hiding unit with a declared
    /// interface — depth metrics need the interface.
    pub exports: Vec<Box<str>>,
    /// Every distinct identifier this file mentions, definitions and
    /// member names included. An export's consumers cannot be found from
    /// import lists alone: Go and C never import names, and Rust and
    /// Python reach through a module (`pkg.Symbol`) just as often as they
    /// bind the symbol itself.
    pub mentioned: Vec<Box<str>>,
    /// Intra-file call direction (down, up): a downward reference points
    /// at a unit defined later — the step-down narrative (Clean Code;
    /// Knuth: programs are literature). Bare-name calls only.
    pub step_refs: (u32, u32),
    /// Public-before-private ordering: (public-first pairs, total
    /// public/private pairs) — entry points first, details after.
    pub pub_order: (u32, u32),
    /// Declared method bundles (Go `interface`, Rust `trait`, TS
    /// `interface`) and how many methods each one demands. The bigger
    /// the interface, the weaker the abstraction — an implementer owes
    /// every method whether or not a caller ever wanted them together.
    pub interfaces: Vec<InterfaceFact>,
}

/// One declared interface: what it is called, where, and how many
/// methods it requires. Data fields and embedded interfaces are not
/// methods — a props shape is a record, and embedding is composition.
pub struct InterfaceFact {
    pub name: Box<str>,
    pub line: u32,
    pub methods: u16,
}

#[derive(Clone)]
pub struct ImportFact {
    pub target: Box<str>,
    /// Local names this import binds (named-import styles only).
    pub names: Vec<Box<str>>,
}

/// A sorted set of names, joined with `\u{1f}`, and where it appeared.
/// Used for two recurrences: the arms of a match, and the keys of an
/// anonymous record.
pub struct LabelSet {
    pub key: Box<str>,
    pub line: u32,
}

pub struct UnitFacts {
    pub name: Box<str>,
    /// Scope-qualified name (`Class.method`, `Type::method`, `outer.inner`)
    /// — what reports and machine output show, since bare names are
    /// ambiguous at scale.
    pub qualname: Box<str>,
    /// 1-based line of the definition.
    pub line: u32,
    pub lines: u32,
    pub is_module: bool,
    pub is_method: bool,
    /// Part of the public surface (exported, `pub`, conventionally public).
    pub is_public: bool,
    /// Parameters in declaration order, receiver excluded on methods.
    pub params: Vec<ParamFact>,
    /// Contract-documentation lines (docstring, `///`, JSDoc) — interface
    /// docs, distinct from inline implementation comments.
    pub doc_lines: u32,
    pub max_vis_depth: u16,
    /// Tallest single-line expression tree — the clever-one-liner signal.
    pub max_expr_depth: u16,
    /// Unnamed non-trivial numeric literals outside constant contexts.
    pub magic_numbers: u16,
    /// Longest local live span in lines, with the variable's name —
    /// McConnell: keep variables live for as short a time as possible.
    pub max_live_span: u16,
    pub max_live_var: Box<str>,
    /// Straight-line reassignments whose new value never mentions the
    /// old — the same name now means something else, and every earlier
    /// read the reader remembers is silently wrong (Fowler's Split
    /// Variable). Collecting updates (`x = x + 1`, `s = s.trim()`),
    /// conditional overrides, and try-sheltered fills are all exempt:
    /// each of those keeps or guards the meaning.
    pub repurposed: u16,
    /// Law of Demeter violations: attribute chains reaching >=3 data links
    /// deep (fluent call chains are exempt).
    pub demeter: u16,
    /// Negative logic a reader must invert twice: double negation,
    /// negated negative-polarity names, De Morgan candidates.
    pub negations: u16,
    /// Body is a single call forwarding this unit's own parameters —
    /// Ousterhout's shallow wrapper / Fowler's Middle Man.
    pub is_passthrough: bool,
    /// Member accesses rooted at the receiver (methods only).
    pub self_accesses: u16,
    /// The most-touched foreign receiver and its access count — a method
    /// that spends its time in another object's data belongs there
    /// (Fowler's Feature Envy).
    pub envy_count: u16,
    pub envy_object: Box<str>,
    /// Handlers whose body silences the error entirely.
    pub swallowed: u16,
    /// Bare or Exception-wide catches.
    pub broad_catch: u16,
    /// Handlers that raise a new error without forwarding the original —
    /// the stack that explains WHY is gone.
    pub lost_context: u16,
    /// unwrap()/expect() calls — panics where errors belonged.
    pub unwraps: u16,
    /// Types asserted rather than proved: `as`, `x.(T)`, `(T)x`,
    /// `cast(T, x)`, `@intCast`.
    pub casts: u16,
    /// Declared async: a blocking call in here stalls the executor, not
    /// just this task.
    pub is_async: bool,
    /// Calls that park the thread inside an async unit.
    pub blocking_calls: u16,
    /// Sleeps of ANY flavour. Inside a test the async exemption
    /// reverses: `await asyncio.sleep(...)` is the correct way to
    /// yield an executor and the wrong way to wait for a result, so a
    /// test that sleeps is timing-dependent whoever schedules it.
    pub sleep_calls: u16,
    /// Statement-position calls to a SAME-FILE async unit with no await
    /// and the result discarded. In Python the coroutine never runs; in
    /// Rust the future is dropped unpolled; in TS the promise floats
    /// with nobody to catch its rejection. Same-file evidence only —
    /// a cross-file callee is never guessed at.
    pub unawaited: u16,
    /// Winnowed fingerprints of this unit's normalized token stream —
    /// the raw material of near-clone detection. Empty for units too
    /// short to say anything.
    pub fingerprints: Vec<u64>,
    /// Deepest nesting of loops within this unit. Exact and structural:
    /// three deep IS cubic in whatever the loops range over, whether or
    /// not that is a problem here.
    pub max_loop_depth: u16,
    /// Calls that allocate a fresh copy inside a loop body, where the
    /// allocation could have been hoisted or borrowed.
    pub allocs_in_loop: u16,
    /// Hook calls (`useState`, `useEffect`, ...) reached through a
    /// branch or a loop. React identifies a hook by CALL ORDER, so a
    /// conditional one shifts every later hook's identity the first
    /// time the branch flips — state belonging to another hook.
    pub conditional_hooks: u16,
    /// Resources opened without a scope guard: closing them becomes a
    /// promise made in prose, and an early return breaks it.
    pub unmanaged: u16,
    /// Matches with a catch-all arm: adding a variant will not break
    /// this, which is the entire benefit of an exhaustive match.
    pub wildcard_matches: u16,
    /// Spawned tasks whose handle is discarded — nothing can await them,
    /// nothing observes their panic, and the runtime may drop them at
    /// shutdown mid-write.
    pub dropped_tasks: u16,
    /// Test by attribute, naming convention, or test-file location.
    pub is_test: bool,
    /// Declared a test by evidence its context supports: an attribute,
    /// structure, or a `test` block anywhere — or the naming convention,
    /// inside a test file only. Test-quality metrics judge these;
    /// test-file helpers and `cfg(test)` fixtures are exempt, unjudged.
    pub named_test: bool,
    /// Assertion calls/macros — a test without any tests nothing.
    pub assert_calls: u16,
    /// Assertions that cannot fail: the subject is a literal, so the
    /// check passes no matter what the code under test did.
    pub vacuous_asserts: u16,
    /// Declared return type text ("" when absent/untyped).
    pub returns: Box<str>,
    /// How many values a caller must destructure: a Go result list's
    /// width, a Rust or TS tuple return type's width, the widest tuple
    /// a Python `return` ships. Four values travelling together are a
    /// struct in hiding — the same argument `params` makes, pointed at
    /// the other end of the signature.
    pub return_arity: u16,
    /// Receiver taken mutably (`&mut self`) — a getter that mutates lies.
    pub mut_receiver: bool,
    pub self_recursive: bool,
    /// Control events in source order, with the cognitive nesting depth at
    /// which each occurred.
    pub ctrl: Vec<CtrlFact>,
}

pub struct ParamFact {
    pub name: Box<str>,
    /// Boolean-typed or boolean-defaulted — flag parameter candidate.
    pub boolish: bool,
    /// `**kwargs`-style splat — interface opacity.
    pub kw_splat: bool,
    /// Optional at the call site (default value, `?`, splat): adding
    /// one to a signature breaks no caller.
    pub optional: bool,
    /// Carries a declared type.
    pub typed: bool,
    /// Declared, but with one of the language's escape hatches.
    pub loose: bool,
    /// Declared type as written, empty when absent.
    pub type_name: Box<str>,
}

impl FileFacts {
    /// More than 0.5% of nodes are parse errors (or nothing parsed at all):
    /// metrics over such trees are noise and must not enter distributions.
    pub fn low_confidence(&self) -> bool {
        self.too_deep
            || self.parse_errors > 0
                && (self.mass == 0 || self.parse_errors as u64 * 200 > self.mass as u64)
    }
}

impl UnitFacts {
    pub fn flag_params(&self) -> usize {
        self.params.iter().filter(|p| p.boolish).count()
    }

    /// Parameters with no declared type. In a gradually-typed language
    /// this is the surface a refactor cannot be checked against.
    pub fn untyped_params(&self) -> usize {
        self.params.iter().filter(|p| !p.typed).count()
    }

    /// Parameters typed with an escape hatch: annotated, asserting
    /// nothing. Worse than untyped, because it reads as a decision.
    pub fn loose_params(&self) -> usize {
        self.params.iter().filter(|p| p.loose).count()
    }

    /// Parameters that name an identity but are typed as text. Any
    /// string fits a `str` — including another entity's id, a slug, or
    /// the empty string — so the only thing standing between a user id
    /// and an order id is the caller paying attention.
    pub fn stringly_ids(&self) -> usize {
        const IDENTITY: &[&str] = &["id", "key", "uuid", "guid", "token", "handle"];
        const TEXTUAL: &[&str] = &["str", "string", "String", "&str", "&'static str", "text"];
        self.params
            .iter()
            .filter(|p| {
                let name = p.name.to_ascii_lowercase();
                let names_identity = IDENTITY
                    .iter()
                    .any(|w| name == *w || name.ends_with(&format!("_{w}")));
                names_identity && TEXTUAL.contains(&&*p.type_name)
            })
            .count()
    }

    /// Longest run of ADJACENT parameters sharing a declared type. Two
    /// such parameters can be swapped at a call site and nothing —
    /// compiler, test, or reviewer reading the call — will notice.
    /// `copy(src: Path, dst: Path)` is the canonical case; the remedy is
    /// a newtype per role, or a parameter object.
    pub fn confusable_run(&self) -> usize {
        let (mut best, mut run) = (0, 0);
        let mut previous = "";
        for p in &self.params {
            run = if !p.type_name.is_empty() && &*p.type_name == previous {
                run + 1
            } else {
                1
            };
            previous = &p.type_name;
            best = best.max(run);
        }
        best
    }

    /// A named unit with everything else empty — fixture scaffolding.
    #[cfg(test)]
    pub fn for_test(name: &str) -> UnitFacts {
        let mut u = extract::blank_unit();
        u.name = name.into();
        u.qualname = name.into();
        u
    }
}

#[derive(Clone, Copy)]
pub struct CtrlFact {
    pub sem: Sem,
    /// 1-based line of the construct.
    pub line: u32,
    pub cog_depth: u8,
    /// For `BoolOp`: starts a new operator sequence (`a and b or c` = 2,
    /// `a and b and c` = 1). Always true for other sems.
    pub new_seq: bool,
}

/// A subtree whose normalized hash may recur elsewhere. Identifiers and
/// literals are bucketed, so Type-2 clones (renamed, re-literaled) collide.
#[derive(Clone, Copy)]
pub struct CloneSite {
    pub hash: u64,
    pub mass: u32,
    pub line: u32,
    pub end_line: u32,
}
