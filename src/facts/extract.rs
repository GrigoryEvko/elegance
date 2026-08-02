use std::path::Path;

use tree_sitter::{Node, Parser};

use super::{CloneSite, CtrlFact, FileFacts, UnitFacts};
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
        is_test_file: (pack.test_path)(&path.display().to_string()),
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
        test_refs: Vec::new(),
        switch_sigs: Vec::new(),
        record_shapes: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        mentioned: Vec::new(),
        step_refs: (0, 0),
        pub_order: (0, 0),
        interfaces: Vec::new(),
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
        envy: vec![std::collections::HashMap::new()],
        callees: vec![Vec::new()],
        self_names: vec!["".into()],
        tokens: vec![Vec::new()],
        import_roots: std::collections::HashSet::new(),
        mentions: std::collections::HashSet::new(),
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
            depth: 0,
        },
    );
    let Extractor {
        commentary,
        live,
        envy,
        callees,
        import_roots,
        mentions,
        tokens,
        ..
    } = ex;
    fingerprint_units(&mut facts, &tokens);
    facts.mentioned = mentions.into_iter().collect();
    facts.step_refs = step_refs(&facts.units, &callees);
    facts.pub_order = pub_order(&facts.units);
    resolve_envy(&mut facts.units, envy, &import_roots);
    facts.test_refs = resolve_locals(&mut facts.units, live, facts.is_test_file);
    facts.mass = root.map_or(0, |sub| sub.mass);
    facts.comment_lines = commentary.count_excluding(&blank) as u32;
    let unit_syms = public_unit_names(&facts.units);
    facts.exports.extend(unit_syms);
    facts
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
fn resolve_envy(
    units: &mut [UnitFacts],
    envy: Vec<std::collections::HashMap<Box<str>, u16>>,
    import_roots: &std::collections::HashSet<Box<str>>,
) {
    for (unit, foreign) in units.iter_mut().zip(envy) {
        for (name, count) in foreign {
            // Imported roots are modules in disguise (os, np, std) —
            // reaching into a module is not Feature Envy of an object.
            if import_roots.contains(&name) {
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
            is_module: false,
            is_method: false,
            is_public: false,
            params: Vec::new(),
            doc_lines: 0,
            max_vis_depth: 0,
            max_expr_depth: 0,
            magic_numbers: 0,
            max_live_span: 0,
            max_live_var: "".into(),
            demeter: 0,
            negations: 0,
            is_passthrough: false,
            self_accesses: 0,
            envy_count: 0,
            envy_object: "".into(),
            swallowed: 0,
            broad_catch: 0,
            lost_context: 0,
            unwraps: 0,
            casts: 0,
            is_async: false,
            blocking_calls: 0,
            fingerprints: Vec::new(),
            max_loop_depth: 0,
            allocs_in_loop: 0,
            conditional_hooks: 0,
            unmanaged: 0,
            dropped_tasks: 0,
            wildcard_matches: 0,
            is_test: false,
            named_test: false,
            assert_calls: 0,
            vacuous_asserts: 0,
            returns: "".into(),
            return_arity: 0,
            repurposed: 0,
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
    /// Every distinct identifier the file mentions, for the dead-export
    /// join. Deduplicated here so the aggregate counts FILES per name.
    mentions: std::collections::HashSet<Box<str>>,
    facts: &'a mut FileFacts,
}

impl Extractor<'_> {
    fn sem_of(&self, node: Node) -> Sem {
        self.pack.sem_of(node, self.src)
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
                None
            }
            Sem::None if (self.pack.is_doc)(node) => {
                self.mark_commentary(node);
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
            // Catch nodes and error-as-value If checks are one family.
            Sem::If | Sem::Catch => self.record_error_handling(node, sem, ctx.unit),
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
            Sem::StrLit => self.record_secret(node, ctx.unit),
            _ => {}
        }
        self.record_ctrl(node, sem, ctx);
        // A TypeDef can also be spooky (Python metaclasses); the arm above
        // returns before this, so it is checked here for both.
        if matches!(sem, Sem::Call | Sem::TypeDef) && (self.pack.spooky)(node, sem, self.src) {
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
        let Some(mut keys) = (self.pack.record_keys)(node, self.src) else {
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
        if sem == Sem::If {
            let swallowed = (self.pack.swallows_error)(node, self.src);
            self.facts.units[unit_idx].swallowed += swallowed as u16;
            return;
        }
        let lost = (self.pack.loses_context)(node, self.src);
        let unit = &mut self.facts.units[unit_idx];
        unit.lost_context += lost as u16;
        match (self.pack.catch_sin)(node, self.src) {
            Some(crate::lang::CatchSin::Swallowed) => unit.swallowed += 1,
            Some(crate::lang::CatchSin::Broad) => unit.broad_catch += 1,
            None => {}
        }
    }

    fn record_call(&mut self, node: Node, ctx: Ctx) {
        let unit_idx = ctx.unit;
        if let Some(name) = self.callee_simple_name(node).map(Box::<str>::from) {
            self.callees[unit_idx].push(name);
        }
        let unit = &mut self.facts.units[unit_idx];
        if !unit.self_recursive && (self.pack.is_self_call)(node, self.src, &unit.name) {
            unit.self_recursive = true;
        }
        if (self.pack.panicky)(node, self.src) {
            unit.unwraps += 1;
        }
        if (self.pack.asserty)(node, self.src) {
            unit.assert_calls += 1;
        }
        if unit.is_async && self.parks_the_thread(node) {
            self.facts.units[unit_idx].blocking_calls += 1;
        }
        if (self.pack.asserty)(node, self.src) && self.asserts_a_literal(node) {
            self.facts.units[unit_idx].vacuous_asserts += 1;
        }
        self.record_lifetime(node, unit_idx);
        self.record_placement(node, ctx);
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
            ctx.branched && holds_hooks(&unit.name) && (self.pack.is_hook)(node, self.src);
        let copy_per_iteration = ctx.loops > 0 && self.allocates_a_copy(node);
        let unit = &mut self.facts.units[ctx.unit];
        unit.conditional_hooks += stray_hook as u16;
        unit.allocs_in_loop += copy_per_iteration as u16;
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

    /// Two things acquired without anything arranging their release: a
    /// resource opened outside a scope guard, and a spawned task whose
    /// handle is thrown away.
    fn record_lifetime(&mut self, node: Node, unit_idx: usize) {
        let unguarded = (self.pack.unguarded_resource)(node, self.src);
        let dropped = self.callee_trailing_name(node) == Some("spawn") && discards_its_result(node);
        let unit = &mut self.facts.units[unit_idx];
        unit.unmanaged += unguarded as u16;
        unit.dropped_tasks += dropped as u16;
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
    fn asserts_a_literal(&self, call: Node) -> bool {
        let args = call
            .child_by_field_name("arguments")
            .or_else(|| call.child_by_field_name("argument"))
            // Rust macros carry their arguments in a token tree, not a
            // field: `assert!(true)` is a macro_invocation(token_tree).
            .or_else(|| {
                let mut cursor = call.walk();
                call.named_children(&mut cursor)
                    .find(|c| matches!(c.kind(), "token_tree" | "arguments"))
            });
        let subjects: Vec<Node> = match args {
            Some(args) => {
                let mut cursor = args.walk();
                args.named_children(&mut cursor).collect()
            }
            // Zig nests arguments DIRECTLY under the call node; the
            // function field is the only non-argument child.
            None => {
                let func = call.child_by_field_name("function");
                let mut cursor = call.walk();
                call.named_children(&mut cursor)
                    .filter(|n| func.is_none_or(|f| f.id() != n.id()))
                    .collect()
            }
        };
        let [only] = subjects[..] else { return false };
        self.is_literal(only)
    }

    fn record_imports(&mut self, node: Node) {
        for edge in (self.pack.imports)(node, self.src) {
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
        let found = (self.pack.interfaces)(node, self.src);
        self.facts.interfaces.extend(found);
    }

    fn record_type_export(&mut self, node: Node) {
        if (self.pack.is_public)(node, self.src)
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
    fn enclosing_scope_is_class(&self, node: Node) -> bool {
        let mut anc = node.parent();
        while let Some(a) = anc {
            match self.sem_of(a) {
                Sem::TypeDef => return true,
                Sem::FnDef | Sem::Lambda => return false,
                _ => anc = a.parent(),
            }
        }
        false
    }

    /// Base name of a scope-forming node: the pack's name_node hook wins,
    /// then the `name` field, then the parent's binding site (promoted
    /// lambdas), then a `type` field (Rust impl blocks, generics stripped).
    fn scope_name(&self, node: Node) -> Option<&str> {
        let named = (self.pack.name_node)(node)
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

    fn open_unit(&mut self, node: Node) -> usize {
        let recv = self.receiver(node);
        let (name, qualname) = self.unit_names(node, recv.as_ref());
        let mut unit = UnitFacts {
            name,
            qualname,
            line: node.start_position().row as u32 + 1,
            lines: line_span(node),
            is_method: self.enclosing_scope_is_class(node) || recv.is_some(),
            ..UnitFacts::blank()
        };
        // What this unit calls its own object: a receiver consumed from the
        // parameter list, else one the grammar declares separately. Empty
        // for free functions.
        let self_name = self
            .take_params(node, &mut unit)
            .or_else(|| recv.map(|r| r.name))
            .unwrap_or_else(|| "".into());
        unit.is_public = (self.pack.is_public)(node, self.src);
        unit.doc_lines = (self.pack.unit_docs)(node, self.src);
        unit.is_passthrough = self.is_passthrough(node, &unit.params);
        unit.is_async = (self.pack.is_async)(node, self.src);
        unit.named_test = (self.pack.declares_test)(node, self.src)
            || (self.facts.is_test_file && (self.pack.names_test)(node, self.src));
        unit.is_test =
            self.facts.is_test_file || unit.named_test || (self.pack.is_test_code)(node, self.src);
        unit.returns = node
            .child_by_field_name(self.pack.return_type_field)
            .and_then(|r| r.utf8_text(self.src).ok())
            .map(|t| t.trim_start_matches(':').trim())
            .unwrap_or("")
            .into();
        unit.return_arity = (self.pack.return_arity)(node, self.src);
        self.facts.units.push(unit);
        self.live.push(LiveMap::new());
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
        let name: Box<str> = self.scope_name(node).unwrap_or("?").into();
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
            let mut cursor = params.walk();
            params.named_children(&mut cursor).collect()
        };
        let mut receiver = None;
        for (i, p) in list.into_iter().enumerate() {
            let Some(info) = (self.pack.param_info)(p, self.src) else {
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
        let Some(body) = node.child_by_field_name("body") else {
            return false;
        };
        // Expression-bodied lambdas forward directly; blocks must hold
        // exactly one statement (a docstring does not count as substance).
        let mut stmt = body;
        if self.pack.table_sem(body) != Sem::Call {
            let mut cursor = body.walk();
            let stmts: Vec<Node> = body
                .named_children(&mut cursor)
                .filter(|n| !(self.pack.is_doc)(*n) && self.sem_of(*n) != Sem::Comment)
                .collect();
            let [only] = stmts[..] else { return false };
            stmt = only;
        }
        // Unwrap `return expr` / expression statements to the call itself.
        while matches!(stmt.kind(), "return_statement" | "expression_statement") {
            match stmt.named_child(0) {
                Some(inner) => stmt = inner,
                None => return false,
            }
        }
        if self.pack.table_sem(stmt) != Sem::Call {
            return false;
        }
        let Some(args) = stmt.child_by_field_name("arguments") else {
            return false;
        };
        let mut cursor = args.walk();
        let arg_names: Option<Vec<&str>> = args
            .named_children(&mut cursor)
            .map(|a| {
                (self.pack.table_sem(a) == Sem::Ident)
                    .then(|| a.utf8_text(self.src).ok())
                    .flatten()
            })
            .collect();
        let Some(arg_names) = arg_names else {
            return false;
        };
        // Forwarded args must be this unit's own parameters, as a prefix in
        // declaration order (defaults may be dropped). A zero-argument
        // callee matches vacuously, which made every one-line accessor
        // (`self.bits.len()`) a Middle Man — half of all Rust firings.
        // Known cost: a genuine zero-argument forwarder now goes unseen.
        !arg_names.is_empty()
            && arg_names.len() <= params.len()
            && arg_names.iter().zip(params).all(|(a, p)| *a == &*p.name)
    }

    /// Does this call park the thread rather than yield? Node's `*Sync`
    /// family is named after the problem. `sleep` needs its QUALIFIER
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
        let segs: Vec<&str> = path.split(['.', ':']).filter(|s| !s.is_empty()).collect();
        let Some(last) = segs.last() else {
            return false;
        };
        if last.len() > 4 && last.ends_with("Sync") {
            return true;
        }
        match segs.as_slice() {
            // The standard thread module, however deeply qualified.
            [.., "thread", "sleep"] => true,
            // Python's `time.sleep` exactly: `tokio::time::sleep` is
            // three segments and stays exempt.
            ["time", "sleep"] => true,
            _ => false,
        }
    }

    /// Whole text of a call's target: `time.sleep`, `std::thread::sleep`.
    fn callee_text(&self, call: Node) -> Option<&str> {
        let f = call
            .child_by_field_name("function")
            .or_else(|| call.child_by_field_name("macro"))?;
        f.utf8_text(self.src).ok()
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
        "api_token",
        "auth_token",
        "access_token",
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
            if self.pack.assign_kinds.contains(&a.kind())
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
        if trivial {
            return false;
        }
        let screaming = |text: &str| {
            text.chars().any(|c| c.is_ascii_alphabetic())
                && !text.chars().any(|c| c.is_ascii_lowercase())
        };
        let mut anc = node.parent();
        for _ in 0..8 {
            let Some(a) = anc else { break };
            let kind = a.kind();
            if self.pack.magic_exempt.contains(&kind) {
                return false;
            }
            if self.pack.assign_kinds.contains(&kind) {
                let bound = a
                    .child_by_field_name("left")
                    .or_else(|| a.child_by_field_name("name"))
                    .and_then(|n| n.utf8_text(self.src).ok());
                if bound.is_some_and(screaming) {
                    return false;
                }
            }
            anc = a.parent();
        }
        true
    }

    /// Law of Demeter (Lieberherr 1989): reaching through >=3 consecutive
    /// data links couples the reader to neighbors' internal structure.
    /// Fluent chains break naturally — a call ends the descent — and a
    /// self/this base forgives its first link.
    fn check_demeter(&mut self, node: Node, unit: usize) {
        let Some((attr_kind, object_field)) = self.pack.attr() else {
            return;
        };
        // Only chain roots: a parent of the same kind means we are one of
        // its links and will be counted from the top.
        if node.kind_id() != attr_kind || node.parent().is_some_and(|p| p.kind_id() == attr_kind) {
            return;
        }
        let mut links = 0u16;
        let mut base = node;
        while base.kind_id() == attr_kind {
            links += 1;
            match base.child_by_field_name(object_field) {
                Some(inner) => base = inner,
                None => break,
            }
        }
        // The same chain root feeds Feature Envy: one chain = one access
        // to its base receiver (methods only).
        let base_name = (self.pack.table_sem(base) == Sem::Ident)
            .then(|| base.utf8_text(self.src).ok())
            .flatten();
        let selfish_base = base_name
            .is_some_and(|n| matches!(n, "self" | "cls" | "this") || n == &*self.self_names[unit]);
        if self.facts.units[unit].is_method {
            if selfish_base {
                self.facts.units[unit].self_accesses += 1;
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
        if links >= 3 {
            self.facts.units[unit].demeter += 1;
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
                if self.pack.table_sem(child) == Sem::CaseArm {
                    labels.push(arm_label(child, self.src));
                } else if depth < 1 && self.pack.table_sem(child) != Sem::Match {
                    stack.push((child, depth + 1));
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

    /// K&P: "say what you mean" — flag only provable inversions: nested
    /// nots, negation of != comparisons, negated negative-polarity names,
    /// and De Morgan candidates (not over and/or).
    fn check_negation(&mut self, node: Node, unit: usize) {
        let Some(mut operand) = (self.pack.negation_operand)(node, self.src) else {
            return;
        };
        while operand.kind() == "parenthesized_expression" {
            match operand.named_child(0) {
                Some(inner) => operand = inner,
                None => break,
            }
        }
        // `!!x` / `not not x` is a coercion to bool — a cast idiom, not
        // inverted logic — and it was 78% of this metric's TypeScript
        // firings. De Morgan candidates below are the real finding.
        if (self.pack.negation_operand)(operand, self.src).is_some() {
            return;
        }
        let demorgan = self.sem_of(operand) == Sem::BoolOp;
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
        if demorgan || negative_name || neq {
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
    fn record_defs(&mut self, node: Node, field: &str, unit: usize) {
        let Some(target) = node.child_by_field_name(field) else {
            return;
        };
        let row = node.start_position().row as u32 + 1;
        let mut stack = vec![target];
        while let Some(n) = stack.pop() {
            if self.pack.table_sem(n) == Sem::Ident {
                if let Ok(name) = n.utf8_text(self.src)
                    && !name.starts_with('_')
                {
                    let entry = self.live[unit].entry(name.into()).or_insert((None, row));
                    entry.0.get_or_insert(row);
                    entry.1 = entry.1.max(row);
                }
                continue;
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                stack.push(child);
            }
        }
    }

    fn mark_commentary(&mut self, node: Node) {
        for row in node.start_position().row..=node.end_position().row {
            self.commentary.set(row);
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
        let f = call
            .child_by_field_name("function")
            .or_else(|| call.child_by_field_name("macro"))?;
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

/// Does this literal carry the entropy of a real key? A placeholder, an
/// interpolation, or a plain English word does not.
fn looks_like_a_key(value: &str) -> bool {
    // Structure separators mean this NAMES a secret rather than being
    // one: an OAuth URN (colons), a dotted setting id, an interpolation.
    // A dot also separates a JWT's segments, so a hardcoded JWT outside
    // a test goes unseen; that is the price of a gate that never cries
    // wolf, and JWTs in source are overwhelmingly test fixtures anyway.
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

/// The single bare identifier a reassignment writes, if that is what
/// it writes. Go wraps the target in an `expression_list`, so one
/// layer of single-child wrapper is unwrapped; two targets (a swap, a
/// multi-assign) or a member/index target (`self.x`, `a[i]`) answer
/// None — those mutate state THROUGH a name rather than rebinding it.
fn single_reassign_target<'t>(pack: &Pack, node: Node<'t>, field: &str) -> Option<Node<'t>> {
    let mut target = node.child_by_field_name(field)?;
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

/// A definition's parameter list, wherever this grammar keeps it.
fn param_list<'t>(node: Node<'t>) -> Option<Node<'t>> {
    node.child_by_field_name("parameters")
        .or_else(|| node.child_by_field_name("parameter"))
        // Some grammars (Zig) leave the parameter list unfielded.
        .or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|c| c.kind() == "parameters")
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

    fn facts(source: &str) -> FileFacts {
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new("test.py"), source)
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
        // != inversion, De Morgan, negated negative name; `not found` is a
        // clean single negation of a positive name.
        assert_eq!(f.units[1].negations, 3);
    }

    #[test]
    fn double_negation_is_coercion_not_inverted_logic() {
        // `!!x` casts to bool; it was 78% of this metric's TS firings.
        let pack = crate::lang::Lang::TypeScript.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.ts"),
            "function f(x: unknown, ok: boolean, other: boolean) {\n    const a = !!x;\n    if (!(ok && other)) { return 1; }\n    return a;\n}\n",
        );
        assert_eq!(f.units[1].negations, 1, "De Morgan only, coercion exempt");
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
}
