use std::path::Path;

use tree_sitter::{Node, Parser};

use super::{CloneSite, CommentFact, CommentRole, CtrlFact, FileFacts, UnitFacts};
use crate::facts::BodyShape;
use crate::lang::Pack;
use crate::sem::Sem;

/// Smallest normalized subtree recorded as a clone candidate. Below this,
/// matches are coincidence, not duplication.
const MIN_CLONE_MASS: u32 = 24;
const MIN_CLONE_LINES: u32 = 3;
/// Keys below which a record literal is a pair or a flag, not a shape
/// somebody should have named.
const MIN_SHAPE_KEYS: usize = 3;

/// Minimum control/call content: we hunt duplicated *logic*. Literal blobs
/// (data tables, generated models) collapse under Type-2 normalization but
/// duplicated data is content, not an engineering defect.
const MIN_CLONE_LOGIC: u32 = 3;

/// Tree depth past which extraction stops and the file is disqualified.
///
/// `walk` and `scan` recurse together, one frame pair per level, on a
/// rayon worker whose stack is ALREADY partly consumed by rayon's own
/// recursive splitter — so the budget is shared with a stranger. A tree
/// deeper than this is not something a person reads: it is a generated
/// blob, a minified bundle, or a left-nested operator chain as deep as
/// it is long. Refusing to measure it is the same call `low_confidence`
/// already makes about a file that would not parse.
const MAX_TREE_DEPTH: u16 = 300;

/// Share of a comment's words that must appear in the code beside it
/// before the comment counts as an echo of that code.
const ECHO_SHARE: f32 = 0.65;

pub fn extract(pack: &Pack, parser: &mut Parser, path: &Path, source: &str) -> FileFacts {
    let mut blank = Rows::new(source.lines().count());
    for (row, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            blank.set(row);
        }
    }
    let lines = blank.len as u32;
    let mut facts = FileFacts {
        path: path.to_path_buf(),
        lang: pack.lang,
        lines,
        blank_lines: blank.count() as u32,
        comment_lines: 0,
        parse_errors: 0,
        too_deep: false,
        is_test_file: pack.test_path(&path.display().to_string()),
        is_script_file: runs_once(&path.display().to_string()),
        mass: 0,
        units: vec![UnitFacts {
            name: "<module>".into(),
            qualname: "<module>".into(),
            line: 1,
            lines,
            is_module: true,
            ..UnitFacts::blank()
        }],
        clone_sites: Vec::new(),
        echo_comments: Vec::new(),
        suppressions: Vec::new(),
        spooky_lines: Vec::new(),
        secrets: Vec::new(),
        sql_built: Vec::new(),
        shelled_out: Vec::new(),
        debt_markers: Vec::new(),
        commented_code: Vec::new(),
        skipped_tests: Vec::new(),
        magic_strings: Vec::new(),
        test_refs: Vec::new(),
        switch_sigs: Vec::new(),
        record_shapes: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        mentioned: Vec::new(),
        step_refs: (0, 0),
        pub_order: (0, 0),
        classes: Vec::new(),
        interfaces: Vec::new(),
        comments: Vec::new(),
    };
    let Some(tree) = parser.parse(source, None) else {
        facts.parse_errors = 1;
        return facts;
    };
    let mut ex = Extractor {
        pack,
        src: source.as_bytes(),
        seed: mix(0, pack.lang as u64 + 1),
        commentary: Rows::new(blank.len),
        live: vec![LiveMap::new()],
        declared: vec![std::collections::HashSet::new()],
        envy: vec![std::collections::HashMap::new()],
        callees: vec![Vec::new()],
        self_names: vec!["".into()],
        tokens: vec![Vec::new()],
        import_roots: std::collections::HashSet::new(),
        chain_roots: Vec::new(),
        mentions: std::collections::HashSet::new(),
        comment_lines: Vec::new(),
        comments_through: 0,
        doc_targets: Vec::new(),
        stray_calls: Vec::new(),
        string_rows: std::collections::HashMap::new(),
        facts: &mut facts,
    };
    let root = ex.walk(
        tree.root_node(),
        Ctx {
            unit: 0,
            cog: 0,
            vis: 0,
            loops: 0,
            branched: false,
            sheltered: false,
            awaited: false,
            depth: 0,
        },
    );
    let mass = root.map_or(0, |sub| sub.mass);
    finish(ex, &blank, mass);
    facts
}

/// Every post-walk join: work that needs the whole file's units known
/// before it can say anything (asyncness of callees, envy targets,
/// live spans), plus the counts the walk only accumulated.
fn finish(ex: Extractor, blank: &Rows, mass: u32) {
    let Extractor {
        commentary,
        live,
        declared,
        envy,
        callees,
        import_roots,
        chain_roots,
        mentions,
        tokens,
        stray_calls,
        comment_lines,
        doc_targets,
        string_rows,
        facts,
        pack,
        ..
    } = ex;
    facts.magic_strings = repeated_strings(string_rows);
    attach_docs(facts, &doc_targets);
    facts.commented_code = commented_out_code(pack, &comment_lines);
    facts.classes = class_cohesion(&facts.units, &callees, pack.scope_sep);
    fingerprint_units(facts, &tokens);
    resolve_unawaited(facts, &stray_calls);
    facts.mentioned = mentions.into_iter().collect();
    facts.step_refs = step_refs(&facts.units, &callees);
    facts.pub_order = pub_order(&facts.units);
    resolve_envy(&mut facts.units, envy, &import_roots, &declared);
    resolve_chains(&mut facts.units, chain_roots, &import_roots);
    facts.test_refs = resolve_locals(&mut facts.units, live, facts.is_test_file);
    facts.mass = mass;
    facts.comment_lines = commentary.count_excluding(blank) as u32;
    let unit_syms = public_unit_names(&facts.units);
    facts.exports.extend(unit_syms);
}

/// Join each function summary to the unit it introduces. The
/// definition is opened AFTER its documentation is read, so the walk
/// can only record which line it starts on; the units it produced are
/// known here.
fn attach_docs(facts: &mut FileFacts, targets: &[u32]) {
    if targets.iter().all(|t| *t == 0) {
        return;
    }
    let mut by_line: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for (i, u) in facts.units.iter().enumerate().skip(1) {
        by_line.entry(u.line).or_insert(i as u32);
    }
    for (fact, target) in facts.comments.iter_mut().zip(targets) {
        if *target != 0 {
            fact.unit = by_line.get(target).copied();
        }
    }
}

/// Same-file resolution of statement-position unawaited calls. A name
/// is judged only when it is UNAMBIGUOUS: every same-file unit wearing
/// it must be async, or the call stays silent — a sync twin means no
/// claim can be made without types.
///
/// Python and Rust ONLY, because the claim is "this body never ran":
/// a Python coroutine is created inert and a Rust future drops
/// unpolled, but a JS promise is eagerly scheduled — the call RUNS
/// and only its rejection goes unobserved. That weaker claim belongs
/// to no-floating-promises, and gold showed admired code violating it
/// deliberately: 144 fire-and-forget telemetry sends in vscode alone.
fn resolve_unawaited(facts: &mut FileFacts, stray: &[(usize, Box<str>)]) {
    if stray.is_empty()
        || !matches!(
            facts.lang,
            crate::lang::Lang::Python | crate::lang::Lang::Rust
        )
    {
        return;
    }
    let mut always_async: std::collections::HashMap<Box<str>, bool> =
        std::collections::HashMap::new();
    for u in facts.units.iter().filter(|u| !u.is_module) {
        always_async
            .entry(u.name.clone())
            .and_modify(|v| *v &= u.is_async)
            .or_insert(u.is_async);
    }
    for (unit, name) in stray {
        if always_async.get(name.as_ref()).copied() == Some(true) {
            facts.units[*unit].unawaited += 1;
        }
    }
}

/// Per-unit work that needs the whole file walked first: winnowed
/// fingerprints, and the hook rules — counted during the walk, because
/// only there is the branch context known, and withdrawn here if the
/// import list turns out not to be React's.
fn fingerprint_units(facts: &mut FileFacts, tokens: &[Vec<u64>]) {
    let react = uses_react(facts);
    for (unit, stream) in facts.units.iter_mut().zip(tokens) {
        if stream.len() >= crate::near::MIN_TOKENS {
            unit.fingerprints = crate::near::fingerprints(stream);
        }
        if !react {
            unit.conditional_hooks = 0;
        }
    }
}

/// The foreign receiver each unit touches most, which is the one Feature
/// Envy is about. Name tie-break so HashMap order cannot leak into output.
///
/// FOREIGN is the load-bearing word. Fowler's smell is a method more
/// interested in ANOTHER class's data than in its own; a method cannot
/// be envying an object it allocated three lines earlier, because that
/// object is its own working state and there is nowhere to move the
/// method to. `JsonReader reader = new JsonTextReader(..); reader.Read();
/// reader.Read();` was the largest single shape in the metric — 8,035 of
/// 19,424 gold findings named a target the flagging unit had DECLARED,
/// counted exactly over the whole population rather than sampled.
///
/// `declared` already knows, and knows narrowly: a pattern that reaches
/// through a member access binds nothing, so Ruby's
/// `@config.cache[k] = 1` does not make `@config` this unit's own.
/// Parameters are deliberately NOT in any pack's `def_sites`, so a
/// receiver passed in — Zig's `fn parse(parser: *Parser)` — is
/// untouched by this rule and stays a separate question.
fn resolve_envy(
    units: &mut [UnitFacts],
    envy: Vec<std::collections::HashMap<Box<str>, u16>>,
    import_roots: &std::collections::HashSet<Box<str>>,
    declared: &[std::collections::HashSet<Box<str>>],
) {
    for ((unit, foreign), locals) in units.iter_mut().zip(envy).zip(declared) {
        for (name, count) in foreign {
            // Imported roots are modules in disguise (os, np, std) —
            // reaching into a module is not Feature Envy of an object.
            if import_roots.contains(&name) {
                continue;
            }
            // A local this unit declared is its own, not a neighbour's.
            if locals.contains(&name) {
                continue;
            }
            if count > unit.envy_count
                || (count == unit.envy_count && count > 0 && name < unit.envy_object)
            {
                unit.envy_count = count;
                unit.envy_object = name;
            }
        }
    }
}

/// Withdraw every Demeter chain rooted at an imported name.
///
/// `torch.nn.functional.pad(x)` is namespace.namespace.namespace.function
/// — three qualifiers on one call, not three data links into a
/// neighbour's internals — and Lieberherr's rule is about a neighbour's
/// STRUCTURE. `resolve_envy` has always applied exactly this rule one
/// branch away, from the same set; the chain count did not, and 520 of
/// the 890 gold Python findings had every chain rooted at an import.
///
/// Withdrawn afterwards rather than skipped during the walk because a
/// file may import below the code that uses the name (a Python function
/// importing lazily inside its body, a re-export at the foot of a
/// module), and a rule that depended on walk order would judge those
/// differently for no reason a reader could see.
fn resolve_chains(
    units: &mut [UnitFacts],
    chains: Vec<(usize, Box<str>)>,
    import_roots: &std::collections::HashSet<Box<str>>,
) {
    for (unit, base) in chains {
        if import_roots.contains(&base) {
            units[unit].demeter = units[unit].demeter.saturating_sub(1);
        }
    }
}

/// Longest local live span per unit (definition to last mention), and
/// every name test code touches — the join key for untested complexity.
fn resolve_locals(units: &mut [UnitFacts], live: Vec<LiveMap>, test_file: bool) -> Vec<Box<str>> {
    let mut test_refs = std::collections::HashSet::new();
    for (unit, locals) in units.iter_mut().zip(live) {
        if unit.is_test || (test_file && unit.is_module) {
            // Every name a test mentions is a name the test touches —
            // including the test file's module scope (imports, fixtures).
            test_refs.extend(locals.keys().cloned());
        }
        for (name, (def, last)) in locals {
            let Some(def) = def else { continue };
            let span = last.saturating_sub(def) as u16;
            if span > unit.max_live_span
                || (span == unit.max_live_span && span > 0 && name < unit.max_live_var)
            {
                unit.max_live_span = span;
                unit.max_live_var = name;
            }
        }
    }
    test_refs.into_iter().collect()
}

/// Public units join the export surface next to the types the walk
/// collected.
fn public_unit_names(units: &[UnitFacts]) -> Vec<Box<str>> {
    units
        .iter()
        .filter(|u| u.is_public && !u.is_module && !u.is_test)
        .map(|u| u.name.clone())
        .collect()
}

/// Line-membership bitmap; marking is idempotent, so overlapping comment and
/// docstring spans never double count.
struct Rows {
    len: usize,
    bits: Vec<u64>,
}

impl Rows {
    fn new(len: usize) -> Rows {
        Rows {
            len,
            bits: vec![0; len.div_ceil(64)],
        }
    }

    fn set(&mut self, row: usize) {
        if row < self.len {
            self.bits[row / 64] |= 1 << (row % 64);
        }
    }

    fn count(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    fn count_excluding(&self, other: &Rows) -> usize {
        self.bits
            .iter()
            .zip(&other.bits)
            .map(|(a, b)| (a & !b).count_ones() as usize)
            .sum()
    }
}

#[cfg(test)]
pub(super) fn blank_unit() -> UnitFacts {
    UnitFacts::blank()
}

impl UnitFacts {
    fn blank() -> UnitFacts {
        UnitFacts {
            name: "".into(),
            qualname: "".into(),
            line: 0,
            lines: 0,
            body: BodyShape::Real,
            is_override: false,
            is_module: false,
            is_method: false,
            is_public: false,
            params: Vec::new(),
            doc_lines: 0,
            documented_params: Box::new([]),
            max_vis_depth: 0,
            max_expr_depth: 0,
            magic_numbers: 0,
            max_live_span: 0,
            max_live_var: "".into(),
            demeter: 0,
            negations: 0,
            is_passthrough: false,
            self_accesses: 0,
            own_members: Vec::new(),
            envy_count: 0,
            envy_object: "".into(),
            swallowed: 0,
            broad_catch: 0,
            lost_context: 0,
            unwraps: 0,
            casts: 0,
            is_async: false,
            blocking_calls: 0,
            sleep_calls: 0,
            bool_traps: 0,
            fingerprints: Vec::new(),
            max_loop_depth: 0,
            allocs_in_loop: 0,
            conditional_hooks: 0,
            dropped_tasks: 0,
            wildcard_matches: 0,
            is_test: false,
            named_test: false,
            assert_calls: 0,
            vacuous_asserts: 0,
            returns: "".into(),
            return_arity: 0,
            repurposed: 0,
            unawaited: 0,
            mut_receiver: false,
            self_recursive: false,
            ctrl: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct Ctx {
    unit: usize,
    cog: u8,
    vis: u16,
    /// How many loops enclose this node, within its own unit.
    loops: u8,
    /// Is this node reached only when some condition holds? A hook
    /// call here is a React rules-of-hooks violation.
    branched: bool,
    /// Inside a `try` body. Not a fork — the body runs unconditionally
    /// — but an exception may abandon it halfway, so a reassignment
    /// here leaves the OLD value live on the handler path and cannot
    /// be judged a repurposing.
    sheltered: bool,
    /// Under an `await`. Deliberately covers the WHOLE subtree: a call
    /// nested in an awaited expression (`await gather(f(), g())`) has
    /// handed its coroutine to a consumer, and silence there is the
    /// right direction to be wrong in.
    awaited: bool,
    /// Nodes between here and the root. `walk` and `scan` are mutually
    /// recursive, so this is also the stack depth they are using.
    depth: u16,
}

/// Bottom-up subtree summary: normalized hash, named-node count, height,
/// and how much of the mass is control flow and calls rather than data.
#[derive(Clone, Copy)]
struct Sub {
    hash: u64,
    mass: u32,
    logic: u32,
    height: u16,
}

/// What a comment run documents.
///
/// A unit the walk already opened, or — for the summary above a
/// definition — the LINE that definition starts on, since the walk
/// reads the documentation before it opens the thing documented.
enum Attach {
    Unit(u32),
    AtLine(u32),
    Nothing,
}

/// Local name -> (definition row if bound here, last-seen row); shadowing
/// approximated by name (first def wins).
type LiveMap = std::collections::HashMap<Box<str>, (Option<u32>, u32)>;

/// The object a method calls its own, for grammars that declare it apart
/// from the parameter list, plus the type it hangs off.
struct Receiver {
    name: Box<str>,
    type_name: String,
}

struct Extractor<'a> {
    pack: &'a Pack,
    src: &'a [u8],
    /// Language-discriminated hash seed: kind ids from different grammars
    /// share a numeric space and must never collide across languages.
    seed: u64,
    commentary: Rows,
    /// One live map per unit, parallel to `facts.units`.
    live: Vec<LiveMap>,
    /// Names each unit BOUND, parallel to `facts.units`. Narrower than
    /// the live map's definition rows, which a language without a
    /// declaration keyword also writes for `cfg.field = v` — that
    /// statement rebinds nothing, and Feature Envy has to know the
    /// difference. See [`Extractor::record_defs`].
    declared: Vec<std::collections::HashSet<Box<str>>>,
    /// Foreign-receiver access counts per unit, parallel to `facts.units`.
    envy: Vec<std::collections::HashMap<Box<str>, u16>>,
    /// Bare-name callees per unit, parallel to `facts.units` — the raw
    /// material of the step-down narrative join.
    callees: Vec<Vec<Box<str>>>,
    /// What each unit calls its own object, parallel to `facts.units`
    /// (`self`, `cls`, or a Go receiver's chosen name). Empty for free
    /// functions.
    self_names: Vec<Box<str>>,
    /// Normalized token stream per unit, parallel to `facts.units`. The
    /// same keys the Merkle hash is built from, kept linear so an edit
    /// in the middle can be survived rather than fatal.
    tokens: Vec<Vec<u64>>,
    /// Local names bound by imports — modules wearing value names.
    import_roots: std::collections::HashSet<Box<str>>,
    /// (unit, base name) of every counted Demeter chain, kept until the
    /// whole file's imports are known — a chain rooted at an import is
    /// withdrawn in [`resolve_chains`].
    chain_roots: Vec<(usize, Box<str>)>,
    /// Every distinct identifier the file mentions, for the dead-export
    /// join. Deduplicated here so the aggregate counts FILES per name.
    mentions: std::collections::HashSet<Box<str>>,
    /// Statement-position bare-name calls made without an await, result
    /// discarded: (calling unit, callee name). Resolved after the walk,
    /// when every unit's asyncness is known.
    stray_calls: Vec<(usize, Box<str>)>,
    /// Every non-doc comment line, stripped of its marker, with the row
    /// it sat on. Adjacency — and therefore blocks — is only visible
    /// once the file is read, so the grouping happens after the walk.
    comment_lines: Vec<(u32, String)>,
    /// First row no comment run has claimed yet. A run is gathered
    /// whole at its first node, so the rest of its nodes arrive already
    /// accounted for and must not open a second run.
    comments_through: usize,
    /// The definition each comment run introduces, as a 1-based line,
    /// parallel to `facts.comments` (0 where it introduces nothing).
    /// Resolved to a unit index after the walk, because the definition
    /// a doc comment describes is opened after the comment is read.
    doc_targets: Vec<u32>,
    /// Repeatable string literals, each with the rows and the UNITS it
    /// appeared in. A repeat is only visible once the whole file is
    /// read, so the counting happens after the walk.
    string_rows: std::collections::HashMap<Box<str>, Vec<(u32, usize)>>,
    facts: &'a mut FileFacts,
}

impl Extractor<'_> {
    fn sem_of(&self, node: Node) -> Sem {
        self.pack.sem_of(node, self.src)
    }

    /// Is this declaration an override point?
    ///
    /// Two mechanisms. The pack answers for an explicit marker —
    /// `@Override`, `override`, a method inside `impl Trait for` — which
    /// is the only route when the enclosing type is an ordinary class.
    /// Beyond that, a member of a declaration that `interfaces`
    /// recognises is a default for implementors, and that generic check
    /// costs the packs nothing.
    ///
    /// Both matter to `ceremony`, whose 79 gold false positives were
    /// every one of them a documented trait default.
    fn is_override_point(&self, node: Node) -> bool {
        if self.pack.is_override(node, self.src) {
            return true;
        }
        let mut anc = node.parent();
        while let Some(a) = anc {
            if self.sem_of(a) == Sem::TypeDef {
                return !self.pack.interfaces(a, self.src).is_empty();
            }
            anc = a.parent();
        }
        false
    }

    /// Is this declaration's body one literal and nothing else?
    ///
    /// Python and Ruby keep the docstring INSIDE the body, so a
    /// documented function's first statement is prose rather than work
    /// and has to be set aside — otherwise every documented Python stub
    /// reads as two literals and escapes the metric that wants it.
    ///
    /// A grammar whose definitions carry no `body` field reads as
    /// `Real`, which loses the finding rather than inventing one.
    fn body_shape(&self, node: Node) -> BodyShape {
        let Some(body) = node.child_by_field_name("body") else {
            return BodyShape::Real;
        };
        let mut cursor = body.walk();
        let mut kids = body.named_children(&mut cursor);
        let Some(first) = kids.next() else {
            return BodyShape::Real;
        };
        let rest: Vec<Node> = kids.collect();
        let doc_first = self.pack.docs_inside_body && self.only_literal(first) == Some(Sem::StrLit);
        let (head, tail) = match (doc_first, rest.split_first()) {
            // `def f(): """docs"""` — a decorator supplies the
            // behaviour, and the body is prose.
            (true, None) => return BodyShape::Empty,
            (true, Some((second, others))) => (*second, others),
            _ => (first, &rest[..]),
        };
        if !tail.is_empty() {
            return BodyShape::Real;
        }
        match self.only_literal(head) {
            Some(Sem::BoolLit) => BodyShape::BoolLiteral,
            Some(_) => BodyShape::Literal,
            None => BodyShape::Real,
        }
    }

    /// The one literal this subtree amounts to, if that is all it is.
    ///
    /// Aborts on the first node that names or does anything, so an
    /// ordinary statement costs a visit or two. A SECOND literal also
    /// disqualifies: `(1, 2)` builds something.
    fn only_literal(&self, node: Node) -> Option<Sem> {
        let mut found = None;
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            match self.sem_of(n) {
                sem @ (Sem::BoolLit | Sem::NumLit | Sem::StrLit) => {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(sem);
                }
                // A block, an expression statement, a `return`: these
                // carry the literal rather than being it.
                Sem::None | Sem::Jump => {
                    let mut c = n.walk();
                    stack.extend(n.named_children(&mut c));
                }
                _ => return None,
            }
        }
        found
    }

    /// Returns the subtree summary, or None for commentary (comments and doc
    /// strings never contribute to clone identity or expression height).
    fn walk(&mut self, node: Node, ctx: Ctx) -> Option<Sub> {
        if ctx.depth > MAX_TREE_DEPTH {
            self.facts.too_deep = true;
            return None;
        }
        if node.is_error() || node.is_missing() {
            self.facts.parse_errors += 1;
        }
        let sem = self.sem_of(node);
        match sem {
            Sem::Comment => {
                self.mark_commentary(node);
                self.check_echo(node);
                self.check_suppression(node);
                self.keep_comment_text(node);
                self.record_comment_run(node, ctx);
                None
            }
            Sem::None if self.pack.is_doc(node) => {
                self.mark_commentary(node);
                self.record_docstring(node, ctx);
                None
            }
            Sem::FnDef => {
                let unit = self.open_unit(node);
                Some(self.scan(
                    node,
                    Ctx {
                        unit,
                        cog: 0,
                        vis: 0,
                        loops: 0,
                        branched: false,
                        sheltered: false,
                        awaited: false,
                        depth: ctx.depth,
                    },
                    sem,
                ))
            }
            _ => {
                self.record(node, sem, ctx);
                let mut inner = ctx;
                inner.cog = ctx.cog.saturating_add(sem.nests_cognitive() as u8);
                // Anything below a decision runs only sometimes. A loop
                // counts too: React's hook identity is call ORDER, and
                // a loop of variable length shifts it exactly as a
                // branch does.
                inner.branched = ctx.branched || sem.forks_control();
                inner.sheltered = ctx.sheltered || sem == Sem::Try;
                inner.awaited = ctx.awaited || sem == Sem::Await;
                if sem == Sem::Loop {
                    inner.loops = ctx.loops.saturating_add(1);
                    let unit = &mut self.facts.units[ctx.unit];
                    unit.max_loop_depth = unit.max_loop_depth.max(inner.loops as u16);
                }
                if sem.nests_visual() {
                    inner.vis = ctx.vis + 1;
                    let unit = &mut self.facts.units[ctx.unit];
                    unit.max_vis_depth = unit.max_vis_depth.max(inner.vis);
                }
                Some(self.scan(node, inner, sem))
            }
        }
    }

    /// Everything one node contributes to the facts, before descending
    /// into it. Nesting is the caller's business; this is the ledger.
    fn record(&mut self, node: Node, sem: Sem, ctx: Ctx) {
        // Record literals carry no Sem of their own — they are shaped by
        // their keys, not by what they do.
        self.record_shape(node);
        match sem {
            Sem::Call => self.record_call(node, ctx),
            // Catch nodes, error-as-value If checks and the Try forms
            // that carry their own handler are one family.
            Sem::If | Sem::Catch | Sem::Try => self.record_error_handling(node, sem, ctx.unit),
            Sem::NumLit if self.is_magic(node) => {
                self.facts.units[ctx.unit].magic_numbers += 1;
            }
            Sem::Import => self.record_imports(node),
            Sem::TypeDef => {
                self.record_type_export(node);
                self.record_interfaces(node);
            }
            Sem::Ident => self.record_ident(node, ctx.unit),
            Sem::Match => self.record_switch_sig(node, ctx.unit),
            Sem::Cast => self.facts.units[ctx.unit].casts += 1,
            Sem::Assert => self.record_assert(node, ctx.unit),
            Sem::StrLit => {
                self.record_secret(node, ctx.unit);
                self.record_repeatable_string(node, ctx.unit);
                self.check_built_query(node, ctx.unit);
            }
            _ => {}
        }
        self.record_ctrl(node, sem, ctx);
        // Asked of EVERY node, because what escapes the language is
        // not always a call: Python's metaclass is a TypeDef, Perl's
        // `eval "..."` is the same node as its try block, and
        // Solidity's `assembly { }` is a statement with no Sem at all.
        // Each pack gates on the sem or kind it cares about.
        if self.pack.spooky(node, sem, self.src) {
            self.facts
                .spooky_lines
                .push(node.start_position().row as u32 + 1);
        }
        self.record_bindings(node, ctx);
        self.check_demeter(node, ctx.unit);
        self.check_negation(node, ctx.unit);
    }

    /// Both faces of a binding site. Repurposing first: the live map
    /// must still describe the world BEFORE this assignment, or a
    /// first binding reads as its own repurposing.
    fn record_bindings(&mut self, node: Node, ctx: Ctx) {
        if let Some(field) = self.pack.reassign_field(node.kind_id()) {
            self.check_repurposing(node, field, ctx);
        }
        if let Some(field) = self.pack.def_field(node.kind_id()) {
            self.record_defs(node, field, ctx.unit);
        }
    }

    /// Fowler's Split Variable, judged where it is decidable: a
    /// straight-line `x = ...` whose new value never mentions the old
    /// gives the SAME name a SECOND meaning, and every earlier read
    /// the reader remembers is now silently wrong. Everything that
    /// keeps or guards the meaning is exempt — collecting updates
    /// (`x = x + 1`, `s = s.trim()`), compound operators, conditional
    /// overrides (a branch chooses a value, not a meaning), loop-body
    /// refills, and try-sheltered fills whose old value survives on
    /// the handler path.
    fn check_repurposing(&mut self, node: Node, field: &str, ctx: Ctx) {
        if ctx.branched || ctx.sheltered {
            return;
        }
        // A class body declares ATTRIBUTES OF A TYPE, and a type body
        // opens no unit of its own, so every class in a file shares the
        // module's live map. Without this, click's `name = "integer"`
        // and `name = "boolean"` — attributes of two different classes —
        // read as one rewriting the other, ten times over in one file.
        if self.enclosing_scope_is_class(node) {
            return;
        }
        let Some(target) = single_reassign_target(self.pack, node, field) else {
            return;
        };
        let Ok(name) = target.utf8_text(self.src) else {
            return;
        };
        if name.starts_with('_') || !self.plainly_assigns(node, target) {
            return;
        }
        let row = node.start_position().row as u32 + 1;
        let prior = self.live[ctx.unit].get(name).and_then(|entry| entry.0);
        if prior.is_none_or(|def| def >= row) {
            return;
        }
        if self.value_mentions(node, target, name) {
            return;
        }
        self.facts.units[ctx.unit].repurposed += 1;
    }

    /// Is the operator a bare `=`? Go, C and shell spell `+=` inside
    /// the same node kind, so the byte after the target decides —
    /// anything else (`+`, `<`, a type annotation's `:`) keeps the old
    /// value in the story and is not a repurposing.
    fn plainly_assigns(&self, node: Node, target: Node) -> bool {
        let after = self.src[target.end_byte()..node.end_byte()]
            .iter()
            .find(|b| !b.is_ascii_whitespace());
        matches!(after, Some(b'='))
    }

    /// Does anything OUTSIDE the target subtree mention the name? A
    /// value built from the old value is a transformation of one
    /// meaning, not a second meaning.
    fn value_mentions(&self, node: Node, target: Node, name: &str) -> bool {
        let mut cursor = node.walk();
        let mut stack: Vec<Node> = node
            .named_children(&mut cursor)
            .filter(|c| c.start_byte() > target.end_byte() || c.end_byte() <= target.start_byte())
            .collect();
        while let Some(n) = stack.pop() {
            if self.pack.table_sem(n) == Sem::Ident && n.utf8_text(self.src) == Ok(name) {
                return true;
            }
            let mut c = n.walk();
            for child in n.named_children(&mut c) {
                stack.push(child);
            }
        }
        false
    }

    /// The key set of an anonymous record, when it has enough keys to be
    /// a shape rather than a pair.
    fn record_shape(&mut self, node: Node) {
        let Some(mut keys) = self.pack.record_keys(node, self.src) else {
            return;
        };
        if keys.len() < MIN_SHAPE_KEYS {
            return;
        }
        keys.sort_unstable();
        let joined: Vec<&str> = keys.iter().map(|k| &**k).collect();
        self.facts.record_shapes.push(super::LabelSet {
            key: joined.join("\u{1f}").into(),
            line: node.start_position().row as u32 + 1,
        });
    }

    /// A control event, tagged with the cognitive nesting depth it sits
    /// at. Chained boolean operators of one kind count as one sequence.
    fn record_ctrl(&mut self, node: Node, sem: Sem, ctx: Ctx) {
        if !sem.is_ctrl() {
            return;
        }
        let new_seq = sem != Sem::BoolOp || self.bool_starts_seq(node);
        self.facts.units[ctx.unit].ctrl.push(CtrlFact {
            sem,
            line: node.start_position().row as u32 + 1,
            cog_depth: ctx.cog,
            new_seq,
        });
    }

    /// How a handler mishandles its error, if it does. Exception
    /// languages hand us a Catch node; error-as-value languages swallow
    /// in an If (`if err != nil { }`), which the pack recognizes.
    fn record_error_handling(&mut self, node: Node, sem: Sem, unit_idx: usize) {
        // Some languages have no handler NODE at all: Go swallows in an
        // `if err != nil {}` and Perl in an `eval` whose `$@` nobody
        // reads, so the pack is asked about the construct standing in
        // for one. A Catch answers through `catch_sin` instead, whose
        // Swallowed verdict states the same fact once.
        if sem != Sem::Catch {
            let swallowed = self.pack.swallows_error(node, self.src);
            self.facts.units[unit_idx].swallowed += swallowed as u16;
            if sem == Sem::If {
                return;
            }
        }
        let lost = self.pack.loses_context(node, self.src);
        let unit = &mut self.facts.units[unit_idx];
        unit.lost_context += lost as u16;
        match self.pack.catch_sin(node, self.src) {
            Some(crate::lang::CatchSin::Swallowed) => unit.swallowed += 1,
            Some(crate::lang::CatchSin::Broad) => unit.broad_catch += 1,
            None => {}
        }
    }

    fn record_call(&mut self, node: Node, ctx: Ctx) {
        let unit_idx = ctx.unit;
        if let Some(name) = self.callee_simple_name(node).map(Box::<str>::from) {
            // A statement-position call, unawaited, result thrown away.
            // If the name resolves to a same-file async unit once the
            // whole file is walked, the coroutine was created and
            // dropped — it never ran.
            if !ctx.awaited && discards_its_result(node) {
                self.stray_calls.push((unit_idx, name.clone()));
            }
            self.callees[unit_idx].push(name);
        }
        self.record_call_kind(node, unit_idx);
        self.check_shelled_command(node, unit_idx);
        self.record_lifetime(node, unit_idx);
        self.record_placement(node, ctx);
        // `t.Skip()` and `it.skip(...)` are calls; `#[ignore]` and a
        // decorator are caught at open_unit instead. Branch context
        // decides: `if runtime.GOOS == "windows" { t.Skip() }` is
        // stated judgment, and only an UNCONDITIONAL skip suppresses.
        if !ctx.branched && self.pack.skips_test(node, self.src) {
            self.note_skipped_test(node);
        }
    }

    /// What a call IS, judged by the callee alone: a panic where an
    /// error belonged, an assertion, a recursion, a park, a sleep.
    fn record_call_kind(&mut self, node: Node, unit_idx: usize) {
        let asserts = self.pack.asserty(node, self.src);
        let vacuous = asserts && self.asserts_a_literal(node);
        let sleeps = self.is_a_sleep(node);
        let panics = self.pack.panicky(node, self.src);
        let trap = self.bare_boolean_arguments(node) >= MIN_BOOL_TRAP;
        let unit = &self.facts.units[unit_idx];
        // NOT gated on `ctx.awaited`. The verdict asked for that as a
        // cheap stand-in for the name table below, on the evidence that
        // eight of the suffix-rule's false positives were awaited — but
        // once the table decides, awaiting proves nothing: `await
        // fs.readFileSync(p)` and `await requests.get(url)` block
        // exactly as hard, and the guard silenced both. Measured, not
        // reasoned: a two-file probe reported zero with it in place.
        let parks = unit.is_async && self.parks_the_thread(node);
        let recursive = !unit.self_recursive && self.pack.is_self_call(node, self.src, &unit.name);
        let unit = &mut self.facts.units[unit_idx];
        unit.unwraps += panics as u16;
        unit.assert_calls += asserts as u16;
        unit.vacuous_asserts += vacuous as u16;
        unit.blocking_calls += parks as u16;
        unit.sleep_calls += sleeps as u16;
        unit.bool_traps += trap as u16;
        unit.self_recursive |= recursive;
    }

    /// Boolean literals passed POSITIONALLY. A keyword argument names
    /// its meaning at the call site, which is the remedy this metric
    /// asks for, so `f(x, strict=True)` is not a trap however many
    /// booleans follow.
    fn bare_boolean_arguments(&self, call: Node) -> usize {
        call_arguments(self.pack, call)
            .into_iter()
            .filter(|a| self.pack.table_sem(*a) == Sem::BoolLit)
            .count()
    }

    /// What a call's SURROUNDINGS make of it: a copy rebuilt on every
    /// iteration, or a hook whose identity a branch renumbers.
    fn record_placement(&mut self, node: Node, ctx: Ctx) {
        let unit = &self.facts.units[ctx.unit];
        // React's rule is "call hooks only from a component or a custom
        // hook", so the ENCLOSING unit decides whether it applies at
        // all. Without that, every factory returning hooks and every
        // unrelated `useCustomProperty()` in vscode's electron-main
        // read as violations.
        let stray_hook =
            ctx.branched && holds_hooks(&unit.name) && self.pack.is_hook(node, self.src);
        let copy_per_iteration = ctx.loops > 0 && self.allocates_a_copy(node);
        let unit = &mut self.facts.units[ctx.unit];
        unit.conditional_hooks += stray_hook as u16;
        unit.allocs_in_loop += copy_per_iteration as u16;
    }

    /// A sleep of any flavour, judged by the trailing name alone —
    /// `time.sleep`, `asyncio.sleep`, `thread::sleep`,
    /// `tokio::time::sleep`, Go's `time.Sleep`, shell's `sleep`, and
    /// C++'s `std::this_thread::sleep_for`. That is deliberately the
    /// crude rule `blocking async` had to abandon: there the qualifier
    /// decides, because a runtime sleep is the remedy; here every
    /// flavour is the same fact, and only a TEST reads it.
    fn is_a_sleep(&self, call: Node) -> bool {
        matches!(
            self.callee_trailing_name(call),
            Some("sleep" | "Sleep" | "sleep_for" | "sleep_until")
        )
    }

    /// Does this call allocate a fresh copy? Only the names that mean
    /// nothing else: `to_string`, `to_owned`, `to_vec` and `deepcopy`
    /// each exist BECAUSE the alternative is borrowing, so one inside a
    /// loop is a copy per iteration that the author may not have priced.
    /// `clone` is deliberately absent — it is often the only way to
    /// satisfy the borrow checker, and flagging it would be noise.
    fn allocates_a_copy(&self, call: Node) -> bool {
        matches!(
            self.callee_trailing_name(call),
            Some("to_string" | "to_owned" | "to_vec" | "deepcopy")
        )
    }

    /// A task spawned with nothing arranging its end: the handle is
    /// thrown away, so nothing can await it and nothing observes its
    /// panic.
    fn record_lifetime(&mut self, node: Node, unit_idx: usize) {
        let dropped = self.callee_trailing_name(node) == Some("spawn") && discards_its_result(node);
        self.facts.units[unit_idx].dropped_tasks += dropped as u16;
    }

    /// `assert True` as a statement: the subject is the first child, and
    /// a literal there means the check passes whatever the code did.
    fn record_assert(&mut self, node: Node, unit_idx: usize) {
        let literal = node
            .named_child(0)
            .is_some_and(|subject| self.is_literal(subject));
        if literal {
            self.facts.units[unit_idx].vacuous_asserts += 1;
        }
    }

    fn is_literal(&self, node: Node) -> bool {
        matches!(
            self.pack.table_sem(node),
            Sem::BoolLit | Sem::NumLit | Sem::StrLit
        )
    }

    /// Does this assertion check a literal? `assert!(true)` and
    /// `assertTrue(1)` pass whatever the code under test did, so the
    /// test is green by construction. A literal in SECOND position is
    /// the expected value of a real comparison and is fine.
    ///
    /// Three things a single literal argument does NOT mean, each
    /// measured on gold before it was excluded:
    ///
    /// - **The literal is the EXPECTED value of a fluent assertion.**
    ///   `resultA.Name.ShouldBe("name1")` puts the subject on the
    ///   RECEIVER, so the argument is the answer rather than the
    ///   question. C#'s pack accepts any member starting `Should`, and
    ///   this one shape was 2,116 of the metric's 3,116 gold findings —
    ///   the whole reason C# read 24% of its test units.
    /// - **The literal is DATA the helper operates on.** A project's
    ///   own `assert_file("lib/my_app/accounts.ex")` is a parameterised
    ///   case, never a tautology; `assertish` matches any name with an
    ///   `assert` in it, so every such helper qualified.
    /// - **The call is not the whole assertion.** ZIO writes
    ///   `assert(11)(equalTo(12))`, where `assert(11)` is the SUBJECT
    ///   of the assertion that follows it.
    fn asserts_a_literal(&self, call: Node) -> bool {
        let [only] = call_arguments(self.pack, call)[..] else {
            return false;
        };
        // A string is data. Numbers stay: Perl and C have no boolean
        // literal, and `ok(1)` is exactly the tautology this counts.
        if self.pack.table_sem(only) == Sem::StrLit {
            return false;
        }
        self.is_literal(only)
            && self.assertion_owns_its_argument(call)
            && !self.is_applied_again(call)
    }

    /// Is this call the SUBJECT of another call rather than a complete
    /// assertion? A curried assertion applies its subject first.
    fn is_applied_again(&self, call: Node) -> bool {
        outer_node(call)
            .and_then(|p| self.pack.call_target(p))
            .is_some_and(|t| t.id() == call.id())
    }

    /// Does the assertion's own namespace hold the subject, or does its
    /// RECEIVER? The qualifier of the callee decides, and a bare callee
    /// has none to doubt.
    fn assertion_owns_its_argument(&self, call: Node) -> bool {
        self.pack
            .call_target(call)
            .and_then(|t| t.utf8_text(self.src).ok())
            .is_none_or(crate::lang::assertion_names_its_own_subject)
    }

    fn record_imports(&mut self, node: Node) {
        for edge in self.pack.imports(node, self.src) {
            self.import_roots.extend(edge.names.iter().cloned());
            self.facts.imports.push(super::ImportFact {
                target: edge.target,
                names: edge.names,
            });
        }
    }

    /// Declared method bundles under a TypeDef — the pack answers for
    /// the languages whose interfaces are declarations at all.
    fn record_interfaces(&mut self, node: Node) {
        let found = self.pack.interfaces(node, self.src);
        self.facts.interfaces.extend(found);
    }

    fn record_type_export(&mut self, node: Node) {
        if self.pack.is_public(node, self.src)
            && let Some(name) = self.scope_name(node).map(Box::<str>::from)
        {
            self.facts.exports.push(name);
        }
    }

    /// Member names included, unlike `record_use`: `pkg.Symbol` is how Go,
    /// C and qualified Rust/Python reach an export, and the dead-export
    /// join needs those. Allocates only on first sight of a name.
    fn record_ident(&mut self, node: Node, unit_idx: usize) {
        if let Ok(name) = node.utf8_text(self.src)
            && !self.mentions.contains(name)
        {
            self.mentions.insert(name.into());
        }
        self.record_use(node, unit_idx);
    }

    /// One bottom-up combine serves three metrics: normalized Merkle hashing
    /// (clones), named-node mass, and per-line expression height.
    fn scan(&mut self, node: Node, ctx: Ctx, sem: Sem) -> Sub {
        let ctx = Ctx {
            depth: ctx.depth + 1,
            ..ctx
        };
        let key = sem.clone_bucket().unwrap_or(node.kind_id() as u64 + 16);
        self.tokens[ctx.unit].push(key);
        let mut hash = mix(self.seed, key);
        let mut mass = 1u32;
        let mut logic = (sem.is_ctrl() || matches!(sem, Sem::Call | Sem::FnDef)) as u32;
        let mut height = 0u16;
        let multiline = node.start_position().row != node.end_position().row;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                // Anonymous tokens (operators, keywords) shape identity too.
                hash = mix(hash, child.kind_id() as u64 + 16);
                continue;
            }
            let Some(sub) = self.walk(child, ctx) else {
                continue;
            };
            hash = mix(hash, sub.hash);
            mass += sub.mass;
            logic += sub.logic;
            height = height.max(sub.height);
            let single_line = child.start_position().row == child.end_position().row;
            if multiline && single_line && self.pack.table_sem(child) != Sem::FnDef {
                let unit = &mut self.facts.units[ctx.unit];
                unit.max_expr_depth = unit.max_expr_depth.max(sub.height);
            }
        }
        let (line, end_line) = (
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
        );
        if mass >= MIN_CLONE_MASS
            && logic >= MIN_CLONE_LOGIC
            && end_line - line + 1 >= MIN_CLONE_LINES
        {
            self.facts.clone_sites.push(CloneSite {
                hash,
                mass,
                line,
                end_line,
            });
        }
        Sub {
            hash,
            mass,
            logic,
            height: height + 1,
        }
    }

    /// The nearest enclosing scope-former decides method-ness: a def whose
    /// closest TypeDef/FnDef ancestor is a class is a method, even when
    /// decorated or defined conditionally inside the class body.
    /// The declaration that scopes this one WITHOUT enclosing it.
    ///
    /// Only Perl needs this today: `package Foo;` is a statement and the
    /// subs it governs follow it as siblings. Climb to the file-level
    /// statement holding this unit, then walk back for the nearest
    /// TypeDef.
    fn preceding_scope<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        if !self.pack.file_level_scope {
            return None;
        }
        let mut top = node;
        while let Some(p) = top.parent() {
            if p.parent().is_none() {
                break;
            }
            top = p;
        }
        let mut prev = top.prev_named_sibling();
        while let Some(p) = prev {
            if self.sem_of(p) == Sem::TypeDef {
                return Some(p);
            }
            prev = p.prev_named_sibling();
        }
        None
    }

    fn enclosing_scope_is_class(&self, node: Node) -> bool {
        // Ancestors first. A file-level `package Foo;` governs
        // everything after it, but a SUB in between still ends the
        // class scope: a local inside one is a local, not an attribute
        // of a type. Asking the preceding scope first made every
        // rewrite in every Perl sub read as a class attribute, which
        // is the exemption, so `repurposed` was dead for the language.
        let mut anc = node.parent();
        while let Some(a) = anc {
            match self.sem_of(a) {
                Sem::TypeDef => return true,
                Sem::FnDef | Sem::Lambda => return false,
                _ => anc = a.parent(),
            }
        }
        self.preceding_scope(node).is_some()
    }

    /// Base name of a scope-forming node: the pack's name_node hook wins,
    /// then the `name` field, then the parent's binding site (promoted
    /// lambdas), then a `type` field (Rust impl blocks, generics stripped).
    fn scope_name(&self, node: Node) -> Option<&str> {
        let named = self
            .pack
            .name_node(node)
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| {
                let p = node.parent()?;
                p.child_by_field_name("name")
                    .or_else(|| p.child_by_field_name("left"))
                    .or_else(|| p.child_by_field_name("key"))
            });
        let n = named.or_else(|| node.child_by_field_name("type"))?;
        let text = n.utf8_text(self.src).ok()?;
        // A quoted name is PROSE — Zig's `test "..."`, a JS framework's
        // test label — in whichever quote the ecosystem prefers. It has
        // no generics to strip, and a `<` inside it (`renders <Button>`)
        // is part of the sentence.
        const QUOTES: [char; 3] = ['"', '\'', '`'];
        Some(match text.starts_with(QUOTES) {
            true => text.trim_matches(QUOTES),
            false => text.split('<').next().unwrap_or(text),
        })
    }

    fn note_skipped_test(&mut self, node: Node) {
        self.facts
            .skipped_tests
            .push(node.start_position().row as u32 + 1);
    }

    /// The bytes of this definition's contract documentation, wherever
    /// the pack found them — a `///` run above it, a JSDoc block, a
    /// docstring inside the body, an Elixir `@doc` attribute.
    fn doc_text(&self, node: Node) -> Option<&[u8]> {
        let (start, end) = self.pack.doc_span(node, self.src)?;
        self.src.get(start as usize..end as usize)
    }

    fn open_unit(&mut self, node: Node) -> usize {
        if self.pack.skips_test(node, self.src) {
            self.note_skipped_test(node);
        }
        let recv = self.receiver(node);
        let (name, qualname) = self.unit_names(node, recv.as_ref());
        let mut unit = UnitFacts {
            name,
            qualname,
            line: node.start_position().row as u32 + 1,
            lines: line_span(node),
            is_method: self.pack.is_type_member(node)
                || self.enclosing_scope_is_class(node)
                || recv.is_some(),
            ..UnitFacts::blank()
        };
        // What this unit calls its own object: a receiver consumed from the
        // parameter list, else one the grammar declares separately. Empty
        // for free functions.
        let self_name = self
            .take_params(node, &mut unit)
            .or_else(|| recv.map(|r| r.name))
            .unwrap_or_else(|| "".into());
        unit.is_public = self.pack.is_public(node, self.src);
        let docs = self.doc_text(node);
        unit.doc_lines = docs.map_or(0, line_count);
        // What the documentation CLAIMS the parameters are called. Read
        // here rather than beside the comment runs because the claim is
        // only meaningful against the signature it sits above, and this
        // is where the two meet.
        unit.documented_params = docs
            .and_then(|d| str::from_utf8(d).ok())
            .map(|d| crate::docparam::documented(d, self.pack.doc_markers).into())
            .unwrap_or_default();
        unit.body = self.body_shape(node);
        // Only `ceremony` asks this, and only about literal-bodied
        // declarations, so the ancestor walk and the `interfaces`
        // allocation are skipped for every ordinary function.
        unit.is_override = unit.body != BodyShape::Real && self.is_override_point(node);
        unit.is_passthrough = self.is_passthrough(node, &unit.params);
        unit.is_async = self.pack.is_async(node, self.src);
        unit.named_test = self.pack.declares_test(node, self.src)
            || (self.facts.is_test_file && self.pack.names_test(node, self.src));
        unit.is_test =
            self.facts.is_test_file || unit.named_test || self.pack.is_test_code(node, self.src);
        // A trailing return type outranks the declaration's type field:
        // where one is written the field holds `auto`, which names no
        // type at all.
        unit.returns = self
            .pack
            .trailing_return(node)
            .or_else(|| node.child_by_field_name(self.pack.return_type_field))
            .and_then(|r| r.utf8_text(self.src).ok())
            .map(|t| t.trim_start_matches(':').trim())
            .unwrap_or("")
            .into();
        unit.return_arity = self.pack.return_arity(node, self.src);
        self.facts.units.push(unit);
        self.live.push(LiveMap::new());
        self.declared.push(std::collections::HashSet::new());
        self.envy.push(std::collections::HashMap::new());
        self.callees.push(Vec::new());
        self.self_names.push(self_name);
        self.tokens.push(Vec::new());
        self.facts.units.len() - 1
    }

    /// A unit's own name and its scope-qualified form. A receiver-declared
    /// method has no enclosing type node to inherit a scope from, so
    /// `Billing.Total` and `Invoice.Total` in one file would otherwise
    /// share a name — and with it a baseline identity.
    fn unit_names(&self, node: Node, recv: Option<&Receiver>) -> (Box<str>, Box<str>) {
        // A composed name outranks every node-based route: it exists
        // precisely because no single node spells the whole name.
        let name: Box<str> = match self.pack.composed_name(node, self.src) {
            Some(composed) => composed.into(),
            None => self.scope_name(node).unwrap_or("?").into(),
        };
        let mut scopes: Vec<&str> = Vec::new();
        let mut anc = node.parent();
        while let Some(a) = anc {
            if matches!(self.sem_of(a), Sem::TypeDef | Sem::FnDef)
                && let Some(s) = self.scope_name(a)
            {
                scopes.push(s);
            }
            anc = a.parent();
        }
        scopes.reverse(); // ancestors arrive innermost-first
        if let Some(s) = self.preceding_scope(node).and_then(|p| self.scope_name(p)) {
            scopes.insert(0, s);
        }
        if let Some(r) = recv {
            scopes.insert(0, &r.type_name);
        }
        if scopes.is_empty() {
            return (name.clone(), name);
        }
        let mut q = scopes.join(self.pack.scope_sep);
        q.push_str(self.pack.scope_sep);
        q.push_str(&name);
        (name, q.into())
    }

    /// Fill the unit's parameters, returning the receiver's name when the
    /// first parameter IS the receiver (Python `self`, Rust `&self`).
    fn take_params(&mut self, node: Node, unit: &mut UnitFacts) -> Option<Box<str>> {
        let params = param_list(node)?;
        let list = if params.named_child_count() == 0 {
            vec![params] // single bare parameter (`x => x`)
        } else {
            // A CURRIED definition writes its parameters in several
            // LISTS, one after the other: `def fetch[A](req: Request)(f:
            // Response => A)` declares two, and reading only the first
            // made every later one read as documented-but-absent.
            //
            // Only a genuine list may be continued this way. Where the
            // grammar fields a single `parameter` instead — Swift and
            // Solidity — its siblings are not another list but the next
            // declaration's, and following them read openzeppelin's
            // two-argument `pack_1_1` as taking 502.
            let mut lists = vec![params];
            let mut next = params.next_named_sibling();
            while let Some(more) =
                next.filter(|n| params.kind() == "parameters" && n.kind() == params.kind())
            {
                lists.push(more);
                next = more.next_named_sibling();
            }
            lists
                .iter()
                .flat_map(|l| {
                    let mut cursor = l.walk();
                    l.named_children(&mut cursor).collect::<Vec<_>>()
                })
                .collect()
        };
        let mut receiver = None;
        for (i, p) in list.into_iter().enumerate() {
            let Some(info) = self.pack.param_info(p, self.src) else {
                continue;
            };
            if i == 0 && unit.is_method && info.selfish {
                unit.mut_receiver = info.mut_receiver;
                receiver = Some(info.name);
                continue;
            }
            if info.mutable_default {
                self.facts
                    .spooky_lines
                    .push(node.start_position().row as u32 + 1);
            }
            unit.params.push(super::ParamFact {
                name: info.name,
                boolish: info.boolish,
                kw_splat: info.kw_splat,
                optional: info.optional,
                typed: info.typed,
                loose: info.loose,
                type_name: info.type_name,
                destructured: info.destructured,
                splat: info.splat,
            });
        }
        receiver
    }

    /// A method's own object when the grammar declares it in a `receiver`
    /// field instead of as the first parameter (Go). Without this the
    /// receiver reads as a foreign object, so Feature Envy either never
    /// fires — the method is not even recognized as a method — or fires
    /// on every method once it is.
    fn receiver(&self, node: Node) -> Option<Receiver> {
        let decl = node
            .child_by_field_name("receiver")?
            .named_child(0)
            .filter(|d| self.pack.table_sem(*d) != Sem::Ident)?;
        let text = |n: Node| n.utf8_text(self.src).unwrap_or("");
        let bare = |t: &str| {
            let t = t.trim_start_matches(['*', '&']);
            t.split(['[', '<']).next().unwrap_or(t).to_string()
        };
        Some(Receiver {
            name: decl
                .child_by_field_name("name")
                .map(text)
                .unwrap_or("")
                .into(),
            type_name: bare(decl.child_by_field_name("type").map(text).unwrap_or("")),
        })
    }

    /// A unit whose whole body forwards its own parameters, in order, to
    /// one callee adds an interface without adding an abstraction
    /// (Ousterhout's shallow wrapper; Fowler's Middle Man). An adapter
    /// that reorders, transforms, or supplements arguments is not one.
    fn is_passthrough(&self, node: Node, params: &[super::ParamFact]) -> bool {
        // A DECLARED OVERRIDE IS NOT A MIDDLE MAN. Fowler's smell is a
        // class that could be inlined; a method implementing someone
        // else's signature cannot be, and the Decorator pattern
        // forwards on purpose — netty's
        // `Http2FrameListenerDecorator.onRstStreamRead` exists to.
        //
        // Read from the declaration's own marker, not from the pack's
        // `is_override`: that hook answers "could this be an override
        // point" for `ceremony` and says yes to every method of every
        // subclass, which would silence the metric wholesale. Where a
        // language does not MARK an override — Rust's trait impls, Go's
        // interfaces — nothing is claimed and the unit stays judged.
        //
        // Known cost, and it is the honest one: a Decorator whose every
        // method forwards and none adds behaviour IS a Middle Man, and
        // this silences it.
        //
        // A LAMBDA HAS NO NAME to be shallow about. `decode: (str) =>
        // BigInt(str)` is a forwarder by definition — that is what a
        // lambda IS — and it exists to adapt a callback's shape.
        if crate::lang::declares_an_override(node, self.src)
            || crate::lang::is_a_lambda(node.kind())
        {
            return false;
        }
        let Some(call) = self.sole_forwarded_call(node) else {
            return false;
        };
        self.forwards_its_own_params(call, params)
    }

    /// The ONE call a body consists of, if it consists of one.
    ///
    /// An expression-bodied lambda forwards directly; a block must hold
    /// exactly one statement, where a docstring is not substance.
    fn sole_forwarded_call<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        let body = node.child_by_field_name("body")?;
        let mut stmt = body;
        if self.pack.table_sem(body) != Sem::Call {
            let mut cursor = body.walk();
            let stmts: Vec<Node> = body
                .named_children(&mut cursor)
                .filter(|n| !self.pack.is_doc(*n) && self.sem_of(*n) != Sem::Comment)
                .collect();
            let [only] = stmts[..] else { return None };
            stmt = only;
        }
        // Unwrap `return expr` / expression statements to the call
        // itself. `expression_list` is Go's: it fields a return VALUE
        // inside a list even when there is one of them.
        while matches!(
            stmt.kind(),
            "return_statement" | "expression_statement" | "expression_list"
        ) {
            stmt = stmt.named_child(0)?;
        }
        if self.pack.table_sem(stmt) != Sem::Call {
            return None;
        }
        // FORWARDING TO A SUPERCLASS is the language asking, not the
        // author choosing: a subclass that wants an inherited
        // constructor must re-declare it, and `super(cause)` is the
        // whole of that re-declaration.
        let calls_super = self
            .callee_trailing_name(stmt)
            .into_iter()
            .chain(
                stmt.child_by_field_name("function")
                    .and_then(|f| f.utf8_text(self.src).ok()),
            )
            .any(|name| matches!(name, "super" | "base" | "parent" | "super()" | "parent::"));
        (!calls_super).then_some(stmt)
    }

    /// Are the call's arguments this unit's OWN parameters, as a prefix
    /// in declaration order? Defaults may be dropped.
    ///
    /// A zero-argument callee matches vacuously, which made every
    /// one-line accessor (`self.bits.len()`) a Middle Man — half of all
    /// Rust firings. Known cost: a genuine zero-argument forwarder now
    /// goes unseen.
    ///
    /// `call_arguments`, not the `arguments` field: half the packs wrap
    /// each argument in a node of their own (PHP's `argument`, Swift's
    /// `value_argument`) or keep the list somewhere else entirely (Zig
    /// nests them under the call, OCaml repeats a field), and reading
    /// the field directly saw none of them — C#, OCaml and Zig read
    /// zero forwarders in the whole gold corpus.
    fn forwards_its_own_params(&self, call: Node, params: &[super::ParamFact]) -> bool {
        let args = call_arguments(self.pack, call);
        let Some(names): Option<Vec<&str>> = args
            .iter()
            .map(|a| {
                (self.pack.table_sem(*a) == Sem::Ident)
                    .then(|| a.utf8_text(self.src).ok())
                    .flatten()
            })
            .collect()
        else {
            return false;
        };
        !names.is_empty()
            && names.len() <= params.len()
            && names.iter().zip(params).all(|(a, p)| *a == &*p.name)
    }

    /// Does this call park the thread rather than yield? Node's `*Sync`
    /// family is named after the problem — but only the family, read
    /// off [`BLOCKING_SYNC`], because the SUFFIX is not the family. `sleep` needs its QUALIFIER
    /// read: `time.sleep` and `std::thread::sleep` park the thread,
    /// while `asyncio.sleep`, `tokio::time::sleep` and the promise
    /// `sleep` helper every JS codebase carries are the CORRECT pattern
    /// — the fix this metric exists to demand. Judging the trailing
    /// name alone condemned exactly the code that did it right. A bare
    /// unqualified `sleep` is never flagged for the same reason: in JS
    /// it cannot block, and elsewhere it is undecidable without the
    /// import — precision first.
    fn parks_the_thread(&self, call: Node) -> bool {
        let Some(path) = self.callee_text(call) else {
            return false;
        };
        // Case-folded: the same standard call is `time.sleep` in one
        // language and `Thread.Sleep` in the next, and a table that
        // spells only the lowercase one reads zero for the other.
        let lowered: Vec<String> = path
            .split(['.', ':'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        let segs: Vec<&str> = lowered.iter().map(String::as_str).collect();
        let Some(last) = path.rsplit(['.', ':']).find(|s| !s.is_empty()) else {
            return false;
        };
        if BLOCKING_SYNC.contains(&last) {
            return true;
        }
        // tokio's own naming: blocking_recv/blocking_send/blocking_lock
        // exist FOR synchronous contexts and panic inside a runtime.
        if last.len() > 9 && last.starts_with("blocking_") {
            return true;
        }
        match segs.as_slice() {
            // The standard thread module, however deeply qualified.
            [.., "thread", "sleep"] => true,
            // Python's `time.sleep` exactly: `tokio::time::sleep` is
            // three segments and stays exempt.
            ["time", "sleep"] => true,
            // Perl's standard sub-second sleep. `Time::HiRes` parks the
            // interpreter exactly as `time.sleep` parks Python's, and
            // the ecosystem has no async-aware sleep to confuse it with.
            ["time", "hires", "sleep" | "usleep" | "nanosleep"] => true,
            // The synchronous filesystem, spelled by its qualifier:
            // `tokio::fs` names its runtime and stays exempt — the
            // asyncio.sleep lesson, applied to files. (Bare `fs::read`
            // after `use std::fs` is undecidable against `use
            // tokio::fs` without import resolution, and a miss is
            // cheaper than a guess.)
            ["std", "fs", ..] => true,
            _ => self.pack.lang == crate::lang::Lang::Python && python_parks(&segs),
        }
    }

    /// Whole text of a call's target: `time.sleep`, `std::thread::sleep`.
    fn callee_text(&self, call: Node) -> Option<&str> {
        self.pack.call_target(call)?.utf8_text(self.src).ok()
    }

    /// Last segment of a call's target: `sleep` for `time.sleep`,
    /// `thread::sleep` and a bare `sleep` alike.
    fn callee_trailing_name(&self, call: Node) -> Option<&str> {
        let text = self.callee_text(call)?;
        text.rsplit(['.', ':']).next().filter(|s| !s.is_empty())
    }

    /// A literal that carries a real key's entropy under a name that
    /// promises a credential.
    fn record_secret(&mut self, node: Node, unit_idx: usize) {
        // A fixture credential in a test is not a leak: it authenticates
        // nothing, and every JWT test in existence carries one. Test
        // UNITS count too — a #[test] living in a production file is
        // still a test.
        if self.facts.is_test_file || self.facts.units[unit_idx].is_test {
            return;
        }
        if self.is_hardcoded_secret(node) {
            let line = node.start_position().row as u32 + 1;
            self.facts.secrets.push(line);
        }
    }

    /// A string literal worth naming if it turns up again. Interpolated
    /// literals are excluded — an f-string or a template is a
    /// computation, and two of them sharing text are not one constant.
    /// So are literals in constant position, which is the remedy: a
    /// `const KIND = "user"` is the name, not the smell.
    fn record_repeatable_string(&mut self, node: Node, unit_idx: usize) {
        if self.facts.is_test_file || self.facts.units[unit_idx].is_test {
            return;
        }
        // Interpolation is named by the grammar, not implied by having
        // children: a plain Python string is already three nodes
        // (start, content, end), so "has children" excluded every
        // literal in the language.
        let mut cursor = node.walk();
        let interpolated = node.named_children(&mut cursor).any(|c| {
            matches!(
                c.kind(),
                "interpolation" | "template_substitution" | "string_interpolation"
            )
        });
        if interpolated {
            return;
        }
        let Ok(raw) = node.utf8_text(self.src) else {
            return;
        };
        // Strip a prefix sigil (b, r, f, rb) BEFORE the quotes — a
        // blanket trim of those letters ate its way into the content
        // and turned "buffer" into uffe.
        let text = raw
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .trim_matches(['"', '\'', '`']);
        if text.len() < MIN_MAGIC_STRING || !self.is_unnamed_literal(node) {
            return;
        }
        let row = node.start_position().row as u32 + 1;
        self.string_rows
            .entry(text.into())
            .or_default()
            .push((row, unit_idx));
    }

    /// A command handed to a SHELL with a value spliced into it. The
    /// remedy is an argument list, which needs no shell at all and
    /// carries no interpolation — so, like the built-query check, the
    /// fix makes this finding disappear rather than suppressing it.
    ///
    /// `shell=True` with a LITERAL command is a style choice and stays
    /// silent: nothing untrusted reaches the parser.
    fn check_shelled_command(&mut self, call: Node, unit_idx: usize) {
        // A shell script IS the shell. The remedy this metric names —
        // an argument list, which needs no shell at all — does not
        // exist in the language: `bash -c "$a && $b"` has no argv form,
        // and `exec cmd args...` is the POSIX builtin that REPLACES the
        // process with an argv vector, sharing only a spelling with
        // Node's child_process.exec. The matrix has said so since the
        // row was written; nothing enforced it, and gold shell read 36
        // findings at precision zero — seven times the ceiling a rung-2
        // gate may cost.
        if self.pack.lang == crate::lang::Lang::Shell {
            return;
        }
        if self.facts.is_test_file || self.facts.units[unit_idx].is_test {
            return;
        }
        let Some(route) = self.reaches_a_shell(call) else {
            return;
        };
        // The assembled string may be the argument itself (a template),
        // or one level down inside a formatting call — Go writes
        // `exec.Command("sh", "-c", fmt.Sprintf(...))`, where the
        // interpolation lives in Sprintf's argument, not Command's.
        // Descend into a CALL only. Go writes `exec.Command("sh", "-c",
        // fmt.Sprintf(...))`, where the interpolation lives one level
        // down. Descending into an ARRAY instead would flag the
        // remedy: vscode's `exec(['stash', 'list', `--format=${F}`])`
        // passes a list, which reaches no shell at all.
        let args = call_arguments(self.pack, call);
        // What may be read ONE LEVEL DOWN depends on which route got
        // here, and the difference is the whole precision of this
        // check. Under an always-a-shell callee only a nested CALL is
        // read — Go writes `exec.Command("sh", "-c", fmt.Sprintf(..))`
        // — because descending into a LIST there flags the remedy:
        // vscode's `exec(['stash', 'list', `--format=${F}`])` passes an
        // argv vector and reaches no shell at all. Under the `-c` route
        // a list is exactly where the command rides, since the flag
        // that proved a shell is in it.
        fn inside<'t>(me: &Extractor, route: &ShellRoute, arg: Node<'t>) -> Vec<Node<'t>> {
            match route {
                ShellRoute::Interprets { .. } => me.one_level_down(arg),
                ShellRoute::Always if me.pack.table_sem(arg) == Sem::Call => {
                    call_arguments(me.pack, arg)
                }
                ShellRoute::Always => Vec::new(),
            }
        }
        let assembled = args.iter().any(|arg| {
            self.is_assembled_string(*arg)
                || inside(self, &route, *arg)
                    .iter()
                    .any(|el| self.is_assembled_string(*el))
        });
        // A `-c` next to a VARIABLE is the strongest form of this
        // finding and was the one it declined: `exec.Command("bash",
        // "-c", script)` hands the parser a string the caller chose
        // entirely, where an interpolation at least shows what the
        // author wrote around it. Only under the `-c` route — the
        // always-a-shell callees take ordinary arguments, and demanding
        // a literal there is what keeps `exec(cmd)` from firing on
        // every call in the language.
        // A `-c` next to a VARIABLE is the strongest form of this
        // finding and was the one it declined: `exec.Command("bash",
        // "-c", script)` hands the parser a string the caller chose
        // entirely, where an interpolation at least shows what the
        // author wrote around it. The command is what FOLLOWS the flag
        // and nothing else — musl's `execl("/bin/sh", "sh", "-c",
        // <literal script>, "sh", s, redir)` passes its variables as
        // positional parameters, which is the parameterised remedy.
        let handed_over = matches!(
            route,
            ShellRoute::Interprets {
                command: Some(cmd)
            } if self.pack.table_sem(cmd) == Sem::Ident
        );
        let assembled = assembled || handed_over;
        if assembled && !self.hands_over_a_query(call) {
            self.facts
                .shelled_out
                .push(call.start_position().row as u32 + 1);
        }
    }

    /// Is the thing being handed over SQL rather than a command? `exec`
    /// earns its place in the shell list through Node's `child_process`,
    /// and pays for it everywhere else: `db.exec(...)`, `conn.exec(...)`
    /// and `cursor.exec(...)` are how half the world runs a query, so
    /// `db.exec(f"SELECT * FROM {t}")` was reported BOTH as a built
    /// query, correctly, and as a shelled-out command, which is a
    /// different accusation about a different attack surface.
    ///
    /// A shell command does not begin with a SQL verb. That is the whole
    /// rule, and it costs nothing real: `sh -c "SELECT ..."` is not a
    /// thing. The built-query detector still reports the line, so the
    /// finding is not lost — only the wrong name for it is.
    fn hands_over_a_query(&self, call: Node) -> bool {
        call_arguments(self.pack, call).iter().any(|arg| {
            arg.utf8_text(self.src)
                .is_ok_and(|text| starts_a_statement(text.trim_start_matches(['f', 'r', 'b'])))
        })
    }

    /// Is this node a string built from values rather than written?
    fn is_assembled_string(&self, node: Node) -> bool {
        if self.pack.table_sem(node) == Sem::StrLit {
            return interpolates(node) || self.inside_a_format_call(node);
        }
        self.joins_a_literal(node)
    }

    /// `"tar czf " + name + ".tgz"` — assembly spelled as an operator
    /// rather than as a hole. Java and Go have no interpolation at all,
    /// so this is the ONLY way either language builds a command, and
    /// judging interpolation alone read Java's whole contribution to
    /// this metric as nothing.
    ///
    /// One side a string literal, the other not: `a + b` over two
    /// numbers is arithmetic, and `"a" + "b"` is a literal written in
    /// two pieces. Nesting needs no special case — `"a" + x + "b"`
    /// groups as `("a" + x) + "b"`, whose left operand is not a
    /// literal.
    fn joins_a_literal(&self, node: Node) -> bool {
        let Some(op) = crate::lang::field_text_is(node, "operator", self.src) else {
            return false;
        };
        if op != "+" && op != "." {
            return false;
        }
        let side = |field: &str| node.child_by_field_name(field);
        let (Some(left), Some(right)) = (side("left"), side("right")) else {
            return false;
        };
        let literal = |n: Node| self.pack.table_sem(n) == Sem::StrLit;
        (literal(left) && !literal(right)) || (literal(right) && !literal(left))
    }

    /// Does this call hand its argument to a shell parser? Either the
    /// callee is one that always does, or an argument says so —
    /// `shell=True`, or the `-c` that turns any shell into an
    /// interpreter of whatever follows.
    ///
    /// A `-c` only counts when A SHELL INTRODUCED IT. Read as a bare
    /// flag it belongs to half the tools anyone runs: `git -c`,
    /// `git switch -c`, `clang -c`, `tar -c`, `jq -c`, `stat -c`,
    /// `shasum -c`, `pytest -c`, `perl -c`, `swift run -c` were 17 of
    /// the metric's 38 gold false positives, against zero true ones —
    /// every real `sh -c` on gold names the shell right beside the
    /// flag, which is also the only form the rule was added for
    /// (`exec.Command("sh", "-c", ..)` in Java, Go and C#).
    fn reaches_a_shell<'t>(&self, call: Node<'t>) -> Option<ShellRoute<'t>> {
        let callee = self.callee_trailing_name(call);
        if matches!(
            callee,
            Some("system" | "popen" | "exec" | "execSync" | "spawnSync" | "shell")
        ) {
            return Some(ShellRoute::Always);
        }
        let text = |n: Node| n.utf8_text(self.src).unwrap_or("");
        let args = call_arguments(self.pack, call);
        if args.iter().any(|a| text(*a).contains("shell=True")) {
            return Some(ShellRoute::Always);
        }
        // ARGV IS A LIST almost everywhere it is spelled at all:
        // node's `spawn('sh', ['-c', cmd])`, Elixir's `System.cmd("sh",
        // ["-c", cmd])`, Swift's `Process.launchedProcess(launchPath:
        // "/bin/sh", arguments: ["-c", cmd])`. Reading only DIRECT
        // arguments compared the whole `["-c", ...]` text against `-c`
        // and missed every one — the default spelling in four
        // languages. One level down, and no further: the shell has to
        // be named beside the flag either way.
        for (i, arg) in args.iter().enumerate() {
            let before = i.checked_sub(1).map(|p| args[p]);
            if let Some(route) = self.interpreted_here(callee, before, &args[i..]) {
                return Some(route);
            }
            let nested = self.one_level_down(*arg);
            for k in 0..nested.len() {
                let before = k.checked_sub(1).map(|p| nested[p]).or(before);
                if let Some(route) = self.interpreted_here(callee, before, &nested[k..]) {
                    return Some(route);
                }
            }
        }
        None
    }

    /// Does `rest[0]` turn a shell into an interpreter of what follows?
    ///
    /// The shell must be NAMED, and named as a literal: `exec.Command(
    /// "sh", "-c", ..)`, `spawn('bash', ['-c', ..])`. A bare identifier
    /// that happens to spell one is not a shell — git's own argument
    /// parser writes `strcmp(cmd, "-c")`, where `cmd` is the variable
    /// holding git's subcommand and matching it would report the
    /// program that IMPLEMENTS `-c` as a program that passes it on.
    fn interpreted_here<'t>(
        &self,
        callee: Option<&str>,
        before: Option<Node<'t>>,
        rest: &[Node<'t>],
    ) -> Option<ShellRoute<'t>> {
        let text = |n: Node| n.utf8_text(self.src).unwrap_or("");
        if !self.interprets_what_follows(text(rest[0])) {
            return None;
        }
        let named = callee.is_some_and(names_a_shell)
            || before
                .is_some_and(|b| self.pack.table_sem(b) == Sem::StrLit && names_a_shell(text(b)));
        named.then(|| ShellRoute::Interprets {
            command: rest.get(1).copied(),
        })
    }

    /// `-c` alone, or `-c` with the command riding in the same string —
    /// Java and C# hand the shell one argument, and a rule that only
    /// knew the separated form saw neither.
    fn interprets_what_follows(&self, text: &str) -> bool {
        let bare = text.trim_start_matches('$').trim_matches(['"', '\'']);
        bare == "-c" || bare.starts_with("-c ")
    }

    /// One level inside an argument: a list's elements, or a nested
    /// call's own arguments. The second is how C# writes
    /// `Process.Start("sh", string.Format("-c {0}", dir))` and Go
    /// `exec.Command("sh", "-c", fmt.Sprintf(..))`.
    fn one_level_down<'t>(&self, arg: Node<'t>) -> Vec<Node<'t>> {
        if self.pack.table_sem(arg) == Sem::Call {
            return call_arguments(self.pack, arg);
        }
        let mut cursor = arg.walk();
        arg.named_children(&mut cursor).collect()
    }

    /// An SQL statement being ASSEMBLED rather than written. A literal
    /// query is safe whatever it says, and a parameter marker (`?`,
    /// `$1`, `:name`, psycopg's `%s`) is the remedy — so only
    /// interpolation counts, and it is judged at the START of the
    /// string, where a statement announces itself.
    fn check_built_query(&mut self, node: Node, unit_idx: usize) {
        // A test builds queries to exercise the builder; the risk is
        // in production. Same line the credential family draws.
        if self.facts.is_test_file || self.facts.units[unit_idx].is_test {
            return;
        }
        let Ok(raw) = node.utf8_text(self.src) else {
            return;
        };
        if !starts_a_statement(raw) {
            return;
        }
        let built =
            (interpolates(node) || self.inside_a_format_call(node)) && interpolates_a_value(raw);
        if built {
            self.facts
                .sql_built
                .push(node.start_position().row as u32 + 1);
        }
    }

    /// Is this literal an argument of a formatting call — `Sprintf`,
    /// `format!`, `"...".format(...)`? Those spell interpolation as a
    /// call, so the literal itself carries no interpolation node.
    ///
    /// Three ancestors, not one: a grammar may wrap the literal in an
    /// `argument` node AND that in an `arguments` list before reaching
    /// the call, which is how PHP and C# spell every call there is.
    fn inside_a_format_call(&self, node: Node) -> bool {
        let mut call = None;
        let mut anc = node.parent();
        for _ in 0..3 {
            let Some(a) = anc else { break };
            if self.pack.table_sem(a) == Sem::Call {
                call = Some(a);
                break;
            }
            anc = a.parent();
        }
        call.and_then(|c| self.callee_trailing_name(c))
            .is_some_and(|name| {
                matches!(
                    name,
                    // C's own family is the bulk of it: `sprintf` was
                    // here and `snprintf` was not, so the rule caught
                    // the spelling nobody should use and missed the one
                    // everybody does. `Format` is C#'s, and the
                    // lowercase-only list could never match it.
                    "Sprintf"
                        | "Sprint"
                        | "Sprintln"
                        | "format"
                        | "Format"
                        | "sprintf"
                        | "snprintf"
                        | "vsnprintf"
                        | "asprintf"
                        | "printf"
                )
            })
    }

    /// Names that promise a credential. A literal bound to one of these
    /// is in the artifact and in the history, so rotating it is a
    /// release rather than a config change.
    const SECRET_NAMES: &'static [&'static str] = &[
        "password",
        "passwd",
        "secret",
        // Not bare "token": a lexer token, a syntax-highlighting token
        // and a parser token all wear that name, and vscode's themes
        // alone put a thousand of them in the source. Only the
        // qualified forms promise a credential.
        // Both spellings, because camelCase drops the separator and
        // `apikey` was already here without one: `apiToken` is as much
        // a promise as `api_token`, and only the qualified form is.
        "api_token",
        "apitoken",
        "auth_token",
        "authtoken",
        "access_token",
        "accesstoken",
        "refresh_token",
        "bearer",
        "apikey",
        "api_key",
        "access_key",
        "private_key",
        "credential",
        "auth_key",
    ];

    /// Precision first, on purpose. A credential-shaped NAME is not
    /// enough — `PASSWORD_FIELD = "password"` and `TOKEN_HEADER =
    /// "authorization"` are ordinary constants — so the value must also
    /// carry the entropy of a real key: mixed letters and digits, or
    /// twenty-odd characters of one. Interpolations and placeholders are
    /// out, since those are the shapes of code that reads a secret from
    /// somewhere else. Vendor-prefixed values are the one exception:
    /// they identify THEMSELVES, and need no name at all.
    fn is_hardcoded_secret(&self, node: Node) -> bool {
        let Ok(raw) = node.utf8_text(self.src) else {
            return false;
        };
        let value = raw.trim_matches(['"', '\'', '`']);
        if is_placeholder(value) || self.in_client_config(node) {
            return false;
        }
        // `BUILD_ID = "AKIA..."` is still a leak: the prefix plus the
        // tail's shape is the vendor's own declaration of what this is.
        if vendor_key(value) {
            return true;
        }
        if !looks_like_a_key(value) {
            return false;
        }
        let mut anc = node.parent();
        for _ in 0..4 {
            let Some(a) = anc else { break };
            if self.pack.binds_value(a.kind())
                || a.kind().contains("parameter")
                || a.kind() == "pair"
            {
                // The literal must be the VALUE, not part of the name.
                // `headers["Access-Control-Allow-Credentials"] = "true"`
                // has a credential-shaped subscript, and the string that
                // matched it was the subscript itself.
                return assigns_to(a, node)
                    && self.bound_name(a).is_some_and(promises_a_credential);
            }
            anc = a.parent();
        }
        false
    }

    /// A client-SDK config block publishes its keys BY DESIGN: an
    /// `apiKey` beside `appId`/`authDomain`/`indexName` is the
    /// publishable-key shape (Algolia search, Firebase web), and
    /// flagging every docs site's search config would teach readers to
    /// ignore the metric. A real secret hidden beside an `appId` goes
    /// unseen; that is the price, and it is the right one.
    fn in_client_config(&self, literal: Node) -> bool {
        const CLIENT_SIBLINGS: &[&str] = &["appId", "authDomain", "projectId", "indexName"];
        let Some(pair) = literal.parent().filter(|p| p.kind() == "pair") else {
            return false;
        };
        let Some(object) = pair.parent() else {
            return false;
        };
        let mut cursor = object.walk();
        object.named_children(&mut cursor).any(|sib| {
            sib.kind() == "pair"
                && sib
                    .child_by_field_name("key")
                    .and_then(|k| k.utf8_text(self.src).ok())
                    .is_some_and(|k| CLIENT_SIBLINGS.contains(&k.trim_matches(['"', '\''])))
        })
    }

    /// The name an assignment-ish node binds. `declarator` is C's
    /// spelling; the identifier fallback serves grammars that field
    /// nothing (Zig's variable_declaration).
    fn bound_name(&self, node: Node) -> Option<&str> {
        node.child_by_field_name("left")
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.child_by_field_name("pattern"))
            .or_else(|| node.child_by_field_name("declarator"))
            .or_else(|| node.child_by_field_name("key"))
            .or_else(|| {
                let mut cursor = node.walk();
                node.named_children(&mut cursor)
                    .find(|c| self.pack.table_sem(*c) == Sem::Ident)
            })?
            .utf8_text(self.src)
            .ok()
    }

    /// A number is magic when it is non-trivial and unnamed: outside const
    /// definitions, parameter defaults, indexing, types, and patterns
    /// (Kernighan & Plauger; McConnell ch. 12).
    fn is_magic(&self, node: Node) -> bool {
        let trivial = matches!(
            node.utf8_text(self.src),
            Ok("0" | "1" | "2" | "0.0" | "1.0" | "0.5" | "10" | "100" | "100.0")
        );
        !trivial && self.is_unnamed_literal(node)
    }

    /// Is this literal unnamed — outside const definitions, parameter
    /// defaults, indexing, types and patterns (Kernighan & Plauger;
    /// McConnell ch. 12)? A SCREAMING binding IS the name, and is the
    /// remedy both literal metrics ask for.
    fn is_unnamed_literal(&self, node: Node) -> bool {
        let screaming = |text: &str| {
            text.chars().any(|c| c.is_ascii_alphabetic())
                && !text.chars().any(|c| c.is_ascii_lowercase())
        };
        let mut anc = node.parent();
        for _ in 0..8 {
            let Some(a) = anc else { break };
            let kind = a.kind();
            if self.pack.exempts_literal(kind) {
                return false;
            }
            if self.pack.binds_value(kind) && self.binds_a_screaming_name(a, screaming) {
                return false;
            }
            anc = a.parent();
        }
        true
    }

    /// Does this binding site name its value in SCREAMING_CASE? That
    /// name is the remedy both literal metrics ask for.
    fn binds_a_screaming_name(&self, site: Node, screaming: impl Fn(&str) -> bool) -> bool {
        site.child_by_field_name("left")
            .or_else(|| site.child_by_field_name("name"))
            .and_then(|n| n.utf8_text(self.src).ok())
            .is_some_and(screaming)
    }

    /// Law of Demeter (Lieberherr 1989): reaching through >=3 consecutive
    /// data links couples the reader to neighbors' internal structure.
    /// Fluent chains break naturally — a call ends the descent — and a
    /// self/this base forgives its first link.
    fn check_demeter(&mut self, node: Node, unit: usize) {
        // C HAS NO METHODS, so it has no Demeter. Lieberherr's rule
        // constrains which OBJECTS a method may send a message to, and
        // its remedy — Hide Delegate, ask the neighbour instead of
        // reaching through it — needs a neighbour with behaviour to
        // ask. `s->layout.sparse.offsets` is a path into a nested
        // RECORD, which the rule does not address at all: redis, curl
        // and git write it because a C struct is a namespace, and all
        // 219 gold findings were that shape. C++ and Zig keep the cell,
        // because both have methods and `ctx.shstrtab->shndx` really is
        // a reach through one object to another's field. That
        // distinction is the whole of this exemption, so it is drawn
        // where the language draws it and nowhere wider.
        if self.pack.lang == crate::lang::Lang::C {
            return;
        }
        let Some((attr_kind, _)) = self.pack.attr() else {
            return;
        };
        // Only chain roots: a parent of the same kind means we are one of
        // its links and will be counted from the top.
        if node.kind_id() != attr_kind || outer_node(node).is_some_and(|p| p.kind_id() == attr_kind)
        {
            return;
        }
        self.record_chain(node, unit);
    }

    /// What one chain root contributes: an access to its own object, a
    /// tally against a foreign one, and — when it reaches far enough
    /// through something that is neither — a Demeter finding.
    fn record_chain(&mut self, node: Node, unit: usize) {
        let Some((attr_kind, object_field)) = self.pack.attr() else {
            return;
        };
        let mut links = 0u16;
        let mut base = node;
        while base.kind_id() == attr_kind {
            links += 1;
            match base.child_by_field_name(object_field) {
                Some(inner) => base = bare_argument(inner),
                None => break,
            }
        }
        // The same chain root feeds Feature Envy: one chain = one access
        // to its base receiver (methods only).
        let base_text = base.utf8_text(self.src).ok();
        let selfish_base = self.is_own_object(base_text, unit);
        let base_name = (self.pack.table_sem(base) == Sem::Ident)
            .then_some(base_text)
            .flatten();
        if self.facts.units[unit].is_method {
            if selfish_base {
                self.facts.units[unit].self_accesses += 1;
                self.note_own_member(node, object_field, unit);
            } else if let Some(name) = base_name
                && !name.starts_with(|c: char| c.is_uppercase())
            {
                // Uppercase bases are types/modules, not envied objects.
                *self.envy[unit].entry(name.into()).or_insert(0) += 1;
            }
        }
        if selfish_base {
            links = links.saturating_sub(1);
        }
        if links < 3 {
            return;
        }
        // An uppercase base is a type or a module — `Console.Out.Write`,
        // `Ecto.Query.Builder.apply` — and reaching through a namespace
        // is not reaching through an object. The envy branch above has
        // always said so; the chain count now says the same.
        if base_name.is_some_and(|n| n.starts_with(|c: char| c.is_uppercase())) {
            return;
        }
        self.facts.units[unit].demeter += 1;
        if let Some(name) = base_name {
            self.chain_roots.push((unit, name.into()));
        }
    }

    /// Does this chain start at the unit's OWN object?
    ///
    /// Judged by TEXT, where envy is judged by identifier-ness. Rust
    /// spells `self` with its own node kind rather than an identifier,
    /// so requiring Sem::Ident made every `self.field` in the language
    /// invisible — no self access, no own member, and a cohesive impl
    /// block reading as scattered.
    ///
    /// The text must lose its SIGIL first. PHP spells the receiver
    /// `$this` and Perl `$self`, neither of which matched, so every
    /// method in both languages was read as envying a foreign object
    /// that happens to be itself: 993 of PHP's 1138 gold findings named
    /// `$this` and 195 of Perl's 382 named `$self`. The same branch is
    /// the only writer of `own_members`, so both languages also
    /// measured ZERO classes for cohesion while being made of almost
    /// nothing else.
    fn is_own_object(&self, base_text: Option<&str>, unit: usize) -> bool {
        base_text.is_some_and(|n| {
            let n = crate::lang::unsigiled(n);
            matches!(n, "self" | "cls" | "this")
                || n == crate::lang::unsigiled(&self.self_names[unit])
        })
    }

    /// WHICH member this chain reaches first off the receiver:
    /// `self.cache.get()` touches `cache`. The property field is named
    /// differently in every grammar, so it is found by POSITION — the
    /// first name the link spells after its object.
    ///
    /// Elimination by `Sem::Ident` alone is not enough, and Perl is why:
    /// it gives a method name its own `method` kind, so the first
    /// identifier left over in `$self->cache($k)` was the ARGUMENT, and
    /// the class would have gained a local called `$k` as a member.
    /// Searching forward from the object costs nothing everywhere else,
    /// because there the member is both the next child AND an identifier.
    fn note_own_member(&mut self, chain: Node, object_field: &str, unit: usize) {
        // Innermost link: descend while the object is ANOTHER link of
        // the same kind, so the one left is the access to the receiver.
        let mut link = chain;
        while let Some(inner) = link
            .child_by_field_name(object_field)
            .filter(|inner| inner.kind_id() == link.kind_id())
        {
            link = inner;
        }
        let object = link.child_by_field_name(object_field);
        let mut cursor = link.walk();
        let kids: Vec<Node> = link.named_children(&mut cursor).collect();
        let after = object
            .and_then(|o| kids.iter().position(|c| c.id() == o.id()))
            .map_or(0, |i| i + 1);
        let member = kids[after.min(kids.len())..]
            .iter()
            .find(|c| {
                self.pack.table_sem(**c) == Sem::Ident
                    || c.utf8_text(self.src).is_ok_and(is_a_plain_name)
            })
            .and_then(|c| c.utf8_text(self.src).ok());
        if let Some(name) = member {
            let members = &mut self.facts.units[unit].own_members;
            if !members.iter().any(|m| &**m == name) {
                members.push(name.into());
            }
        }
    }

    /// A match/switch is identified by its sorted arm-label set. A single
    /// switch is fine; the SAME set in many places is the finding.
    fn record_switch_sig(&mut self, node: Node, unit_idx: usize) {
        let mut labels: Vec<String> = Vec::new();
        // Arms are direct children or one wrapper (body/block) below.
        let mut stack = vec![(node, 0u8)];
        while let Some((n, depth)) = stack.pop() {
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                match self.pack.table_sem(child) {
                    Sem::CaseArm => labels.push(arm_label(child, self.src)),
                    // A match's `else` IS its catch-all arm. Ruby,
                    // Elixir and Lua spell it with the same node a
                    // branch uses, so the kind table cannot separate
                    // them and the POSITION has to: an else directly
                    // under a match answers every label nobody wrote.
                    Sem::Else if depth == 0 => labels.push("default".into()),
                    _ if depth < 1 && self.pack.table_sem(child) != Sem::Match => {
                        stack.push((child, depth + 1))
                    }
                    _ => {}
                }
            }
        }
        if labels.len() < 3 {
            return;
        }
        // A catch-all turns "the compiler will tell you" into silence:
        // add a variant and this match keeps compiling, handling the new
        // case as if it were every old case it never anticipated.
        if labels.iter().any(|l| CATCH_ALL.contains(&l.as_str())) {
            self.facts.units[unit_idx].wildcard_matches += 1;
        }
        labels.sort_unstable();
        self.facts.switch_sigs.push(super::LabelSet {
            key: labels.join("\u{1f}").into(),
            line: node.start_position().row as u32 + 1,
        });
    }

    /// K&P: "say what you mean" — flag only provable inversions:
    /// negation of a `!=` comparison, and a negated negative-polarity
    /// name.
    ///
    /// A De Morgan candidate — `!(a && b)`, `not (a or b)` — used to
    /// count and no longer does. It was 1,416 of this metric's 1,758
    /// gold findings, 80.5%, and not one of the fourteen read verbatim
    /// was a defect anyone would ask to change: distributing the
    /// negation is usually LONGER and worse, `!(0 < rate && rate <= 1)`
    /// says "not in range" in one breath where `rate <= 0 || rate > 1`
    /// does not, `!(a && b)` is the canonical spelling of a
    /// mutual-exclusion assertion, and a bitmask query
    /// `!(flags & A || flags & B)` reads to the tree as a boolean
    /// disjunction while the negation is its only correct spelling.
    /// This is a rung-1 GATE; a clause that admired code trips 1,416
    /// times is measuring a convention.
    fn check_negation(&mut self, node: Node, unit: usize) {
        let Some(mut operand) = self.pack.negation_operand(node, self.src) else {
            return;
        };
        // Parentheses, and the wrappers a grammar puts around every
        // operand: Ruby writes `parenthesized_statements`, Solidity
        // nests an `expression` node at each level. Only these — a
        // general "one named child" rule would unwrap the inner `!` of
        // `!!x` and defeat the coercion guard below.
        while matches!(
            operand.kind(),
            "parenthesized_expression" | "parenthesized_statements" | "expression"
        ) {
            match operand.named_child(0) {
                Some(inner) => operand = inner,
                None => break,
            }
        }
        // `!!x` / `not not x` is a coercion to bool — a cast idiom, not
        // inverted logic — and it was 78% of this metric's TypeScript
        // firings.
        if self.pack.negation_operand(operand, self.src).is_some() {
            return;
        }
        let text = operand.utf8_text(self.src).unwrap_or("");
        let mut end = text.len().min(80);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let head = &text[..end];
        let negative_name = ["not_", "no_", "disabled", "invalid", "missing", "unset"]
            .iter()
            .any(|w| head.starts_with(w));
        // Known approximation: a "!=" inside a string literal within the
        // operand head would false-positive; rare enough to accept.
        let neq = operand.end_position().row == operand.start_position().row && head.contains("!=");
        if negative_name || neq {
            self.facts.units[unit].negations += 1;
        }
    }

    /// Any mention keeps a local alive; last row wins. Identifiers on the
    /// member side of an access (`x` in `foo.x`) are field names, not
    /// locals — counting them would inflate spans of same-named locals.
    fn record_use(&mut self, node: Node, unit: usize) {
        if let Some((attr_kind, object_field)) = self.pack.attr()
            && node.parent().is_some_and(|p| {
                p.kind_id() == attr_kind
                    && p.child_by_field_name(object_field)
                        .is_none_or(|obj| obj.id() != node.id())
            })
        {
            // Member names still matter to the test join: `replica.open()`
            // in a test exercises `open`. Insert-if-absent with no def so
            // spans stay untouched but test_refs sees the name.
            let u = &self.facts.units[unit];
            if (u.is_test || (self.facts.is_test_file && u.is_module))
                && let Ok(name) = node.utf8_text(self.src)
                && !name.starts_with('_')
            {
                let row = node.start_position().row as u32 + 1;
                self.live[unit].entry(name.into()).or_insert((None, row));
            }
            return;
        }
        let Ok(name) = node.utf8_text(self.src) else {
            return;
        };
        if name.starts_with('_') || matches!(name, "self" | "cls" | "this") {
            return;
        }
        let row = node.start_position().row as u32 + 1;
        match self.live[unit].get_mut(name) {
            Some(entry) => entry.1 = entry.1.max(row),
            None => {
                self.live[unit].insert(name.into(), (None, row));
            }
        }
    }

    /// Binding sites: every identifier inside the bound pattern becomes a
    /// local of the current unit (first definition wins).
    ///
    /// A pattern that REACHES THROUGH a member access binds no name at
    /// all — `cfg.field = v` and `@config.cache[k] = 1` write state the
    /// unit already had a handle on. `single_reassign_target` has always
    /// said so on the write side; `declared` now says it on the bind
    /// side, and it is what lets Feature Envy tell an object the unit
    /// built from one whose member it merely set. The live map keeps its
    /// older, looser reading, so no span and no repurposing moves.
    fn record_defs(&mut self, node: Node, field: &str, unit: usize) {
        let Some(target) = bound_pattern(node, field) else {
            return;
        };
        let binds = !self.reaches_through_a_member(target);
        let row = node.start_position().row as u32 + 1;
        let mut stack = vec![target];
        while let Some(n) = stack.pop() {
            if self.pack.table_sem(n) == Sem::Ident {
                self.note_bound_name(n, row, unit, binds);
                continue;
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                stack.push(child);
            }
        }
    }

    /// One name out of a binding pattern: alive from `row`, and this
    /// unit's own if the pattern bound it rather than reached through it.
    fn note_bound_name(&mut self, n: Node, row: u32, unit: usize, binds: bool) {
        let Ok(name) = n.utf8_text(self.src) else {
            return;
        };
        if name.starts_with('_') {
            return;
        }
        let entry = self.live[unit].entry(name.into()).or_insert((None, row));
        entry.0.get_or_insert(row);
        entry.1 = entry.1.max(row);
        if binds {
            self.declared[unit].insert(name.into());
        }
    }

    /// Does this bound pattern touch a member access anywhere inside it?
    fn reaches_through_a_member(&self, target: Node) -> bool {
        let Some((attr_kind, _)) = self.pack.attr() else {
            return false;
        };
        let mut stack = vec![target];
        while let Some(n) = stack.pop() {
            if n.kind_id() == attr_kind {
                return true;
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        false
    }

    fn mark_commentary(&mut self, node: Node) {
        for row in node.start_position().row..=node.end_position().row {
            self.commentary.set(row);
        }
    }

    /// One comment RUN, classified by what it introduces and measured
    /// as prose.
    ///
    /// Gathered whole at its first node. A `///` doc parses one node
    /// per line, so a fenced example, a sentence and a paragraph all
    /// span several nodes — none of them can be recognized a node at a
    /// time. The rest of the run arrives already accounted for.
    fn record_comment_run(&mut self, node: Node, ctx: Ctx) {
        if node.start_position().row < self.comments_through {
            return;
        }
        // A comment that trails code labels THAT line; the comment on
        // the next line is a new thought, not a continuation.
        let trails = self.trails_code(node);
        let mut last = node;
        while !trails
            && let Some(next) = last.next_named_sibling().filter(|n| {
                self.sem_of(*n) == Sem::Comment
                    && n.start_position().row == crate::lang::last_row(last) + 1
            })
        {
            last = next;
        }
        self.comments_through = crate::lang::last_row(last) + 1;
        let follows = last
            .next_named_sibling()
            .filter(|n| n.start_position().row == crate::lang::last_row(last) + 1);
        let role = match trails {
            true => CommentRole::Trailing,
            false => self.introduced_role(node, follows, ctx),
        };
        let start = match trails {
            true => node.start_byte(),
            // The run's own indentation is part of its text: it is what
            // tells an indented example from a paragraph.
            false => self.line_start(node.start_byte()),
        };
        let attach = match role {
            CommentRole::Inline | CommentRole::Trailing => Attach::Unit(ctx.unit as u32),
            CommentRole::FnSummary => match follows {
                Some(n) => Attach::AtLine(n.start_position().row as u32 + 1),
                None => Attach::Nothing,
            },
            _ => Attach::Nothing,
        };
        self.push_comment(start..last.end_byte(), node, role, attach);
    }

    /// What a run that opens its own line introduces.
    fn introduced_role(&self, node: Node, follows: Option<Node>, ctx: Ctx) -> CommentRole {
        match follows.map(|n| self.declared_sem(n)) {
            Some(Sem::FnDef) => return CommentRole::FnSummary,
            Some(Sem::TypeDef) => return CommentRole::TypeDoc,
            // A member of a type that is neither a method nor a nested
            // type: a field, a property, a constant, an enum case. No
            // Sem names one — every grammar spells it as an ordinary
            // declaration — so it is recognized by where it sits.
            Some(_) if self.enclosing_scope_is_class(node) => return CommentRole::FieldDoc,
            _ => {}
        }
        if ctx.unit != 0 {
            return CommentRole::Inline;
        }
        // Opens the file: nothing declared precedes it, and it declares
        // nothing itself.
        let opens_file = node.prev_named_sibling().is_none()
            && node.parent().is_some_and(|p| p.parent().is_none());
        match opens_file {
            true => CommentRole::ModuleHeader,
            false => CommentRole::Inline,
        }
    }

    /// What this statement DECLARES, seen through whatever the grammar
    /// wraps the declaration in.
    ///
    /// A doc comment is written above the statement, and the statement
    /// is not always the declaration: TypeScript hangs `export` outside
    /// the function and binds an arrow through a declarator, OCaml
    /// wraps a binding in a `value_definition`. A wrapper starts on the
    /// same ROW as the thing it wraps — that is what makes it a wrapper
    /// rather than a neighbour — so the search stays on that row.
    ///
    /// Without this, every exported TypeScript function's JSDoc and
    /// every OCaml `(** *)` above a `let` read as loose commentary, and
    /// the summary distribution they belong to would be missing them.
    ///
    /// A decorated definition is the one wrapper that does NOT fit on
    /// one row — Python hands the decorators their own lines — and it
    /// is recognized instead by holding exactly one declaration.
    fn declared_sem(&self, node: Node) -> Sem {
        const MAX_WRAPPERS: u8 = 3;
        let row = node.start_position().row;
        let mut stack = vec![(node, 0u8)];
        while let Some((n, depth)) = stack.pop() {
            let sem = self.sem_of(n);
            if matches!(sem, Sem::FnDef | Sem::TypeDef) {
                return sem;
            }
            if depth >= MAX_WRAPPERS {
                continue;
            }
            let mut cursor = n.walk();
            let kids: Vec<Node> = n.named_children(&mut cursor).collect();
            let mut declares = kids
                .iter()
                .map(|k| self.sem_of(*k))
                .filter(|s| matches!(s, Sem::FnDef | Sem::TypeDef));
            if let (Some(only), None) = (declares.next(), declares.next()) {
                return only;
            }
            let same_row = kids.into_iter().filter(|k| k.start_position().row == row);
            stack.extend(same_row.map(|k| (k, depth + 1)));
        }
        Sem::None
    }

    /// A docstring is documentation the grammar spells as a VALUE —
    /// Python's and Ruby's first-statement string — so it carries no
    /// comment node and the run walk never reaches it.
    fn record_docstring(&mut self, node: Node, ctx: Ctx) {
        let mut role = CommentRole::ModuleHeader;
        let mut anc = node.parent();
        while let Some(a) = anc {
            match self.sem_of(a) {
                Sem::FnDef | Sem::Lambda => {
                    role = CommentRole::FnSummary;
                    break;
                }
                Sem::TypeDef => {
                    role = CommentRole::TypeDoc;
                    break;
                }
                _ => anc = a.parent(),
            }
        }
        let attach = match role {
            CommentRole::FnSummary => Attach::Unit(ctx.unit as u32),
            _ => Attach::Nothing,
        };
        let start = self.line_start(node.start_byte());
        self.push_comment(start..node.end_byte(), node, role, attach);
    }

    /// Measure one comment run and keep it. A run that cannot be read
    /// as words at all — a comment written in a script that does not
    /// space them — is skipped rather than mismeasured.
    fn push_comment(
        &mut self,
        span: std::ops::Range<usize>,
        first: Node,
        role: CommentRole,
        attach: Attach,
    ) {
        let Some(text) = self.src.get(span).and_then(|b| str::from_utf8(b).ok()) else {
            return;
        };
        let Some(prose) = crate::prose::measure(text, self.pack.doc_markers) else {
            return;
        };
        self.facts.comments.push(CommentFact {
            line: first.start_position().row as u32 + 1,
            role,
            unit: match attach {
                Attach::Unit(u) => Some(u),
                _ => None,
            },
            prose,
        });
        self.doc_targets.push(match attach {
            Attach::AtLine(line) => line,
            _ => 0,
        });
    }

    /// Does code sit on this node's line, before it?
    fn trails_code(&self, node: Node) -> bool {
        self.src[..node.start_byte()]
            .iter()
            .rev()
            .take_while(|b| **b != b'\n')
            .any(|b| !b.is_ascii_whitespace())
    }

    /// Byte offset of the start of the line this byte sits on.
    fn line_start(&self, byte: usize) -> usize {
        self.src[..byte]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |n| n + 1)
    }

    /// Keep every non-doc comment line for the block analysis. A doc
    /// comment is DOCUMENTATION whatever it contains — an example in
    /// a docstring is the point of the docstring, not abandoned code.
    fn keep_comment_text(&mut self, node: Node) {
        let Ok(text) = node.utf8_text(self.src) else {
            return;
        };
        let leading = text.trim_start();
        let documents = leading.starts_with("///")
            || leading.starts_with("//!")
            || leading.starts_with("/**")
            || leading.starts_with("(**")
            || self.pack.doc_markers.iter().any(|m| leading.starts_with(m));
        let first = node.start_position().row as u32 + 1;
        for (offset, line) in text.lines().enumerate() {
            let row = first + offset as u32;
            // A debt marker counts wherever it is written. `/// TODO:
            // handle the error case` is a promise in public, which is
            // if anything the more binding kind.
            if carries_debt(line) {
                self.facts.debt_markers.push(row);
            }
            // The commented-out-code analysis is another matter: a doc
            // comment is DOCUMENTATION whatever it contains, since an
            // example in a docstring is the point of the docstring.
            if !documents {
                self.comment_lines.push((row, strip_comment_marker(line)));
            }
        }
    }

    /// A comment that tells the type checker to stop looking. Whatever it
    /// would have said is now undocumented and unenforced — the one
    /// suppression that silences a whole class of error at once.
    fn check_suppression(&mut self, node: Node) {
        const MARKERS: &[&str] = &[
            "@ts-ignore",
            "@ts-expect-error",
            "@ts-nocheck",
            "type: ignore",
            "pyright: ignore",
            "mypy: disable",
            // PHP's checkers are separate programs, but these are the
            // same act: an inline comment telling a type checker to
            // stop looking. `nolint` and `#[allow]` are NOT here —
            // those configure a linter's style rules, not a type.
            "phpstan-ignore",
            "psalm-suppress",
        ];
        let Ok(text) = node.utf8_text(self.src) else {
            return;
        };
        if MARKERS.iter().any(|m| text.contains(m)) {
            self.facts
                .suppressions
                .push(node.start_position().row as u32 + 1);
        }
    }

    /// A comment that merely restates its adjacent code is noise with a
    /// maintenance cost (Kernighan & Plauger: "don't just echo the code").
    /// Lexical containment, precision-first: doc comments, markers, links
    /// and directives are exempt; only literal echoes are flagged.
    fn check_echo(&mut self, node: Node) {
        let Ok(text) = node.utf8_text(self.src) else {
            return;
        };
        let body = text.trim_start_matches(['#', '/', '*', '!']).trim();
        if self.is_commentary_not_echoable(node, text, body) {
            return;
        }
        let Some(target) = self.echo_target(node) else {
            return;
        };
        let Ok(code) = target.utf8_text(self.src) else {
            return;
        };
        let comment_tokens = word_set(body);
        if comment_tokens.len() < 2 {
            return;
        }
        // Only the target's first line: a comment above a 40-line function
        // was being matched against the entire body, so any word it shared
        // with any statement counted as an echo.
        let code_tokens = word_set(code.lines().next().unwrap_or(code));
        let echoed = comment_tokens
            .iter()
            .filter(|t| code_tokens.contains(*t))
            .count();
        if echoed as f32 / comment_tokens.len() as f32 >= ECHO_SHARE {
            self.facts
                .echo_comments
                .push(node.start_position().row as u32 + 1);
        }
    }

    /// Comments that are not claims about the line beside them: doc
    /// syntax (an interface contract), markers, links, tool directives,
    /// and paragraphs — adjacent comment lines are an explanation, and
    /// each line parses as its own node.
    fn is_commentary_not_echoable(&self, node: Node, text: &str, body: &str) -> bool {
        const MARKERS: &[&str] = &[
            "todo", "fixme", "note", "safety", "hack", "xxx", "type:", "noqa", "eslint", "pylint",
            "mypy", "clippy", "allow", "deny", "ruff", "coding", "!",
        ];
        let doc_markers = self.pack.doc_markers;
        let lower = body.to_ascii_lowercase();
        if text.starts_with("///")
            || text.starts_with("//!")
            || text.starts_with("/**")
            || doc_markers.iter().any(|m| text.starts_with(m))
            || lower.contains("http")
            || MARKERS.iter().any(|m| lower.starts_with(m))
        {
            return true;
        }
        let row = node.start_position().row;
        let in_paragraph = |sib: Option<Node>| {
            sib.is_some_and(|s| {
                self.sem_of(s) == Sem::Comment && s.start_position().row.abs_diff(row) == 1
            })
        };
        in_paragraph(node.prev_named_sibling()) || in_paragraph(node.next_named_sibling())
    }

    /// The code a comment is plausibly restating: what it trails on its
    /// own row, else what it introduces just below. A previous sibling
    /// that merely ENDS on this row is a multi-line construct whose
    /// closing delimiter we are labelling (`}` then `// name`), and a
    /// comment target is prose, not code — neither is a comparison.
    fn echo_target<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        let row = node.start_position().row;
        let target = match node.prev_named_sibling() {
            Some(prev) if prev.start_position().row == row => prev,
            _ => node
                .next_named_sibling()
                .filter(|n| n.start_position().row <= node.end_position().row + 2)?,
        };
        (self.sem_of(target) != Sem::Comment).then_some(target)
    }

    /// Bare-identifier callee of a call (or Rust macro) — the only call
    /// form whose target is decidable within one file. Method and
    /// qualified calls are deliberately out: their receivers need types.
    fn callee_simple_name(&self, call: Node) -> Option<&str> {
        let f = self.pack.call_target(call)?;
        if self.pack.table_sem(f) != Sem::Ident {
            return None;
        }
        f.utf8_text(self.src).ok()
    }

    /// A boolean operator starts a new sequence unless its parent is the same
    /// operator (`a and b and c` chains count once cognitively).
    fn bool_starts_seq(&self, node: Node) -> bool {
        let Some(parent) = node.parent() else {
            return true;
        };
        if self.sem_of(parent) != Sem::BoolOp {
            return true;
        }
        let op = |n: Node| {
            n.child_by_field_name(self.pack.bool_op_field)
                .and_then(|o| o.utf8_text(self.src).ok())
        };
        op(parent) != op(node)
    }
}

/// Is `literal` the whole value of this binding? Containment is not
/// enough: `const apiKey = defineSetting('chat.foo')` puts a setting id
/// inside the value of a credential-shaped name, and a string that
/// merely rides along in a call is an argument, not the credential.
fn assigns_to(binding: Node, literal: Node) -> bool {
    binding
        .child_by_field_name("right")
        .or_else(|| binding.child_by_field_name("value"))
        // Field-less grammars (Zig): the value is the last named child.
        .or_else(|| {
            let mut cursor = binding.walk();
            binding.named_children(&mut cursor).last()
        })
        .is_some_and(|v| {
            v.id() == literal.id()
                // Go wraps the value in an expression_list; a
                // single-child wrapper around the literal is the literal.
                || (v.named_child_count() == 1
                    && v.named_child(0).is_some_and(|c| c.id() == literal.id()))
        })
}

/// Is this name/value pair a hardcoded credential? The same rules the
/// extractor applies to source, exposed for configuration — a
/// ConfigMap and an env block are where a credential actually leaks,
/// and they deserve the tested judgment rather than a second one that
/// drifts from it.
pub fn is_leaked_credential(name: &str, value: &str) -> bool {
    !is_placeholder(value)
        && (vendor_key(value) || (promises_a_credential(name) && looks_like_a_key(value)))
}

/// Does this name promise a credential?
fn promises_a_credential(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    Extractor::SECRET_NAMES.iter().any(|s| lower.contains(s))
}

/// The shapes of a value that only pretends: sample keys from docs,
/// scaffolding, redactions. Checked before EITHER detection tier —
/// `ghp_example000000000` is documentation, not a leak.
fn is_placeholder(value: &str) -> bool {
    const PLACEHOLDERS: &[&str] = &[
        "example",
        "placeholder",
        "changeme",
        "your",
        "xxx",
        "todo",
        "dummy",
        "sample",
        "redacted",
        "none",
        "null",
        "fake",
        "mock",
        "stub",
        "smoketest",
        "fixture",
    ];
    let lower = value.to_ascii_lowercase();
    PLACEHOLDERS.iter().any(|p| lower.contains(p))
}

/// A literal shorter than this cannot hold PEM armor AND key material:
/// curl's examples build a placeholder PEM from separate strings, and
/// the bare `-----BEGIN PRIVATE KEY-----\n` line is armor, not a key.
const PEM_WITH_MATERIAL: usize = 60;

/// Vendor token tails below this are truncated examples, not keys.
const VENDOR_TAIL_MIN: usize = 12;

/// AWS access key ids are AKIA/ASIA plus exactly this many characters.
const AWS_TAIL_LEN: usize = 16;

/// Values that identify THEMSELVES as credentials: vendor key formats.
/// These need no credential-shaped name, because the prefix plus the
/// tail's shape is the vendor's own declaration of what the value is.
fn vendor_key(value: &str) -> bool {
    // A PEM block is a private key wherever it appears — but only when
    // this literal carries key MATERIAL, not just the marker.
    if value.contains("PRIVATE KEY-----") && value.len() >= PEM_WITH_MATERIAL {
        return true;
    }
    if url_credential(value) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "sk-ant-",
        "AIza",
    ];
    if let Some(tail) = PREFIXES.iter().find_map(|p| value.strip_prefix(p)) {
        return tail.len() >= VENDOR_TAIL_MIN
            && tail
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
    if let Some(tail) = value
        .strip_prefix("AKIA")
        .or_else(|| value.strip_prefix("ASIA"))
    {
        return tail.len() == AWS_TAIL_LEN
            && tail
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    }
    false
}

/// A call's arguments, however this grammar keeps them. Three shapes:
/// a fielded list, a Rust macro's token tree, and Zig's — which nests
/// arguments DIRECTLY under the call, where the function field is the
/// only child that is not one.
fn call_arguments<'t>(pack: &Pack, call: Node<'t>) -> Vec<Node<'t>> {
    // A fielded list, plus Rust macros' token tree.
    let mut list = call
        .child_by_field_name("arguments")
        .or_else(|| holder_child(call));
    // Swift wraps the list a second time: `call_suffix > value_arguments`.
    while let Some(suffix) = list.filter(|l| l.kind() == "call_suffix") {
        list = holder_child(suffix);
    }
    if let Some(list) = list {
        // Perl fields `f($x)`'s arguments as the ARGUMENT, and only
        // `f($x, $y)` as a list. A wrapper carries no meaning of its
        // own, so a node the ontology recognises IS the single
        // argument — reading its children instead handed `ok(1)` back
        // as no arguments at all.
        if pack.table_sem(list) != Sem::None {
            return vec![bare_argument(list)];
        }
        let mut cursor = list.walk();
        return list
            .named_children(&mut cursor)
            .map(bare_argument)
            .collect();
    }
    // REPEATED `argument` fields: OCaml spells `f a b c` with one field
    // per argument, so child_by_field_name would return only the first
    // and every argument after it would go unseen.
    let mut cursor = call.walk();
    let mut fielded = Vec::new();
    if cursor.goto_first_child() {
        loop {
            if cursor.field_name() == Some("argument") {
                fielded.push(cursor.node());
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    if !fielded.is_empty() {
        return fielded.into_iter().map(bare_argument).collect();
    }
    // Zig and Solidity nest arguments DIRECTLY under the call; the
    // function field is the only child that is not one.
    let func = call.child_by_field_name("function");
    let mut cursor = call.walk();
    call.named_children(&mut cursor)
        .filter(|n| func.is_none_or(|f| f.id() != n.id()))
        .map(bare_argument)
        .collect()
}

/// The first ancestor that spells MORE than this node does. Solidity
/// stacks an `expression` around every link of a member chain, so the
/// question "is my parent another link" has to look past them.
fn outer_node<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut up = node.parent();
    while let Some(p) = up.filter(|p| p.byte_range() == node.byte_range()) {
        up = p.parent();
    }
    up
}

/// The child that HOLDS a call's arguments, when no field names it:
/// a Rust macro's token tree, and the two list nodes Swift stacks.
fn holder_child<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| {
        matches!(
            c.kind(),
            "token_tree" | "arguments" | "value_arguments" | "call_suffix"
        )
    })
}

/// What a node WRAPS. PHP, C#, Solidity and Swift each put a node of
/// their own around every argument, and Solidity wraps every expression
/// in an `expression` besides, so the metrics that ask what something
/// IS — a bare boolean, a literal assertion subject, the receiver of a
/// member access — saw a wrapper and answered no.
///
/// A wrapper that adds NO TOKENS is transparent: `argument [true]`
/// spans exactly its child. `(true)` spans two characters more and
/// stays, because a parenthesis is something the author wrote.
fn bare_argument<'t>(mut node: Node<'t>) -> Node<'t> {
    while node.named_child_count() == 1 {
        let Some(inner) = node.named_child(0) else {
            break;
        };
        if inner.start_byte() != node.start_byte() || inner.end_byte() != node.end_byte() {
            break;
        }
        node = inner;
    }
    node
}

/// A bare word a member could be called — no dots, no subscripts, no
/// call. What a grammar CALLS such a node varies; what it looks like
/// does not.
fn is_a_plain_name(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(|c: char| c.is_ascii_digit())
        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// A class needs at least this many methods before "do they hang
/// together" is a question. One method is trivially cohesive.
const MIN_COHESION_METHODS: usize = 2;

/// Per class, how many disconnected groups its methods fall into
/// (Hitz & Montazeri's LCOM4). Two methods are connected when they
/// touch a member in common, or when one calls the other — both are
/// evidence they belong to the same object. One group means cohesive;
/// more means the class is several objects sharing a name.
fn class_cohesion(
    units: &[UnitFacts],
    callees: &[Vec<Box<str>>],
    sep: &str,
) -> Vec<super::ClassFact> {
    let mut by_class: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, u) in units.iter().enumerate() {
        // A method touching NO member is not part of the object's
        // state — it is a free function that happens to live in a
        // class, and counting it as its own island would say every
        // class with a helper is incoherent. Cohesion is a question
        // about the methods that DO share state.
        if !u.is_method || u.own_members.is_empty() {
            continue;
        }
        // `Store.get` belongs to `Store`; a free function has no owner.
        if let Some(owner) = u.qualname.rsplit_once(sep).map(|(owner, _)| owner) {
            by_class.entry(owner).or_default().push(i);
        }
    }
    let mut out: Vec<super::ClassFact> = by_class
        .into_iter()
        .filter(|(_, methods)| methods.len() >= MIN_COHESION_METHODS)
        .map(|(owner, methods)| class_fact(units, callees, owner, &methods))
        .collect();
    out.sort_by_key(|c| c.line);
    out
}

fn class_fact(
    units: &[UnitFacts],
    callees: &[Vec<Box<str>>],
    owner: &str,
    methods: &[usize],
) -> super::ClassFact {
    let first = methods.iter().map(|i| units[*i].line);
    let line = first.min().unwrap_or(1);
    super::ClassFact {
        name: owner.into(),
        line,
        groups: cohesion_groups(units, callees, methods),
    }
}

/// Connected components of the method graph, by union-find over a
/// small index set — a class with hundreds of methods is rare enough
/// that the quadratic pairing is cheaper than building an index.
fn cohesion_groups(units: &[UnitFacts], callees: &[Vec<Box<str>>], methods: &[usize]) -> u16 {
    let mut group: Vec<usize> = (0..methods.len()).collect();
    let root = |group: &Vec<usize>, mut i: usize| {
        while group[i] != i {
            i = group[i];
        }
        i
    };
    for a in 0..methods.len() {
        for b in (a + 1)..methods.len() {
            if !share_state(units, callees, methods[a], methods[b]) {
                continue;
            }
            let (ra, rb) = (root(&group, a), root(&group, b));
            group[rb] = ra;
        }
    }
    let distinct: std::collections::HashSet<usize> =
        (0..methods.len()).map(|i| root(&group, i)).collect();
    distinct.len() as u16
}

/// Do these two methods belong to the same object? Either they read
/// the same member, or one calls the other.
fn share_state(units: &[UnitFacts], callees: &[Vec<Box<str>>], a: usize, b: usize) -> bool {
    let shares_member = units[a]
        .own_members
        .iter()
        .any(|m| units[b].own_members.contains(m));
    // A call THROUGH the receiver — `self.len()` — is recorded as a
    // touched member named `len`, because that is what it looks like
    // to a syntax tree. Without this, regex's LookSet read as eleven
    // groups: `is_empty` calls `self.len()` and shares no field with
    // it, though they are plainly one object.
    let calls = |from: usize, to: usize| {
        let name = &units[to].name;
        units[from].own_members.contains(name) || callees[from].iter().any(|c| c == name)
    };
    shares_member || calls(a, b) || calls(b, a)
}

/// Does this literal OPEN an SQL statement? Anchored on purpose: a
/// log line mentioning "select" is prose, and only a string that
/// begins as a statement is one.
/// Node's documented synchronous API, spelled out. A trailing `Sync`
/// is not a family: `Sync` is also an ordinary domain noun (vscode's
/// entire user-data-SYNC feature writes performSync, triggerSync,
/// getKeysForSync, onDidFinishSync) and the conventional suffix for a
/// pure-CPU variant of a user API (summarizeDocumentSync,
/// computeDiffSync, mergeObjectSync). Reading the suffix cost 48 of
/// this metric's 137 gold false positives against 3 true ones — and
/// eight of the 48 were AWAITED, which alone proves they yield.
///
/// The lesson `parks_the_thread` already writes down for `sleep`: the
/// qualifier decides, and where there is no qualifier the name must be
/// one the platform documents.
const BLOCKING_SYNC: &[&str] = &[
    // fs
    "accessSync",
    "appendFileSync",
    "chmodSync",
    "chownSync",
    "closeSync",
    "copyFileSync",
    "cpSync",
    "existsSync",
    "fchmodSync",
    "fchownSync",
    "fdatasyncSync",
    "fstatSync",
    "fsyncSync",
    "ftruncateSync",
    "futimesSync",
    "globSync",
    "lchmodSync",
    "lchownSync",
    "linkSync",
    "lstatSync",
    "lutimesSync",
    "mkdirSync",
    "mkdtempSync",
    "openSync",
    "opendirSync",
    "readFileSync",
    "readSync",
    "readdirSync",
    "readlinkSync",
    "readvSync",
    "realpathSync",
    "renameSync",
    "rmSync",
    "rmdirSync",
    "statSync",
    "statfsSync",
    "symlinkSync",
    "truncateSync",
    "unlinkSync",
    "utimesSync",
    "writeFileSync",
    "writeSync",
    "writevSync",
    // child_process
    "execFileSync",
    "execSync",
    "spawnSync",
    // zlib
    "brotliCompressSync",
    "brotliDecompressSync",
    "deflateRawSync",
    "deflateSync",
    "gunzipSync",
    "gzipSync",
    "inflateRawSync",
    "inflateSync",
    "unzipSync",
    // crypto
    "checkPrimeSync",
    "generateKeyPairSync",
    "generateKeySync",
    "generatePrimeSync",
    "hkdfSync",
    "randomFillSync",
    "scryptSync",
    // Electron's modal dialogs, which block the main process
    "showMessageBoxSync",
    "showOpenDialogSync",
    "showSaveDialogSync",
];

/// Does this argument name a shell interpreter? A path is allowed —
/// `/bin/sh` and `/usr/bin/env bash` are how a script names one — and
/// the quotes come off first, because every language spells the name as
/// a literal.
/// How a call was found to reach a shell. The two routes carry
/// different evidence, so they license different findings: an
/// always-a-shell callee says nothing about which argument is the
/// command, while a `-c` says the very next thing IS one.
#[derive(Clone, Copy)]
enum ShellRoute<'t> {
    Always,
    /// A `-c` was found, so the shell will parse whatever follows it.
    /// `command` is that argument where the grammar keeps it separate.
    Interprets {
        command: Option<Node<'t>>,
    },
}

fn names_a_shell(text: &str) -> bool {
    const SHELLS: &[&str] = &[
        "sh",
        "bash",
        "zsh",
        "dash",
        "ksh",
        "csh",
        "tcsh",
        "fish",
        "ash",
        "busybox",
        "cmd",
        "cmd.exe",
        "powershell",
        "powershell.exe",
        "pwsh",
    ];
    let bare = text.trim().trim_matches(['"', '\'', '`']).trim();
    let name = bare.rsplit(['/', '\\']).next().unwrap_or(bare);
    SHELLS.contains(&name)
}

/// Is this file a one-shot script rather than a shipped program?
///
/// It exists for `blocking async` and for nothing else. That metric's
/// whole argument is that a blocking call stalls the EXECUTOR — every
/// other task in the process waits behind it — and a build step, a
/// codegen pass or a benchmark harness has no other task. 79 of the 110
/// findings left on gold after the Sync-name table landed were exactly
/// that: `fs.readFileSync` and `existsSync` inside `async function
/// main()` in vscode's build pipeline, where the alternative is
/// strictly worse code for no gain.
///
/// Deliberately coarse, and the cost is worth naming: a long-running
/// server parked under `scripts/` goes unjudged by this one metric. The
/// exemption is not extended to any other, because for every other one
/// the question a script raises is the same question a program does.
fn runs_once(path: &str) -> bool {
    const ONE_SHOT: &[&str] = &[
        "/build/",
        "/scripts/",
        "/script/",
        "/benchmarks/",
        "/benchmark/",
        "/perf-measures/",
        "/examples/",
        "/samples/",
        "/codegen/",
        "/tools/",
    ];
    // Leading separator added, so a directory at the ROOT of a
    // relative path matches the same rule as one further down:
    // `examples/demo.ts` and `pkg/examples/demo.ts` are both examples.
    let norm = format!("/{}", path.replace('\\', "/").trim_start_matches('/'));
    ONE_SHOT.iter().any(|d| norm.contains(d)) || crate::lang::config_file(&norm)
}

fn starts_a_statement(raw: &str) -> bool {
    // Each verb with the keyword that makes it a STATEMENT rather than
    // an English sentence. `"Update File Error: ..."` opens with the
    // word update and is prose; a real UPDATE reaches a SET.
    const SHAPES: &[(&str, &str)] = &[
        ("select ", " from "),
        ("insert into ", ""),
        ("update ", " set "),
        ("delete from ", ""),
        ("drop table", ""),
        ("drop index", ""),
        ("alter table", ""),
        ("create table", ""),
    ];
    let body = raw
        // A prefix sigil belongs to the syntax, not the statement: `f`
        // and `s` are alphabetic, `$` is C#'s and is not.
        .trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '$')
        .trim_start_matches(['"', '\'', '`'])
        .trim_start()
        .to_ascii_lowercase();
    SHAPES
        .iter()
        .any(|(verb, rest)| body.starts_with(verb) && body.contains(rest))
}

/// Does an interpolation land where a VALUE goes, rather than where
/// structure goes? The difference decides whether this is a
/// vulnerability or the idiom for it.
///
/// `WHERE id = ${x}` splices a value into the statement: injection.
/// `IN (${placeholders})` and `VALUES ${rows}` splice STRUCTURE — the
/// generated `?` markers whose values travel separately, which is how
/// a parameterized IN clause is written in every language. vscode does
/// the safe one five times and the unsafe one once, and a gate that
/// cannot tell them apart is not a gate.
fn interpolates_a_value(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    let mut at = 0;
    while let Some(open) = next_hole(&lower, at) {
        // Quotes around the hole are the author's, not the syntax's;
        // the sigil in front of it belongs to the language. Ruby and
        // Elixir write `#{...}`, and without the `#` every interpolated
        // query in both read as splicing into nothing.
        let mut before = lower[..open].trim_end_matches(['$', '#', ' ', '\t', '\'', '"']);
        while before.ends_with([' ', '\'', '"']) {
            before = before.trim_end_matches([' ', '\'', '"']);
        }
        // Two positions where an interpolation is the vulnerability: a
        // comparison, where a VALUE belongs, and an identifier slot,
        // where a table or column name belongs. Both splice untrusted
        // text into the statement's meaning.
        //
        // `IN (` and `VALUES ` are deliberately absent. That is the
        // placeholder generator — `IN (${ids.map(() => '?').join()})`
        // splices structure whose values travel separately, and it is
        // how a parameterized IN clause is written in every language.
        // vscode writes the safe one five times and the unsafe one
        // once; a gate that cannot tell them apart is not a gate.
        let value_slot = before.ends_with(['=', '<', '>']) || before.ends_with(" like");
        let name_slot = ["from", "join", "table", "into", "update"]
            .iter()
            .any(|k| before.ends_with(k));
        if value_slot || name_slot {
            return true;
        }
        at = open + 1;
    }
    false
}

/// Where the next interpolation hole opens: `{` for f-strings,
/// templates and `format!`, or a printf verb for `Sprintf`.
fn next_hole(lower: &str, from: usize) -> Option<usize> {
    let brace = lower[from..].find('{').map(|i| from + i);
    // Swift spells the hole `\(...)`, with no brace anywhere.
    let escaped = lower[from..].find("\\(").map(|i| from + i);
    let printf = lower[from..].match_indices('%').find_map(|(i, _)| {
        let verb = lower[from + i + 1..].chars().next()?;
        matches!(verb, 's' | 'd' | 'v' | 'q' | 'x').then_some(from + i)
    });
    [brace, escaped, printf].into_iter().flatten().min()
}

/// Does this string node carry an interpolation, as the grammar
/// names it? Plain literals have children too — a Python string is
/// three nodes — so the kind is what decides.
fn interpolates(node: Node) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).any(|c| {
        is_a_hole(c) || {
            // Perl hangs `$x` under the literal's `string_content`
            // rather than beside it, so the hole is a grandchild.
            let mut inner = c.walk();
            c.named_children(&mut inner).any(is_a_hole)
        }
    })
}

/// One interpolation hole, as its grammar names it. PHP and Perl give
/// the hole no node of its own — the spliced VARIABLE sits in the
/// literal — which is why both languages read as carrying no
/// interpolation at all, and their built-query gate never fired.
fn is_a_hole(node: Node) -> bool {
    matches!(
        node.kind(),
        "interpolation"
            | "template_substitution"
            | "string_interpolation"
            | "interpolated_expression"
            | "variable_name"
            | "scalar"
    )
}

/// Does this comment line promise work that has not happened? Only
/// the four conventional markers, and only as WORDS — `todos` in a
/// sentence and a variable named `fixme_count` are not promises.
pub fn carries_debt(line: &str) -> bool {
    const MARKERS: [&str; 4] = ["TODO", "FIXME", "HACK", "XXX"];
    // The comment's SUBJECT must be the debt. A marker that merely
    // appears inside a sentence is prose ABOUT markers — this file is
    // full of it, and the first draft dutifully reported its own
    // documentation as six years-zero TODOs.
    let body = strip_comment_marker(line);
    MARKERS.iter().any(|m| {
        let Some(rest) = body.strip_prefix(m) else {
            return false;
        };
        // `TODO:`, `TODO(ada):`, `TODO fix the retry`, or bare.
        match rest.chars().next() {
            None => true,
            Some(c) => matches!(c, ':' | '(' | ' ' | '\t' | '-' | '!'),
        }
    })
}

/// A comment line without its marker. Block comments carry a leading
/// `*` on continuation lines, which is decoration, not content.
fn strip_comment_marker(line: &str) -> String {
    // Ordered prefixes, longest first — NOT trim_start_matches, which
    // repeats: on `/// TODO` it strips `//` and leaves a stray slash,
    // so the body no longer began with the marker.
    let text = line.trim();
    let text = ["///", "//!", "//", "/**", "/*", "#!", "#"]
        .iter()
        .find_map(|p| text.strip_prefix(p))
        .unwrap_or(text);
    // A block comment's continuation lines are decorated with `*`.
    let text = text.strip_prefix("* ").unwrap_or(text);
    text.trim_end_matches("*/").trim().to_string()
}

/// A comment block must be at least this tall before it can be code:
/// one line is a note, and a single statement is as often an example
/// as an abandonment.
const MIN_COMMENTED_BLOCK: usize = 2;

/// Comment blocks that PARSE as this language. The cheap filter runs
/// first: prose does not end its lines in `;` or braces, and parsing
/// every license header in a corpus would cost more than the finding
/// is worth.
fn commented_out_code(pack: &Pack, lines: &[(u32, String)]) -> Vec<u32> {
    let mut found = Vec::new();
    let mut parser: Option<Parser> = None;
    for block in adjacent_blocks(lines) {
        let [(first, _), ..] = block else { continue };
        if block.len() < MIN_COMMENTED_BLOCK || !looks_like_code(block) {
            continue;
        }
        let source: String = block
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let parser = parser.get_or_insert_with(|| pack.make_parser());
        let Some(tree) = parser.parse(&source, None) else {
            continue;
        };
        let root = tree.root_node();
        if !root.has_error() && root.named_child_count() >= MIN_COMMENTED_BLOCK {
            found.push(*first);
        }
    }
    // Ascending, so the reported line is the FIRST one in the file
    // rather than whichever block the walk happened to reach first.
    found.sort_unstable();
    found
}

/// Runs of comment lines with no code between them.
fn adjacent_blocks(lines: &[(u32, String)]) -> Vec<&[(u32, String)]> {
    let mut blocks = Vec::new();
    let mut start = 0;
    for i in 1..=lines.len() {
        let broken = i == lines.len() || lines[i].0 != lines[i - 1].0 + 1;
        if broken {
            blocks.push(&lines[start..i]);
            start = i;
        }
    }
    blocks
}

/// Does this block carry code's punctuation? Prose does not end lines
/// with a semicolon or a brace, and the whole point of the filter is
/// that a parse is expensive and a license header is not code.
fn looks_like_code(block: &[(u32, String)]) -> bool {
    let coded = block
        .iter()
        .filter(|(_, t)| t.ends_with([';', '{', '}', ',']) || t.contains(" = "))
        .count();
    coded * 2 >= block.len() && coded > 0
}

/// One bare boolean is often a legitimate `force`/`recursive` flag.
/// Two is where a call site goes dark: nothing at `f(x, true, false)`
/// says which is which, and swapping them type-checks.
const MIN_BOOL_TRAP: usize = 2;

/// Shorter literals than this are punctuation, flags and format
/// fragments — `", "`, `"-v"`, `"%s"` — where repetition is not a
/// missing name. PROVISIONAL: chosen from the gold distribution.
const MIN_MAGIC_STRING: usize = 8;

/// How many distinct UNITS must write the same literal before the
/// repetition is a missing constant. Counting occurrences alone
/// measured data, not logic: gold's worst offenders were a TextMate
/// grammar repeating `include: '#ever_present_context'` 186 times
/// inside ONE object literal, and git's CLI tables. A table is
/// content — the same reason clone detection refuses duplicated data —
/// while a literal spelled out in three separate functions is a
/// decision nobody named.
const MAGIC_STRING_UNITS: usize = 3;

/// Literals written in that many distinct units, reported at the row
/// of the first — the point where naming it became overdue.
fn repeated_strings(rows: std::collections::HashMap<Box<str>, Vec<(u32, usize)>>) -> Vec<u32> {
    let mut out: Vec<u32> = rows
        .into_values()
        .filter(|sites| {
            sites
                .iter()
                .map(|(_, unit)| *unit)
                .collect::<std::collections::HashSet<_>>()
                .len()
                >= MAGIC_STRING_UNITS
        })
        .filter_map(|sites| sites.into_iter().map(|(row, _)| row).min())
        .collect();
    out.sort_unstable();
    out
}

/// The shortest password a connection string is trusted to carry. Below
/// this the userinfo is a fixture (`redis://:a@localhost`) or a scheme
/// artifact, not a credential worth a release to rotate.
const URL_PASS_MIN: usize = 8;

/// A password in a connection string: `postgres://admin:s3cr3t9x@db/prod`.
/// The NAME promises nothing here — `DATABASE_URL` is honestly a URL —
/// but the scheme itself defines the position the credential sits in,
/// which is exactly the vendor-prefix argument: the value declares what
/// it is. `looks_like_a_key` can never see this, because it disqualifies
/// on the `:`, `/` and `.` that every URL is made of.
fn url_credential(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    // Userinfo belongs to the authority, before any path or query —
    // and an authority holds no WHITESPACE, which is what separates a
    // connection string from a sentence that happens to quote a URL.
    // Without that, a tweet in trpc's gold corpus ("impressed by
    // @alexdotjs's http://trpc.io: ...") parsed its own prose as
    // userinfo.
    let authority = rest
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#'))
        .next()
        .unwrap_or(rest);
    // Last `@` wins: a password may legitimately contain one.
    let Some((userinfo, _host)) = authority.rsplit_once('@') else {
        return false;
    };
    let Some((_user, pass)) = userinfo.split_once(':') else {
        return false;
    };
    pass.len() >= URL_PASS_MIN
        && !is_placeholder(pass)
        // An interpolated password is a REFERENCE to a secret, which is
        // the remedy this metric recommends.
        && !pass.contains(['{', '$', '%', '<'])
        // A word is a doc placeholder; a credential mixes classes.
        && (pass.chars().any(|c| c.is_ascii_digit())
            || (pass.chars().any(|c| c.is_ascii_uppercase())
                && pass.chars().any(|c| c.is_ascii_lowercase())))
}

/// Does this literal carry the entropy of a real key? A placeholder, an
/// interpolation, or a plain English word does not.
fn looks_like_a_key(value: &str) -> bool {
    // Structure separators mean this NAMES a secret rather than being
    // one: an OAuth URN (colons), a dotted setting id, an interpolation.
    // A dot also separates a JWT's segments, so a hardcoded JWT goes
    // unseen — and that was checked rather than assumed. Every
    // JWT-shaped literal in the gold corpus lives in a test file, and
    // all of them are the same fixture, whose payload decodes to
    // {"message":"hello world"}. Teaching this gate the `eyJ` shape
    // would have bought seven false positives and no true one.
    if value.len() < 8
        || value.contains(['{', '$', '<', ' ', '%', ':', '.'])
        || value.starts_with('/')
    {
        return false;
    }
    // A slash is allowed only when the value has a base64 secret's
    // shape — mixed case AND digits AND real length (AWS secret keys) —
    // because a path is lowercase words: keys/prod/signing. The old
    // blanket '/' exclusion made the #1 cloud provider's secret format
    // invisible.
    if value.contains('/') {
        let upper = value.chars().any(|c| c.is_ascii_uppercase());
        let lower = value.chars().any(|c| c.is_ascii_lowercase());
        let digit = value.chars().any(|c| c.is_ascii_digit());
        let base64_shaped = upper && lower && digit && value.len() >= 20;
        if !base64_shaped {
            return false;
        }
    }
    // SCREAMING_SNAKE is an environment variable's name, and the module
    // that lists which env vars hold secrets is not the one leaking them.
    let shouted = value.chars().all(|c| !c.is_ascii_lowercase())
        && value.chars().any(|c| c.is_ascii_uppercase());
    if shouted {
        return false;
    }
    // Digits AND letters, no exceptions: a twenty-char value without a
    // single digit is a name, not a key — `OneTimePasswordField` and
    // `excalidraw-oai-api-key` both cleared the old length-alone arm,
    // and a random 20-char key lacks digits three times in a hundred.
    let has_digit = value.chars().any(|c| c.is_ascii_digit());
    let has_alpha = value.chars().any(|c| c.is_ascii_alphabetic());
    has_digit && has_alpha && longest_run(value) >= MIN_KEY_RUN
}

/// A credential is one long unbroken run of entropy; a slug is short
/// words wearing separators. `libsecret-1.so.0` mapped to
/// `libsecret-1-0` in playwright's native-dependency table has a
/// credential-shaped NAME and a value that is plainly a package id —
/// its longest run is nine. A UUID-format key still passes: its final
/// group alone is twelve.
const MIN_KEY_RUN: usize = 12;

fn longest_run(value: &str) -> usize {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::len)
        .max()
        .unwrap_or(0)
}

/// Python's synchronous I/O, judged by QUALIFIED spelling only. The
/// module-level verbs of `requests` and `httpx` are sync by their own
/// documentation (the async client is a method on a VARIABLE receiver,
/// which no name can decide, so it is never judged); `urlopen` names
/// nothing else in the ecosystem; and a bare `open()` inside an async
/// unit blocks where `aiofiles.open` (two segments, exempt by shape)
/// was the remedy.
fn python_parks(segs: &[&str]) -> bool {
    const SYNC_VERBS: &[&str] = &[
        "get", "post", "put", "delete", "head", "patch", "options", "request", "stream",
    ];
    match segs {
        ["requests" | "httpx", verb] => SYNC_VERBS.contains(verb),
        [.., "urlopen"] => true,
        ["open"] => true,
        _ => false,
    }
}

/// The pattern a binding site binds. An EMPTY field name means the
/// grammar labels nothing and the first named child is the target —
/// Zig spells `var x: u32 = 1` with an unfielded identifier, which is
/// why its live spans went untracked for as long as they did.
fn bound_pattern<'t>(node: Node<'t>, field: &str) -> Option<Node<'t>> {
    match field.is_empty() {
        true => node.named_child(0),
        false => node.child_by_field_name(field),
    }
}

/// The single bare identifier a reassignment writes, if that is what
/// it writes. Go wraps the target in an `expression_list`, so one
/// layer of single-child wrapper is unwrapped; two targets (a swap, a
/// multi-assign) or a member/index target (`self.x`, `a[i]`) answer
/// None — those mutate state THROUGH a name rather than rebinding it.
fn single_reassign_target<'t>(pack: &Pack, node: Node<'t>, field: &str) -> Option<Node<'t>> {
    let mut target = bound_pattern(node, field)?;
    // A keyword BEFORE the name makes this a declaration, not a write.
    // Zig spells `var x = 1` and `x = 3` with the same node kind, and
    // position is what separates them: a fresh binding always states a
    // keyword first, so its name cannot start where the node starts.
    // Without this, ghostty's six sibling-scope `const run = ...`
    // declarations read as five repurposings of the first.
    if target.start_byte() != node.start_byte() {
        return None;
    }
    while pack.table_sem(target) != Sem::Ident && target.named_child_count() == 1 {
        let inner = target.named_child(0)?;
        // Only a wrapper that ADDS NO TOKENS is transparent — Go's
        // one-element expression_list. A deref or a paren spells more
        // than its child: `*slot = true` writes through the pointer
        // and rebinds nothing, which the first self-scan proved by
        // flagging this repository's own `set_switch`.
        if inner.byte_range() != target.byte_range() {
            return None;
        }
        target = inner;
    }
    (pack.table_sem(target) == Sem::Ident).then_some(target)
}

/// Is this a unit React's hook rules govern — a component (capitalized)
/// or a custom hook (`useThing`)? A lowercase helper that happens to
/// call a hook is a FACTORY: the hooks it returns are called normally
/// by whoever uses them, and tRPC's `createTRPCNext` is the shape.
fn holds_hooks(name: &str) -> bool {
    name.starts_with(char::is_uppercase)
        || name
            .strip_prefix("use")
            .is_some_and(|rest| rest.starts_with(char::is_uppercase))
}

/// Does this file live in React's world at all? The rules-of-hooks
/// question is meaningless without it, and a `useX()` helper in an
/// unrelated codebase is just a function with a name.
fn uses_react(facts: &FileFacts) -> bool {
    facts.imports.iter().any(|i| {
        let target = &*i.target;
        matches!(target, "react" | "preact")
            || target.starts_with("react/")
            || target.starts_with("react-")
            || target.starts_with("preact/")
    })
}

/// How each language spells "everything else". `*` is shell's, and it
/// collides with nothing: a glob that is not bare (`*.txt`) reads as
/// its own label, and every other language quotes a literal asterisk.
const CATCH_ALL: &[&str] = &["_", "default", "else", "otherwise", "*"];

/// One arm's label. Catch-all arms may keep their keyword ANONYMOUS
/// (Zig's `else =>`, C/Go/TS `default:`), which left the arm's VALUE as
/// its first named child — the wildcard detector read `else => 0` as a
/// label of "0" and never fired in four languages. The recall harness
/// found it on its first run.
fn arm_label(arm: Node, src: &[u8]) -> String {
    let mut cursor = arm.walk();
    let keyword_arm = arm
        .children(&mut cursor)
        .any(|t| !t.is_named() && matches!(t.kind(), "else" | "default"));
    if keyword_arm {
        return "default".to_string();
    }
    let pat = arm
        .child_by_field_name("pattern")
        .or_else(|| arm.child_by_field_name("value"))
        .or_else(|| arm.named_child(0));
    pat.and_then(|p| p.utf8_text(src).ok())
        .unwrap_or("default")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Is this call's value thrown away? A spawn as a bare statement leaves
/// nobody able to await the task, observe its panic, or stop the runtime
/// dropping it mid-write at shutdown.
fn discards_its_result(call: Node) -> bool {
    call.parent()
        .is_some_and(|p| p.kind() == "expression_statement")
}

fn line_span(node: Node) -> u32 {
    (node.end_position().row - node.start_position().row) as u32 + 1
}

/// Lines a byte range covers, counted as `str::lines` counts them: a
/// trailing newline closes the last line rather than opening another.
/// One answer for all twenty-two languages, which is why it lives here
/// and not twenty-two times over in the packs.
fn line_count(text: &[u8]) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let breaks = text.iter().filter(|b| **b == b'\n').count() as u32;
    breaks + (text.last() != Some(&b'\n')) as u32
}

/// A definition's parameter list, wherever this grammar keeps it.
fn param_list<'t>(node: Node<'t>) -> Option<Node<'t>> {
    // Scala fields its TYPE parameters under the same name as its value
    // parameters, and the field lookup returns the first — so every
    // generic definition read as taking none, and `def compute[F[_]:
    // Monad](method: String, ...)` reported nine documented parameters
    // against an empty signature. A type parameter is never an
    // argument, in any grammar that names one.
    node.child_by_field_name("parameters")
        .filter(|p| p.kind() != "type_parameters")
        .or_else(|| node.child_by_field_name("parameter"))
        // Some grammars (Zig) leave the parameter list unfielded, and
        // Perl calls it a `signature` — which is also the one place a
        // Perl sub declares its parameters at all, so missing it made
        // every modern signature read as taking none.
        .or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| matches!(c.kind(), "parameters" | "signature"))
        })
        // Elixir's definition is a CALL to `def`, whose first argument
        // is the function head; the parameters are that head's own
        // arguments, one level further down than anywhere else.
        .or_else(|| {
            let mut c = node.walk();
            let head = node
                .named_children(&mut c)
                .find(|n| n.kind() == "arguments")?
                .named_child(0)
                .filter(|h| h.kind() == "call")?;
            let mut i = head.walk();
            head.named_children(&mut i)
                .find(|n| n.kind() == "arguments")
        })
        // OCaml lists parameters as direct children of the binding
        // rather than wrapping them, so the definition IS the list and
        // param_info declines the children that are not parameters.
        .or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .any(|c| c.kind() == "parameter")
                .then_some(node)
        })
        // C hides them in the declarator chain: function_definition ->
        // [pointer_]declarator -> function_declarator(parameters).
        .or_else(|| {
            let mut d = node.child_by_field_name("declarator")?;
            loop {
                if let Some(p) = d.child_by_field_name("parameters") {
                    return Some(p);
                }
                d = d.child_by_field_name("declarator")?;
            }
        })
}

/// Step-down narrative (Clean Code; Knuth): a file reads top-down when
/// intra-file calls point at units defined BELOW the caller. Module-level
/// calls are excluded — a `main()` guard at file end legitimately points
/// up.
fn step_refs(units: &[UnitFacts], callees: &[Vec<Box<str>>]) -> (u32, u32) {
    let mut order = std::collections::HashMap::new();
    for (idx, u) in units.iter().enumerate().skip(1) {
        order.entry(&*u.name).or_insert(idx);
    }
    let (mut down, mut up) = (0, 0);
    for (idx, names) in callees.iter().enumerate().skip(1) {
        for name in names {
            match order.get(&**name) {
                Some(&def) if def > idx => down += 1,
                Some(&def) if def < idx => up += 1,
                _ => {}
            }
        }
    }
    (down, up)
}

/// Entry points first, details after: of every (public, private) unit
/// pair, how many put the public one first.
fn pub_order(units: &[UnitFacts]) -> (u32, u32) {
    let (mut first, mut pairs) = (0, 0);
    let units = &units[1..];
    for (i, a) in units.iter().enumerate() {
        for b in &units[i + 1..] {
            match (a.is_public, b.is_public) {
                (true, false) => {
                    pairs += 1;
                    first += 1;
                }
                (false, true) => pairs += 1,
                _ => {}
            }
        }
    }
    (first, pairs)
}

/// Lowercased word set: splits on non-alphanumerics and camelCase humps,
/// drops one-char tokens and pure glue words.
fn word_set(text: &str) -> std::collections::HashSet<String> {
    const GLUE: &[&str] = &[
        "the", "of", "to", "in", "is", "for", "and", "or", "on", "with", "as", "be", "by", "if",
        "it", "we", "at", "an", "are", "was", "then", "when", "this", "that", "from", "into",
    ];
    let mut end = text.len().min(200);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = std::collections::HashSet::new();
    for raw in text[..end].split(|c: char| !c.is_alphanumeric()) {
        let mut start = 0;
        let mut prev_lower = false;
        for (i, c) in raw.char_indices() {
            if c.is_uppercase() && prev_lower {
                push_word(&raw[start..i], GLUE, &mut out);
                start = i;
            }
            prev_lower = c.is_lowercase();
        }
        push_word(&raw[start..], GLUE, &mut out);
    }
    out
}

fn push_word(word: &str, glue: &[&str], out: &mut std::collections::HashSet<String>) {
    if word.len() >= 2 {
        let w = word.to_ascii_lowercase();
        if !glue.contains(&w.as_str()) {
            out.insert(w);
        }
    }
}

/// splitmix64-style combiner: order-sensitive, statistically strong, cheap.
fn mix(h: u64, x: u64) -> u64 {
    let mut z = h ^ x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;

    #[test]
    fn a_debt_marker_leads_its_note_rather_than_appearing_in_one() {
        // The conventional forms, in the comment styles that carry them.
        assert!(carries_debt("# TODO: retry on timeout"));
        assert!(carries_debt("    // TODO(ada): retry on timeout"));
        assert!(carries_debt("// FIXME the retry is wrong"));
        assert!(carries_debt(" * HACK"));
        assert!(carries_debt("/// TODO: handle the error case"));
        // Prose ABOUT markers is not a marker. This repository is full
        // of it, and the first draft reported its own documentation as
        // six TODOs aged zero days.
        assert!(!carries_debt(
            "/// TODO, FIXME, HACK and XXX, dated by the commit"
        ));
        assert!(!carries_debt("// markers — TODO/FIXME/HACK/XXX — in a"));
        assert!(!carries_debt("// a variable named todo_count is not one"));
        assert!(!carries_debt("// nothing here promises anything"));
    }

    fn facts(source: &str) -> FileFacts {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new("test.py"), source)
    }

    /// A receiver spelled with a sigil is still the receiver.
    ///
    /// `selfish_base` compared the chain base's RAW text against
    /// self|cls|this, which PHP's `$this` and Perl's `$self` can never
    /// equal — so every method in both languages was read as envying a
    /// foreign object that is itself. 993 of PHP's 1138 gold feature-envy
    /// findings named `$this`; 195 of Perl's 382 named `$self`. That
    /// branch is also the only writer of `own_members`, so both languages
    /// measured zero cohesion classes.
    ///
    /// The Ruby row is the guard on the other side: stripping one sigil
    /// must not promote an instance variable to the receiver.
    #[test]
    fn a_receiver_spelled_with_a_sigil_is_not_a_foreign_object() {
        use crate::lang::Lang;
        // (lang, file, source, unit, envied object, times envied,
        //  self accesses, members touched, what the row proves)
        #[allow(clippy::type_complexity)]
        const SIGIL: &[(Lang, &str, &str, &str, &str, u16, u16, &str, &str)] = &[
            // PHP: `$this` is the only receiver the language has.
            (
                Lang::Php,
                "Repo.php",
                "<?php\nclass Repo {\n  function put($k) {\n    $this->cache[$k] = 1;\n    $this->log[] = $k;\n    return $this->cache;\n  }\n}\n",
                "put",
                "",
                0,
                3,
                "cache,log",
                "PHP: $this is the receiver, and cache/log are its own members",
            ),
            // Perl: `$self` is a local bound from @_, not a parameter, so
            // no pack hook can mark it selfish — only the text can. The
            // chain here is a method call, which is what Perl's `attr` is.
            (
                Lang::Perl,
                "Repo.pm",
                "package Repo;\nsub put {\n  my ($self, $k) = @_;\n  $self->cache($k);\n  $self->notes($k);\n  return $self->cache;\n}\n1;\n",
                "put",
                "",
                0,
                3,
                "cache,notes",
                "Perl: $self is the receiver",
            ),
            // Ruby: an instance variable is state the object HOLDS, not
            // the object. `@config` must stay foreign after one sigil
            // comes off — and `@config.cache[k] = 1` must not make it
            // this unit's OWN either, which is what binds nothing.
            (
                Lang::Ruby,
                "repo.rb",
                "class Repo\n  def put(k)\n    @config.cache[k] = 1\n    @config.log[k] = 1\n    @config.cache\n  end\nend\n",
                "put",
                "@config",
                3,
                0,
                "",
                "Ruby: @config is an ivar, not the receiver",
            ),
            // Solidity: `$` is a whole identifier, and OpenZeppelin's
            // ERC-7201 storage pointer is called exactly that. Stripping
            // it to the empty string made it equal the empty receiver
            // name a unit with no declared receiver carries.
            //
            // It arrives as a PARAMETER so the sigil rule is the only
            // thing under test: were it declared in the body, the
            // own-object rule would withdraw it first and the row would
            // pass whatever `unsigiled` did.
            (
                Lang::Solidity,
                "S.sol",
                "contract S {\n  function f(Layout storage $) internal {\n    bool a = $._initializing;\n    uint64 b = $._initialized;\n    $._initializing = a;\n    $._initialized = b;\n  }\n}\n",
                "f",
                "$",
                4,
                0,
                "",
                "Solidity: a bare $ is a name, not a sigil",
            ),
        ];
        for (lang, file, src, unit, object, count, selves, members, why) in SIGIL {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new(file), src);
            let u = f
                .units
                .iter()
                .find(|u| u.name.as_ref() == *unit)
                .unwrap_or_else(|| panic!("{lang:?}: no unit {unit}"));
            assert_eq!(
                (
                    &*u.envy_object,
                    u.envy_count,
                    u.self_accesses,
                    u.own_members.join(",")
                ),
                (*object, *count, *selves, (*members).to_string()),
                "{why}",
            );
        }
    }

    #[test]
    fn a_documented_trait_default_is_not_a_bare_literal_declaration() {
        // Every one of `ceremony`'s 79 gold false positives was this
        // shape: a trait or interface member whose literal body is a
        // DEFAULT, documented at length so implementors know when to
        // replace it. Excluding them cost 6 real hits out of 3,187.
        use crate::lang::Lang;
        let over = |lang: Lang, name: &str, src: &str, unit: &str| -> bool {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new(name), src);
            f.units
                .iter()
                .find(|u| u.name.as_ref() == unit)
                .unwrap_or_else(|| panic!("{lang:?}: no unit {unit}"))
                .is_override
        };
        // Rust: a trait body reaches this through the generic
        // `interfaces` check, an `impl Trait for` through the hook.
        assert!(over(
            Lang::Rust,
            "t.rs",
            "trait Bounded {\n    /// a\n    /// b\n    /// c\n    fn min_len(&self) -> usize { 1 }\n}\n",
            "min_len",
        ));
        assert!(over(
            Lang::Rust,
            "t.rs",
            "trait T { fn f(&self) -> bool; }\nimpl T for S {\n    fn f(&self) -> bool { true }\n}\n",
            "f",
        ));
        // An inherent impl implements nobody's contract.
        assert!(!over(
            Lang::Rust,
            "t.rs",
            "impl S {\n    /// a\n    /// b\n    /// c\n    fn ready(&self) -> bool { true }\n}\n",
            "ready",
        ));
        // Java: `@Override` inside an ordinary class, which the generic
        // check cannot see.
        assert!(over(
            Lang::Java,
            "T.java",
            "class T {\n  @Override\n  public boolean isEmpty() { return true; }\n}\n",
            "isEmpty",
        ));
        assert!(!over(
            Lang::Java,
            "T.java",
            "class T {\n  public boolean ready() { return true; }\n}\n",
            "ready",
        ));
        // A free function is nobody's override.
        assert!(!over(
            Lang::Rust,
            "t.rs",
            "fn ready() -> bool { true }\n",
            "ready"
        ));
    }

    #[test]
    fn a_bare_literal_body_is_told_apart_from_a_real_one() {
        // Ceremony reads this, and it splits booleans from other
        // literals because naming a number is how a codebase avoids
        // magic numbers — `const_item` sits in every pack's
        // `magic_exempt` for that reason. Naming a boolean that asserts
        // project state is a different act.
        let shapes = |lang: crate::lang::Lang, name: &str, src: &str| -> Vec<(String, BodyShape)> {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new(name), src);
            f.units
                .iter()
                .map(|u| (u.name.to_string(), u.body))
                .collect()
        };
        use crate::lang::Lang;
        let cases: &[(Lang, &str, &str)] = &[
            (
                Lang::Rust,
                "t.rs",
                "fn b() -> bool { true }\nfn n() -> u32 { 42 }\nfn s() -> &'static str { \"x\" }\n\
                 fn r() -> u32 { compute() }\nfn two() -> (u32, u32) { (1, 2) }\nfn neg() -> i32 { -1 }\n",
            ),
            (
                Lang::Python,
                "t.py",
                "def b():\n    return True\ndef n():\n    return 42\ndef r():\n    return compute()\n",
            ),
            (
                Lang::TypeScript,
                "t.ts",
                "function b() { return true; }\nfunction n() { return 42; }\nfunction r() { return compute(); }\n",
            ),
            (
                Lang::Go,
                "t.go",
                "package p\nfunc B() bool { return true }\nfunc N() int { return 42 }\nfunc R() int { return compute() }\n",
            ),
        ];
        for (lang, name, src) in cases {
            let got = shapes(*lang, name, src);
            let by = |want: &str| {
                got.iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(want))
                    .map(|(_, s)| *s)
            };
            assert_eq!(by("b"), Some(BodyShape::BoolLiteral), "{lang:?} bare bool");
            assert_eq!(by("n"), Some(BodyShape::Literal), "{lang:?} bare number");
            assert_eq!(
                by("r"),
                Some(BodyShape::Real),
                "{lang:?} a call is not a literal"
            );
        }
        // Rust-only shapes: a string is a literal, a tuple of two builds
        // something, and a negated number is still one number.
        let rust = shapes(
            Lang::Rust,
            "t.rs",
            "fn s() -> &'static str { \"x\" }\nfn two() -> (u32, u32) { (1, 2) }\nfn neg() -> i32 { -1 }\n",
        );
        let find = |want: &str| rust.iter().find(|(n, _)| n == want).map(|(_, s)| *s);
        assert_eq!(find("s"), Some(BodyShape::Literal));
        assert_eq!(
            find("two"),
            Some(BodyShape::Real),
            "two literals build a value"
        );
        assert_eq!(find("neg"), Some(BodyShape::Literal), "-1 is one number");
    }

    #[test]
    fn a_tree_too_deep_to_walk_is_disqualified_rather_than_fatal() {
        // `walk` and `scan` recurse together over data-controlled depth,
        // on a rayon worker whose stack the splitter has already been
        // using. A left-nested operator chain is as deep as it is long,
        // and without the guard this aborts the process — which is how
        // it was found, in a coredump from a vscode scan.
        let chain: String = (0..MAX_TREE_DEPTH as usize + 50)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" + ");
        let pack = crate::lang::Lang::TypeScript.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("deep.ts"),
            &format!("const x = {chain};\n"),
        );
        assert!(f.too_deep, "the walk stopped instead of running out");
        assert!(
            f.low_confidence(),
            "a truncated tree must not enter the distributions"
        );

        // An ordinary file is untouched by the guard.
        let ok = facts("def f(x):\n    return x + 1\n");
        assert!(!ok.too_deep);
        assert!(!ok.low_confidence());
    }

    #[test]
    fn module_unit_first_then_functions_in_source_order() {
        let f = facts("x = 1\n\ndef a():\n    pass\n\ndef b():\n    def inner():\n        pass\n");
        let names: Vec<&str> = f.units.iter().map(|u| &*u.name).collect();
        assert_eq!(names, ["<module>", "a", "b", "inner"]);
        assert!(f.units[0].is_module);
    }

    #[test]
    fn methods_skip_receiver_and_detect_flags() {
        let f = facts("class C:\n    def m(self, verbose=False, n: int = 0):\n        return n\n");
        let m = &f.units[1];
        assert!(m.is_method);
        let names: Vec<&str> = m.params.iter().map(|p| &*p.name).collect();
        assert_eq!(names, ["verbose", "n"]);
        assert_eq!(m.flag_params(), 1);
    }

    #[test]
    fn elif_chain_stays_flat() {
        let f = facts(
            "def f(a):\n    if a == 1:\n        return 1\n    elif a == 2:\n        return 2\n    else:\n        return 3\n",
        );
        let u = &f.units[1];
        assert_eq!(u.max_vis_depth, 1);
        let sems: Vec<Sem> = u.ctrl.iter().map(|c| c.sem).collect();
        assert_eq!(sems, [Sem::If, Sem::ElseIf, Sem::Else]);
    }

    #[test]
    fn bool_sequences_dedup_same_operator_only() {
        let f = facts("def f(a, b, c):\n    return a and b and c or a\n");
        let new_seqs: Vec<bool> = f.units[1].ctrl.iter().map(|c| c.new_seq).collect();
        // `or` at root: new; `and` chain below it: new once, continuation once.
        assert_eq!(new_seqs.iter().filter(|&&n| n).count(), 2);
        assert_eq!(new_seqs.len(), 3);
    }

    #[test]
    fn recursion_and_docstrings() {
        let f = facts("def f(n):\n    \"\"\"doc\n    line\"\"\"\n    return f(n - 1)\n");
        let u = &f.units[1];
        assert!(u.self_recursive);
        assert_eq!(u.doc_lines, 2);
        assert_eq!(f.comment_lines, 2);
    }

    #[test]
    fn public_surface_and_contract_docs_per_language() {
        // Python: underscore convention; docstring is the contract doc.
        let f = facts(
            "def api(x):\n    \"\"\"doc\"\"\"\n    return x\n\ndef _internal():\n    pass\n\ndef bare(y):\n    return y\n",
        );
        assert!(f.units[1].is_public && f.units[1].doc_lines == 1);
        assert!(!f.units[2].is_public);
        assert!(f.units[3].is_public && f.units[3].doc_lines == 0);

        // Rust: bare pub only; /// runs count, attributes in between allowed.
        let pack = crate::lang::Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let rf = extract(
            pack,
            &mut parser,
            Path::new("t.rs"),
            "/// Does the thing.\n/// Well.\n#[inline]\npub fn api() {}\n\npub(crate) fn internal() {}\n\nfn private() {}\n",
        );
        assert!(rf.units[1].is_public);
        assert_eq!(rf.units[1].doc_lines, 2);
        assert!(!rf.units[2].is_public);
        assert!(!rf.units[3].is_public);

        // TS: export reach + JSDoc, including promoted arrows.
        let pack = crate::lang::Lang::TypeScript.pack();
        let mut parser = pack.make_parser();
        let tf = extract(
            pack,
            &mut parser,
            Path::new("t.ts"),
            "/** Adds. */\nexport function add(a: number, b: number) { return a + b; }\n\n/** Doubles. */\nexport const double = (x: number) => x * 2;\n\nfunction local() {}\n",
        );
        assert!(tf.units[1].is_public && tf.units[1].doc_lines == 1);
        assert!(tf.units[2].is_public && tf.units[2].doc_lines == 1);
        assert!(!tf.units[3].is_public);
    }

    #[test]
    fn nested_functions_measured_separately() {
        let f = facts(
            "def outer(xs):\n    if xs:\n        def inner(y):\n            if y:\n                return y\n        return inner\n",
        );
        let outer = &f.units[1];
        let inner = &f.units[2];
        // outer sees its own `if` only; inner starts fresh at depth 0.
        assert_eq!(outer.ctrl.len(), 1);
        assert_eq!(inner.ctrl.len(), 1);
        assert_eq!(inner.ctrl[0].cog_depth, 0);
        assert_eq!(inner.max_vis_depth, 1);
    }

    #[test]
    fn assigned_lambda_becomes_named_unit() {
        let f = facts("double = lambda x: x * 2\n");
        assert_eq!(&*f.units[1].name, "double");
        assert_eq!(&*f.units[1].params[0].name, "x");
    }

    #[test]
    fn qualnames_carry_the_scope_path() {
        let f = facts(
            "class Outer:\n    class Inner:\n        def m(self):\n            def helper():\n                pass\n",
        );
        let quals: Vec<&str> = f.units.iter().map(|u| &*u.qualname).collect();
        assert_eq!(quals, ["<module>", "Outer.Inner.m", "Outer.Inner.m.helper"]);

        let pack = crate::lang::Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let rf = extract(
            pack,
            &mut parser,
            Path::new("t.rs"),
            "impl Widget<'_> {\n    fn draw(&self) {}\n}\n",
        );
        assert_eq!(&*rf.units[1].qualname, "Widget::draw");
    }

    #[test]
    fn comments_do_not_affect_clone_identity() {
        let a = facts(
            "def f(xs):\n    total = 0\n    for x in xs:\n        if x > 0:\n            total += x\n    return total\n",
        );
        let b = facts(
            "def f(xs):\n    # running sum\n    total = 0\n    for x in xs:  # each\n        if x > 0:\n            total += x\n    return total\n",
        );
        let ha: Vec<u64> = a.clone_sites.iter().map(|s| s.hash).collect();
        let hb: Vec<u64> = b.clone_sites.iter().map(|s| s.hash).collect();
        assert!(!ha.is_empty());
        assert!(ha.iter().all(|h| hb.contains(h)));
    }

    #[test]
    fn passthrough_forwards_own_params_adapters_do_not() {
        let f = facts(
            "class S:\n    def get(self, key, default=None):\n        return self._store.get(key, default)\n\n    def norm(self, key):\n        return self._store.get(key.lower())\n\n    def partial(self, key, default=None):\n        return self._store.get(key)\n",
        );
        assert!(f.units[1].is_passthrough, "full forward");
        assert!(!f.units[2].is_passthrough, "transforms its argument");
        assert!(
            f.units[3].is_passthrough,
            "prefix forward (default dropped)"
        );

        let pack = crate::lang::Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let rf = extract(
            pack,
            &mut parser,
            Path::new("t.rs"),
            "fn area(shape: &Shape) -> f64 {\n    inner_area(shape)\n}\n\nfn scaled(shape: &Shape, k: f64) -> f64 {\n    inner_area(shape) * k\n}\n",
        );
        assert!(rf.units[1].is_passthrough);
        assert!(!rf.units[2].is_passthrough, "computes on the result");
    }

    /// The three forwards the AUTHOR did not choose, each with the
    /// freely-chosen twin beside it.
    ///
    /// 4,933 of the 13,816 gold findings were one of these: a declared
    /// override (2,582), a lambda (1,220), a constructor re-declaring
    /// its superclass's (1,131). Fowler's Middle Man is a class that
    /// could be INLINED, and none of the three can be.
    #[test]
    fn a_forward_the_language_demanded_is_not_a_middle_man() {
        use crate::lang::Lang;
        let forwards = |lang: Lang, name: &str, src: &str, unit: &str| {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new(name), src);
            f.units
                .iter()
                .find(|u| u.name.as_ref() == unit)
                .unwrap_or_else(|| panic!("{lang:?}: no unit {unit}"))
                .is_passthrough
        };
        let java = "class D implements L {\n  private final L l;\n  @Override\n  public void onRead(C ctx, int id) { l.onRead(ctx, id); }\n  public void relay(C ctx, int id) { l.onRead(ctx, id); }\n}\n";
        assert!(
            !forwards(Lang::Java, "D.java", java, "onRead"),
            "@Override implements someone else's signature"
        );
        assert!(
            forwards(Lang::Java, "D.java", java, "relay"),
            "the same body without the marker is a layer the author chose"
        );
        let ts = "export const codec = {\n  decode: (str: string) => parse(str),\n};\nexport function decode2(str: string) { return parse(str); }\n";
        assert!(
            !forwards(Lang::TypeScript, "a.ts", ts, "decode"),
            "a lambda has no name of its own to be shallow about"
        );
        assert!(
            forwards(Lang::TypeScript, "a.ts", ts, "decode2"),
            "a named function does"
        );
        let ctor = "class A extends B {\n  constructor(cause: Error) { super(cause); }\n}\nclass C {\n  wrap(cause: Error) { return this.inner.wrap(cause); }\n}\n";
        assert!(
            !forwards(Lang::TypeScript, "b.ts", ctor, "constructor"),
            "a subclass must re-declare an inherited constructor"
        );
        assert!(
            forwards(Lang::TypeScript, "b.ts", ctor, "wrap"),
            "forwarding to a COLLABORATOR is still a hop"
        );
    }

    #[test]
    fn zero_argument_accessors_are_not_pass_through() {
        // A zero-argument callee used to match vacuously (an empty zip
        // is all-true), making every one-line accessor a Middle Man.
        let pack = crate::lang::Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.rs"),
            "impl C {\n    fn count(&self) -> usize {\n        self.bits.len()\n    }\n\n    fn render(&self, w: u32) -> String {\n        draw(w)\n    }\n}\n",
        );
        assert!(
            !f.units[1].is_passthrough,
            "accessor computes, forwards nothing"
        );
        assert!(f.units[2].is_passthrough, "forwards its own parameter");
    }

    #[test]
    fn negations_flag_provable_inversions_only() {
        let f = facts(
            "def f(a, b, found, not_ready):\n    if not a != b:\n        pass\n    if not (a and b):\n        pass\n    if not not_ready:\n        pass\n    if not found:\n        pass\n",
        );
        // `not a != b` inverts a comparison and `not not_ready` inverts a
        // negative name. `not (a and b)` is a De Morgan candidate and is
        // deliberately NOT one: distributing it is usually longer and
        // worse, and it was 80.5% of this metric on gold. `not found` is
        // a clean single negation of a positive name.
        assert_eq!(f.units[1].negations, 2);
    }

    #[test]
    fn double_negation_is_coercion_not_inverted_logic() {
        // `!!x` casts to bool; it was 78% of this metric's TS firings.
        // Neither line here is a finding: the second is De Morgan.
        let pack = crate::lang::Lang::TypeScript.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.ts"),
            "function f(x: unknown, ok: boolean, missing: boolean) {\n    const a = !!x;\n    if (!(ok && other)) { return 1; }\n    if (!missing) { return 2; }\n    return a;\n}\n",
        );
        assert_eq!(
            f.units[1].negations, 1,
            "the negative name only: coercion and De Morgan are exempt"
        );
    }

    #[test]
    fn error_discipline_flags_silent_and_broad_handlers() {
        let f = facts(
            "def f():\n    try:\n        g()\n    except ValueError:\n        pass\n    try:\n        g()\n    except Exception as e:\n        log(e)\n    try:\n        g()\n    except KeyError as e:\n        raise Wrapped(e)\n",
        );
        let u = &f.units[1];
        assert_eq!(u.swallowed, 1, "except: pass silences");
        assert_eq!(u.broad_catch, 1, "except Exception is too broad");

        let pack = crate::lang::Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let rf = extract(
            pack,
            &mut parser,
            Path::new("t.rs"),
            "fn prod(x: Option<u32>) -> u32 {\n    x.unwrap()\n}\n\n#[test]\nfn check() {\n    assert_eq!(prod(Some(1)).unwrap_or(1), 1);\n}\n",
        );
        assert_eq!(rf.units[1].unwraps, 1);
        assert!(!rf.units[1].is_test);
        assert!(rf.units[2].is_test, "#[test] attribute detected");
    }

    #[test]
    fn feature_envy_needs_a_dominant_foreign_receiver() {
        let f = facts(
            "class Billing:\n    def total(self, order):\n        return order.base + order.tax + order.shipping + order.discount\n\n    def label(self, order):\n        return self.prefix + self.sep + order.id\n",
        );
        let envious = &f.units[1];
        assert_eq!(envious.envy_count, 4);
        assert_eq!(&*envious.envy_object, "order");
        assert_eq!(envious.self_accesses, 0);
        let balanced = &f.units[2];
        assert!(balanced.self_accesses > balanced.envy_count);
    }

    #[test]
    fn demeter_flags_data_chains_not_fluent_calls() {
        let f = facts(
            "def f(cfg):\n    a = cfg.db.conn.host\n    b = self.registry.entries\n    c = query.filter(x).order_by(y).limit(3)\n    return a, b, c\n",
        );
        // cfg.db.conn.host = 3 data links: violation. self.registry.entries
        // = 2 links minus self-forgiveness = 1. Fluent chain: calls break it.
        assert_eq!(f.units[1].demeter, 1);
    }

    /// C has no Demeter, and the matrix now says so — enforced here,
    /// because a declared-dead row that nothing checks is how
    /// `shelled out` stayed alive in shell at precision zero for as
    /// long as it did.
    #[test]
    fn a_record_path_is_not_a_message_to_a_stranger() {
        use crate::lang::Lang;
        let chains = |lang: Lang, name: &str, src: &str| {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            extract(pack, &mut parser, Path::new(name), src)
                .units
                .iter()
                .map(|u| u.demeter)
                .sum::<u16>()
        };
        let body =
            "int f(struct S *s) {\n  return s->layout.sparse.offsets[0] + s->layout.dense.n;\n}\n";
        assert_eq!(
            chains(Lang::C, "a.c", body),
            0,
            "a nested record path has no delegate to hide"
        );
        assert!(
            chains(Lang::Cpp, "a.cpp", body) > 0,
            "C++ keeps the cell: it has methods, so a reach through one \
             object to another's field really is one"
        );
    }

    #[test]
    fn a_module_path_is_not_a_chain_into_a_neighbours_data() {
        // Same three links, three bases. `torch` is imported, so the
        // chain is namespace.namespace.namespace.function — 520 of the
        // 890 gold Python findings were this. `Config` is capitalised,
        // so it is a type or a module whatever the file imports. `cfg`
        // is an object and stays a finding.
        let f = facts(
            "import torch\n\ndef f(cfg):\n    torch.nn.functional.pad(x)\n    Config.db.conn.host\n    return cfg.db.conn.host\n",
        );
        assert_eq!(f.units[1].demeter, 1);

        // Withdrawn even when the import is read after the use: a lazy
        // import inside a body must not judge differently.
        let late = facts(
            "def f(cfg):\n    import torch\n    torch.nn.functional.pad(x)\n    return cfg.db.conn.host\n",
        );
        assert_eq!(late.units[1].demeter, 1);
    }

    /// An object the unit built is not a neighbour it envies.
    ///
    /// This was the largest single false class in the whole tool: 8,035
    /// of 19,424 gold feature-envy findings named a target the flagging
    /// unit had DECLARED — `JsonReader reader = new JsonTextReader(..)`
    /// followed by four calls on it. There is nowhere to move such a
    /// method to, so the smell cannot apply.
    ///
    /// Each row keeps a CONTROL beside the excluded shape, because the
    /// rule must not become "any name the unit mentions": a receiver
    /// passed IN is still foreign, and parameters are deliberately
    /// absent from every pack's `def_sites` so the two stay separable.
    #[test]
    fn an_object_the_unit_declared_is_not_a_foreign_receiver() {
        use crate::lang::Lang;
        let envied = |lang: Lang, name: &str, src: &str, unit: &str| {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new(name), src);
            let u = f
                .units
                .iter()
                .find(|u| u.name.as_ref() == unit)
                .unwrap_or_else(|| panic!("{lang:?}: no unit {unit}"));
            (u.envy_object.to_string(), u.envy_count)
        };
        // C#: the shape that dominated the metric. `reader` is born on
        // the first line of the body, so the four calls on it are the
        // method's own working state.
        let cs_body = |decl: &str| {
            format!(
                "class Host {{\n  void Load(string text) {{\n    {decl}\n    reader.Read();\n    reader.Skip();\n    reader.Close();\n    reader.Dispose();\n  }}\n}}\n"
            )
        };
        assert_eq!(
            envied(
                Lang::CSharp,
                "Host.cs",
                &cs_body("JsonReader reader = new JsonTextReader(text);"),
                "Load",
            ),
            (String::new(), 0),
            "C#: a local the method constructed is its own",
        );
        // Control: the SAME four calls on a receiver handed in.
        assert_eq!(
            envied(
                Lang::CSharp,
                "Host.cs",
                "class Host {\n  void Load(JsonReader reader) {\n    reader.Read();\n    reader.Skip();\n    reader.Close();\n    reader.Dispose();\n  }\n}\n",
                "Load",
            ),
            ("reader".to_string(), 4),
            "C#: a parameter is still someone else's object",
        );
        // Python: same rule, and the second-most-envied name wins once
        // the declared one is withdrawn.
        assert_eq!(
            envied(
                Lang::Python,
                "host.py",
                "class Host:\n    def load(self, order):\n        buf = Buffer()\n        buf.a\n        buf.b\n        buf.c\n        order.x\n        order.y\n",
                "load",
            ),
            ("order".to_string(), 2),
            "Python: withdrawing the local leaves the real neighbour",
        );
    }

    #[test]
    fn live_span_measures_definition_to_last_use() {
        let f = facts(
            "def f(xs):\n    total = 0\n    other = 1\n    use(other)\n    a = 2\n    b = 3\n    c = 4\n    use(a, b, c)\n    return total\n",
        );
        let u = &f.units[1];
        // total: defined line 2, last used line 9 -> span 7; other: 1.
        assert_eq!(u.max_live_span, 7);
        assert_eq!(&*u.max_live_var, "total");
    }

    #[test]
    fn spooky_constructs_are_counted_with_lines() {
        let f = facts(
            "def f(x=[]):\n    eval(x)\n    getattr(f, 'name')\n    getattr(f, x)\n\nclass M(metaclass=type):\n    pass\n",
        );
        // mutable default (line 1), eval (2), computed getattr (4),
        // metaclass (6); literal getattr (3) is explicit and exempt.
        assert_eq!(f.spooky_lines, [1, 2, 4, 6]);
    }

    #[test]
    fn an_empty_err_check_swallows_the_error_in_go() {
        // Go has no Catch node, so its entire error-discipline family
        // read zero — for the language whose central discipline is
        // error handling. Only the strictly empty body counts: a
        // comment is explicit silencing, and a non-nil comparison is
        // not an error check at all.
        let pack = crate::lang::Lang::Go.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.go"),
            "package p\n\nfunc Do() error {\n\tif err := run(); err != nil {\n\t}\n\tif err := again(); err != nil {\n\t\t// best-effort cache warm; failure is fine\n\t}\n\tif err := third(); err != nil {\n\t\treturn err\n\t}\n\tif x != 0 {\n\t}\n\treturn nil\n}\n",
        );
        assert_eq!(f.units[1].swallowed, 1);
    }

    #[test]
    fn computed_dynamic_imports_are_spooky_in_both_spellings() {
        let f = facts(
            "import importlib\n\ndef load(name):\n    a = importlib.import_module(f\"cmd_{name}\")\n    b = importlib.import_module(\"json\")\n    c = import_module(name)\n    d = getattr(load, f\"h_{name}\")\n    return a, b, c, d\n",
        );
        assert_eq!(
            f.spooky_lines,
            [4, 6, 7],
            "computed fires in both spellings and through f-strings; a literal module name is explicit"
        );
    }

    #[test]
    fn magic_numbers_require_a_name() {
        let f = facts(
            "TIMEOUT = 30\n\ndef f(x, retries=5):\n    y = x[3]\n    sleep(30)\n    return y * 2\n",
        );
        // TIMEOUT=30 named, retries=5 default, x[3] index, *2 trivial:
        // only sleep(30) is magic.
        assert_eq!(f.units[0].magic_numbers, 0, "module consts are named");
        assert_eq!(f.units[1].magic_numbers, 1);

        let pack = crate::lang::Lang::Rust.pack();
        let mut parser = pack.make_parser();
        let rf = extract(
            pack,
            &mut parser,
            Path::new("t.rs"),
            "const CAP: usize = 4096;\nfn f(ms: u64) -> u64 {\n    ms * 1000\n}\n",
        );
        assert_eq!(rf.units[0].magic_numbers, 0);
        assert_eq!(rf.units[1].magic_numbers, 1);
    }

    #[test]
    fn echo_comments_flag_restated_code_only() {
        let f = facts(
            "# get the user name\ndef get_user_name():\n    pass\n\n# workaround for race in uvloop\nsleep(1)\n\n# TODO get user name\ndef get_user_name2():\n    pass\n",
        );
        assert_eq!(
            f.echo_comments,
            [1],
            "echo flagged, why-comment and TODO exempt"
        );
    }

    #[test]
    fn trailing_echo_comment_attaches_to_its_line() {
        let f = facts("total = total + price  # add price to total\n");
        assert_eq!(f.echo_comments, [1]);
    }

    #[test]
    fn echo_check_ignores_closing_delimiters_and_long_bodies() {
        // A comment after a block's closing brace labels the block; it
        // was being judged against the whole construct.
        let pack = crate::lang::Lang::C.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.c"),
            "#ifdef HAVE_STREAM\nint f(void) { return 1; }\n#endif /* HAVE_STREAM */\n",
        );
        assert!(f.echo_comments.is_empty(), "closing label, not an echo");

        // A comment above a long function was matched against the entire
        // body, so sharing a word with any statement counted.
        let py = facts(
            "# summary of the retry policy applied below\ndef retry(op):\n    summary = 0\n    policy = 1\n    applied = 2\n    below = 3\n    return op(summary, policy, applied, below)\n",
        );
        assert!(
            py.echo_comments.is_empty(),
            "judged against the def line only"
        );
    }

    #[test]
    fn sphinx_and_banner_comments_are_documentation() {
        // `#:` is Sphinx attribute documentation; a banner judged against
        // the next banner compares prose to prose.
        let f = facts("#: the timeout in seconds\nTIMEOUT_IN_SECONDS = 30\n");
        assert!(f.echo_comments.is_empty(), "Sphinx doc marker");
    }

    #[test]
    fn imports_extracted_with_bound_names_per_language() {
        let cases: &[(Lang, &str, &str, &[&str])] = &[
            (
                Lang::Python,
                "t.py",
                "import os, sys as system\nfrom ..pkg import tool, helper as h\nfrom . import sibling\n",
                &["os", "sys", "..pkg", "."],
            ),
            (
                Lang::Rust,
                "t.rs",
                "use std::collections::HashMap;\nuse crate::facts::{extract, Sub as S};\nuse super::util::*;\n",
                &[
                    "std::collections::HashMap",
                    "crate::facts::extract",
                    "crate::facts::Sub",
                    "super::util::*",
                ],
            ),
            (
                Lang::TypeScript,
                "t.ts",
                "import def, { a, b as c } from \"./x\";\nimport * as ns from '../y';\n",
                &["./x", "../y"],
            ),
            (
                Lang::Go,
                "t.go",
                "package main\n\nimport (\n    \"fmt\"\n    myio \"io/ioutil\"\n    _ \"net/http/pprof\"\n)\n",
                &["fmt", "io/ioutil", "net/http/pprof"],
            ),
            (
                Lang::Zig,
                "t.zig",
                "const std = @import(\"std\");\nconst vsr = @import(\"vsr.zig\");\n",
                &["std", "vsr.zig"],
            ),
            (
                Lang::C,
                "t.c",
                "#include <stdio.h>\n#include \"util.h\"\n",
                &["<stdio.h>", "util.h"],
            ),
        ];
        for (lang, path, src, want) in cases {
            let pack = lang.pack();
            let mut parser = pack.make_parser();
            let f = extract(pack, &mut parser, Path::new(path), src);
            let got: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
            assert_eq!(&got, want, "{lang:?}");
        }
    }

    #[test]
    fn imported_roots_are_not_envied_objects() {
        // np is numpy, not a neighbor whose data this method covets.
        let f = facts(
            "import numpy as np\n\nclass M:\n    def calc(self, arr):\n        return np.dot(np.ones(3), np.zeros(3)) + np.sum(arr.a + arr.b + arr.c + arr.d)\n",
        );
        let m = &f.units[1];
        assert_eq!(&*m.envy_object, "arr", "module receiver exempted");
    }

    #[test]
    fn exports_are_public_units_and_types() {
        let f = facts(
            "class Widget:\n    pass\n\nclass _Hidden:\n    pass\n\ndef api():\n    pass\n\ndef _internal():\n    pass\n",
        );
        let names: Vec<&str> = f.exports.iter().map(|e| &**e).collect();
        assert_eq!(names, ["Widget", "api"]);
    }

    #[test]
    fn step_down_narrative_counts_call_direction() {
        // main calls helper (defined below): down. helper calls util
        // (defined above it? no — util is last): down. util calls main: up.
        let f = facts(
            "def main():\n    helper()\n    helper()\n\ndef helper():\n    util()\n\ndef util():\n    main()\n",
        );
        assert_eq!(f.step_refs, (3, 1), "three downward, one upward");

        // Module-level calls are excluded: a __main__ guard pointing up
        // is convention, not a narrative violation.
        let g = facts("def main():\n    pass\n\nmain()\n");
        assert_eq!(g.step_refs, (0, 0));
    }

    #[test]
    fn public_units_before_private_score() {
        let f =
            facts("def api_a():\n    pass\n\ndef _helper():\n    pass\n\ndef api_b():\n    pass\n");
        // Pairs: (api_a, _helper) public-first; (_helper, api_b) private-
        // first. Score 1 of 2.
        assert_eq!(f.pub_order, (1, 2));
    }

    #[test]
    fn expression_height_flags_one_liners() {
        let deep = facts("def f(x):\n    return g(h(k(x + 1)))\n    pass\n");
        let flat = facts("def f(x):\n    y = x + 1\n    return y\n");
        assert!(deep.units[1].max_expr_depth > flat.units[1].max_expr_depth);
    }

    /// (role, line, words) of every comment run, in source order.
    fn roles(lang: Lang, name: &str, src: &str) -> Vec<(&'static str, u32, u16)> {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(name), src);
        f.comments
            .iter()
            .map(|c| (c.role.name(), c.line, c.prose.words))
            .collect()
    }

    #[test]
    fn a_comment_takes_its_role_from_what_it_introduces() {
        let src = "\
//! What this file is for.

/// What the type is for.
pub struct S {
    /// What the field is for.
    pub n: usize,
}

/// What the function is for.
pub fn f(x: usize) -> usize {
    // How this step works.
    x + 1 // and what this line is
}
";
        assert_eq!(
            roles(Lang::Rust, "s.rs", src),
            [
                ("module", 1, 5),
                ("type", 3, 5),
                ("field", 5, 5),
                ("fn", 9, 5),
                ("inline", 11, 4),
                ("trailing", 12, 5),
            ]
        );
    }

    #[test]
    fn a_run_is_one_comment_however_many_nodes_it_takes() {
        // Four `///` nodes, one contract. Counted as one run because a
        // sentence — and a fenced example — spans lines.
        let src = "/// One sentence\n/// split over two lines.\n///\n/// ```\n/// let x = 1;\n/// ```\nfn f() {}\n";
        assert_eq!(roles(Lang::Rust, "s.rs", src), [("fn", 1, 6)]);
        // A blank line ends it: two thoughts, two runs, and only the
        // one touching the definition is its summary.
        let split = "use std::io;\n\n// first\n\n// second\nfn f() {}\n";
        assert_eq!(
            roles(Lang::Rust, "s.rs", split),
            [("inline", 3, 1), ("fn", 5, 1)]
        );
    }

    #[test]
    fn a_docstring_is_a_comment_the_grammar_spells_as_a_value() {
        let src = "\"\"\"Module purpose.\"\"\"\n\n\nclass C:\n    \"\"\"Type purpose.\"\"\"\n\n    def m(self):\n        \"\"\"Method purpose.\"\"\"\n";
        assert_eq!(
            roles(Lang::Python, "s.py", src),
            [("module", 1, 2), ("type", 5, 2), ("fn", 8, 2)]
        );
    }

    #[test]
    fn a_declaration_is_found_through_whatever_wraps_it() {
        // TypeScript hangs `export` outside the declaration and binds an
        // arrow through a declarator; Python gives its decorators their
        // own lines. Read literally, the next sibling of each of these
        // comments declares nothing and the summaries would be lost.
        let ts = "/** Exported. */\nexport function f() {}\n\n/** Bound. */\nexport const g = () => {};\n\n/** Shaped. */\nexport interface I { a: number }\n";
        assert_eq!(
            roles(Lang::TypeScript, "s.ts", ts),
            [("fn", 1, 1), ("fn", 4, 1), ("type", 7, 1)]
        );
        let py = "class C:\n    # What it does.\n    @property\n    def m(self):\n        pass\n";
        assert_eq!(roles(Lang::Python, "s.py", py), [("fn", 2, 3)]);
    }
}
