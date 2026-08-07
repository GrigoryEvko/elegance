//! Language-agnostic facts about one source file — the waist of the system.
//! Packs produce facts; metrics consume them; neither sees the other.

mod extract;

pub use extract::{extract, is_leaked_credential};
/// A directory whose files RUN rather than get imported — the graph asks
/// the same question `is_sink` answers for a C translation unit.
pub(crate) use extract::{one_shot_dir, rooted};

use std::path::PathBuf;

use crate::lang::Lang;
use crate::prose::Prose;
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
    /// A one-shot script rather than a shipped program: a build step, a
    /// codegen pass, a benchmark harness, a config file. It runs, it
    /// finishes, and nothing else is waiting on its executor.
    pub is_script_file: bool,
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
    /// Rows carrying a debt marker — TODO, FIXME, HACK, XXX — in a
    /// comment. A marker is a promise with no deadline; only its AGE
    /// says whether it was a plan or a monument.
    pub debt_markers: Vec<u32>,
    /// First line of each comment block that PARSES as this language:
    /// code someone commented out instead of deleting, which the
    /// version control system was already remembering for them.
    pub commented_code: Vec<u32>,
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
    /// Lines building an SQL statement by INTERPOLATION — an f-string,
    /// a template literal, a format call. A literal query is safe
    /// whatever it says; a query assembled from values is the oldest
    /// vulnerability there is, and the remedy (a parameter marker) is
    /// the shape this deliberately stays silent on.
    pub sql_built: Vec<u32>,
    /// Lines handing an ASSEMBLED command to a shell. `shell=True`
    /// with a literal is a style choice; with a value spliced in it is
    /// the same hole as a built query, at a bigger sink. The remedy —
    /// an argument LIST, which needs no shell — carries no
    /// interpolation and is invisible here, as it should be.
    pub shelled_out: Vec<u32>,
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
    /// Declared classes with two or more methods, and how many
    /// disconnected groups those methods fall into.
    pub classes: Vec<ClassFact>,
    /// Declared method bundles (Go `interface`, Rust `trait`, TS
    /// `interface`) and how many methods each one demands. The bigger
    /// the interface, the weaker the abstraction — an implementer owes
    /// every method whether or not a caller ever wanted them together.
    pub interfaces: Vec<InterfaceFact>,
    /// Every comment RUN in the file, classified by what it documents
    /// and measured as prose. A run, not a line: `///` parses one node
    /// per line, and a fenced example cannot be recognized — nor a
    /// sentence counted — a line at a time.
    pub comments: Vec<CommentFact>,
}

/// What a comment is FOR, decided from the Sem of what follows it.
///
/// Pooling these into one distribution makes a budget meaningless for
/// all of them: a field's doc is a phrase, a module header is a page,
/// and a function summary sits between. The classification is made from
/// the ontology alone, so it means the same thing in every language.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommentRole {
    /// Opens the file and documents no single declaration.
    ModuleHeader,
    /// Introduces a class, struct, trait or interface.
    TypeDoc,
    /// Introduces a function or method — the contract a caller reads.
    FnSummary,
    /// Introduces a member of a type that is neither: a field, a
    /// property, a constant, an enum case.
    FieldDoc,
    /// Explains the code around it rather than declaring a contract.
    Inline,
    /// Sits on a line of code, after it.
    Trailing,
}

impl CommentRole {
    /// Every role, in declaration order — the index into per-role
    /// tallies is the discriminant.
    pub const ALL: [CommentRole; 6] = [
        CommentRole::ModuleHeader,
        CommentRole::TypeDoc,
        CommentRole::FnSummary,
        CommentRole::FieldDoc,
        CommentRole::Inline,
        CommentRole::Trailing,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CommentRole::ModuleHeader => "module",
            CommentRole::TypeDoc => "type",
            CommentRole::FnSummary => "fn",
            CommentRole::FieldDoc => "field",
            CommentRole::Inline => "inline",
            CommentRole::Trailing => "trailing",
        }
    }
}

/// One comment run: where it is, what it documents, and how much it
/// says. Counts rather than text — see `crate::prose`.
pub struct CommentFact {
    /// 1-based line the run starts on.
    pub line: u32,
    pub role: CommentRole,
    /// Index into `units` when the run documents one, which only a
    /// `FnSummary` or an `Inline` comment does — a type and a field are
    /// not measured units.
    pub unit: Option<u32>,
    pub prose: Prose,
}

/// One class's cohesion: how many disconnected groups its methods
/// fall into, where two methods are connected when they touch a member
/// in common or one calls the other. One group is a cohesive class;
/// more means the class is several objects sharing a name.
pub struct ClassFact {
    pub name: Box<str>,
    pub line: u32,
    pub groups: u16,
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
    /// Whether a miss is a dependency or a failure to resolve.
    pub reach: crate::lang::Reach,
}

/// A sorted set of names, joined with `\u{1f}`, and where it appeared.
/// Used for two recurrences: the arms of a match, and the keys of an
/// anonymous record.
pub struct LabelSet {
    pub key: Box<str>,
    pub line: u32,
}

/// What a declaration's body amounts to.
///
/// A body that is one literal and nothing else declares nothing, and the
/// distinction between a boolean and any other literal is load-bearing:
/// naming a number is how a codebase AVOIDS magic numbers, which is why
/// `const_item` sits in every pack's `magic_exempt`. Naming a boolean
/// that asserts project state is a different act.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BodyShape {
    /// `{ true }`, `= false`, `:= true`.
    BoolLiteral,
    /// One literal of any other kind: `{ 1 }`, `= "v1"`.
    Literal,
    /// Documentation and nothing else — a decorator target, an abstract
    /// method, a protocol stub. All three are legitimate, so `ceremony`
    /// passes over them.
    Empty,
    /// Names or does something. The default, and the safe answer for a
    /// grammar whose definitions carry no `body` field.
    #[default]
    Real,
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
    /// Parameter names that documentation CLAIMS this unit takes, in no
    /// order and deduplicated. Empty unless the doc uses one of the
    /// naming conventions — see `crate::docparam`.
    pub documented_params: Box<[Box<str>]>,
    /// Whether this declaration's body says anything.
    pub body: BodyShape,
    /// A trait/interface default, or a method marked as overriding one.
    /// Only meaningful when `body` is not `Real` — see `open_unit`.
    pub is_override: bool,
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
    /// WHICH own members this method touches, deduplicated. Two
    /// methods that share none of them, and never call each other, are
    /// two objects wearing one class's name (Hitz & Montazeri's LCOM4).
    pub own_members: Vec<Box<str>>,
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
    /// Calls carrying two or more BARE boolean literals — `move(x,
    /// true, false)` — where the reader cannot bind a meaning to
    /// either. The declaration-side complement of `flag params`, and
    /// the only version that can see a third party's signature. A
    /// keyword argument (`strict=True`) is exempt: naming it at the
    /// call site IS the remedy.
    pub bool_traps: u16,
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
    /// Bound by a pattern rather than by a name — see
    /// `crate::lang::ParamInfo::destructured`.
    pub destructured: bool,
    /// Stands for arguments it does not name — see
    /// `crate::lang::ParamInfo::splat`.
    pub splat: bool,
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
