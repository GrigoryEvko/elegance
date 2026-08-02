//! Language packs: everything language-specific lives behind this boundary.
//! A pack is a dense `kind_id -> Sem` table plus a few small hooks for the
//! irreducibly syntactic bits (context refinement, parameter shapes,
//! self-call detection). Packs classify syntax; only the core assigns meaning.

mod c;
mod go;
mod js;
mod ocaml;
mod python;
mod rust;
mod shell;
mod typescript;
mod zig;

use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::{Language, Node, Parser};

use crate::sem::Sem;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Python,
    Rust,
    TypeScript,
    Tsx,
    Go,
    JavaScript,
    Zig,
    C,
    OCaml,
    Shell,
}

pub const LANGS: [Lang; 10] = [
    Lang::Python,
    Lang::Rust,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::Go,
    Lang::JavaScript,
    Lang::Zig,
    Lang::C,
    Lang::OCaml,
    Lang::Shell,
];

impl Lang {
    pub fn from_path(path: &Path) -> Option<Lang> {
        match path.extension()?.to_str()? {
            "py" => Some(Lang::Python),
            "rs" => Some(Lang::Rust),
            "ts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            "go" => Some(Lang::Go),
            "js" | "mjs" | "cjs" | "jsx" => Some(Lang::JavaScript),
            "zig" => Some(Lang::Zig),
            "c" | "h" => Some(Lang::C),
            "ml" | "mli" => Some(Lang::OCaml),
            // Extension only: an extensionless script with a shebang is
            // real, but this is a pure path predicate the walk calls on
            // every file, and sniffing would change what a walk costs.
            "sh" | "bash" => Some(Lang::Shell),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "py",
            Lang::Rust => "rs",
            Lang::TypeScript => "ts",
            Lang::Tsx => "tsx",
            Lang::Go => "go",
            Lang::JavaScript => "js",
            Lang::Zig => "zig",
            Lang::C => "c",
            Lang::OCaml => "ml",
            Lang::Shell => "sh",
        }
    }

    pub fn pack(self) -> &'static Pack {
        static PACKS: [OnceLock<Pack>; LANGS.len()] = [const { OnceLock::new() }; LANGS.len()];
        PACKS[self as usize].get_or_init(|| match self {
            Lang::Python => python::pack(),
            Lang::Rust => rust::pack(),
            Lang::TypeScript => typescript::pack(typescript::Dialect::Ts),
            Lang::Tsx => typescript::pack(typescript::Dialect::Tsx),
            Lang::Go => go::pack(),
            Lang::JavaScript => js::pack(),
            Lang::Zig => zig::pack(),
            Lang::C => c::pack(),
            Lang::OCaml => ocaml::pack(),
            Lang::Shell => shell::pack(),
        })
    }
}

/// How a catch clause mishandles errors. Zen of Python: "Errors should
/// never pass silently. Unless explicitly silenced."
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatchSin {
    /// Handler body is only pass/ellipsis/empty — the error vanishes.
    Swallowed,
    /// Catches Exception/BaseException or binds nothing — too broad.
    Broad,
}

/// One import edge as written in source: where it points and which
/// local names it binds (bound names are modules in disguise — they
/// must not read as envied objects or unnamed magic).
pub struct ImportInfo {
    /// Normalized target: `a.b`, `..pkg.x`, `./util`, `crate::x::y`,
    /// `<stdio.h>` (angle brackets preserved: definitionally external).
    pub target: Box<str>,
    /// Local names this import introduces.
    pub names: Vec<Box<str>>,
}

/// The field names of an anonymous record, when a node is one.
pub type RecordKeys = fn(Node, &[u8]) -> Option<Vec<Box<str>>>;

/// What a parameter contributes to interface metrics.
#[derive(Clone, Default)]
pub struct ParamInfo {
    /// Binding name (pattern text for destructuring params).
    pub name: Box<str>,
    /// Boolean-typed or boolean-defaulted — a flag parameter candidate.
    pub boolish: bool,
    /// Receiver (`self`/`cls`) — excluded from parameter counts on methods.
    pub selfish: bool,
    /// Receiver taken mutably (`&mut self`).
    pub mut_receiver: bool,
    /// Mutable default value (Python `def f(x=[])`) — shared across calls.
    pub mutable_default: bool,
    /// `**kwargs`-style splat — an interface that reveals nothing.
    pub kw_splat: bool,
    /// Optional at the call site (default value, `?`, splat): adding
    /// one to a signature breaks no caller.
    pub optional: bool,
    /// Carries a declared type. In a gradually-typed language an untyped
    /// parameter is a promise the compiler cannot keep for you.
    pub typed: bool,
    /// The declared type is an escape hatch (`Any`, `any`, `interface{}`,
    /// `void *`, `anytype`): annotated, but asserting nothing.
    pub loose: bool,
    /// Declared type as written, empty when absent. Adjacent parameters
    /// of the SAME type are silently swappable at every call site.
    pub type_name: Box<str>,
}

pub struct Pack {
    pub lang: Lang,
    pub ts: Language,
    /// Raw grammar-name tables, retained for drift validation: a
    /// tree-sitter upgrade must never silently zero a metric (the Zig
    /// @import lesson). Runtime stays lenient; tests stay merciless.
    ///
    /// A LIST of tables, because dialects of one grammar genuinely differ:
    /// `<X>expr` is a cast in .ts and ambiguous with JSX in .tsx, so the
    /// TSX grammar has no `type_assertion` node to map.
    kind_names: &'static [&'static [(&'static str, Sem)]],
    def_site_names: &'static [(&'static str, &'static str)],
    reassign_names: &'static [(&'static str, &'static str)],
    attr_name: Option<(&'static str, &'static str)>,
    sems: Box<[Sem]>,
    /// Local-binding sites resolved to kind ids: (kind, field holding the
    /// bound pattern). Fuels live-span tracking.
    def_sites: Box<[(u16, &'static str)]>,
    /// Plain-reassignment sites: (kind, field holding the target).
    /// Reassignment only, never fresh bindings — a Rust `let` shadow or
    /// a Go `:=` is a NEW binding, and judging those without scopes
    /// would flag sibling blocks. Fuels the repurposing check.
    reassigns: Box<[(u16, &'static str)]>,
    /// Member-access node: (kind id, receiver field). Fuels Demeter chains.
    attr: Option<(u16, &'static str)>,
    /// Separator for scope-qualified unit names (`.` or `::`).
    pub scope_sep: &'static str,
    /// Field holding a definition's declared return type (`return_type`,
    /// Go `result`, Zig `type`) — the name-contract metric's evidence.
    pub return_type_field: &'static str,
    /// Field holding a boolean operator's operator token (for sequence dedup).
    pub bool_op_field: &'static str,
    /// Does this language have type syntax at all? JavaScript does not,
    /// so "untyped" there is a fact about the language, not the code.
    pub types_declared: bool,
    /// Context-dependent classification the kind table cannot express:
    /// `else if` chains, operator disambiguation, promoting named lambdas.
    pub refine: fn(Node, &[u8], Sem) -> Sem,
    /// Node holding a unit's name when no `name` field exists (Zig `test`
    /// labels, C declarator chains). Consulted before the generic chain.
    pub name_node: for<'t> fn(Node<'t>) -> Option<Node<'t>>,
    /// Import edges of one Import node (a `use` tree or `from` list may
    /// carry several).
    pub imports: fn(Node, &[u8]) -> Vec<ImportInfo>,
    /// Classify one node of a parameter list.
    pub param_info: fn(Node, &[u8]) -> Option<ParamInfo>,
    /// Does this call target the named enclosing unit (direct recursion)?
    pub is_self_call: fn(Node, &[u8], &str) -> bool,
    /// Is this statement a documentation node (e.g. Python docstring)?
    pub is_doc: fn(Node) -> bool,
    /// Comment prefixes this ecosystem reads as documentation rather than
    /// commentary (Sphinx `#:`, section `##`). The universal `///`,
    /// `//!` and `/**` are handled centrally.
    pub doc_markers: &'static [&'static str],
    /// Is this definition part of the public surface? Conservative: when
    /// unsure, say no — coverage findings must be precise.
    pub is_public: fn(Node, &[u8]) -> bool,
    /// Contract-documentation lines attached to this definition (docstring,
    /// `///` run, JSDoc block). Interface docs, not implementation notes.
    pub unit_docs: fn(Node, &[u8]) -> u32,
    /// Constructs where the text stops predicting the run (Dijkstra's gap):
    /// eval/exec, computed attribute access, metaclasses, transmute.
    /// Consulted for Call and TypeDef nodes.
    pub spooky: fn(Node, Sem, &[u8]) -> bool,
    /// If this node is a logical NOT, its operand.
    pub negation_operand: for<'t> fn(Node<'t>, &[u8]) -> Option<Node<'t>>,
    /// Error-handling sin of a Catch node, if any.
    pub catch_sin: fn(Node, &[u8]) -> Option<CatchSin>,
    /// An error-check whose handler is EMPTY (`if err != nil { }`).
    /// Languages where errors are values have no Catch node to judge,
    /// and without this their whole error-discipline family reads
    /// zero. Consulted on If nodes; counts into `swallowed`.
    pub swallows_error: fn(Node, &[u8]) -> bool,
    /// Does this handler bind the error, raise a NEW one, and never
    /// mention the original? A separate hook rather than another
    /// `CatchSin`, so the already-calibrated swallowed and broad-catch
    /// numbers do not shift under it.
    pub loses_context: fn(Node, &[u8]) -> bool,
    /// Call that panics instead of returning an error (`unwrap`/`expect`).
    pub panicky: fn(Node, &[u8]) -> bool,
    /// The keys of an anonymous record literal, if this node is one.
    /// Rust and Go build records through declared types, so their packs
    /// answer None and the metric stays silent rather than wrong.
    pub record_keys: RecordKeys,
    /// Does this call acquire a resource with nothing arranging its
    /// release? Only languages where an explicit scope guard is THE
    /// idiom answer yes: Rust drops on scope exit and needs no guard,
    /// and a language without the idiom has no absence to detect.
    pub unguarded_resource: fn(Node, &[u8]) -> bool,
    /// Is this definition async? A blocking call inside one stalls the
    /// whole executor, not just this task.
    pub is_async: fn(Node, &[u8]) -> bool,
    /// Does this definition DECLARE a test by context-free evidence —
    /// an attribute (`#[test]`), structure (`test('...', fn)`), or a
    /// dedicated kind (Zig `test` blocks)? Judged by the test-quality
    /// metrics wherever it appears.
    pub declares_test: fn(Node, &[u8]) -> bool,
    /// Does this definition's NAME follow the ecosystem's test naming
    /// convention (`test_*`, `TestXxx`)? Honored only inside test
    /// files: a production `test_connection` health check is neither a
    /// test to judge nor test code to pardon.
    pub names_test: fn(Node, &[u8]) -> bool,
    /// Test code by construction beyond the above (Rust `#[cfg(test)]`
    /// modules): exempt from production metrics, never judged as a test.
    pub is_test_code: fn(Node, &[u8]) -> bool,
    /// Is this file path a test file by ecosystem convention?
    pub test_path: fn(&str) -> bool,
    /// Call/macro that asserts (assert_eq!, self.assertEqual, expect).
    pub asserty: fn(Node, &[u8]) -> bool,
    /// A React-style hook call, whose identity is its CALL ORDER
    /// rather than its name. Reached through a branch, it renumbers
    /// every hook after it the first time the condition flips.
    pub is_hook: fn(Node, &[u8]) -> bool,
    /// How many values this definition makes its callers destructure:
    /// a Go result list's width, a Rust or TS tuple return type's
    /// width, the widest tuple a Python `return` ships. Languages
    /// where a compound result is already a single value (a JS array,
    /// an OCaml tuple, a Zig struct) answer 0 — there is nothing to
    /// destructure that a name would not fix.
    pub return_arity: fn(Node, &[u8]) -> u16,
    /// Declared method bundles under this TypeDef node — a Go
    /// `interface`, a Rust `trait`, a TS `interface` — each with how
    /// many methods it demands. A Vec because one Go `type (...)`
    /// block declares several. Languages whose interfaces are
    /// conventions rather than declarations answer nothing.
    pub interfaces: fn(Node, &[u8]) -> Vec<crate::facts::InterfaceFact>,
    /// Ancestor kinds that legitimize a numeric literal: const items,
    /// parameter defaults, indexing, types, patterns.
    pub magic_exempt: &'static [&'static str],
    /// Assignment-ish kinds whose SCREAMING_CASE binding names a constant.
    pub assign_kinds: &'static [&'static str],
}

impl Pack {
    /// (kind id, receiver field) of member-access nodes, if declared.
    pub fn attr(&self) -> Option<(u16, &'static str)> {
        self.attr
    }

    /// Field holding the bound pattern when this node defines locals.
    pub fn def_field(&self, kind_id: u16) -> Option<&'static str> {
        self.def_sites
            .iter()
            .find(|(k, _)| *k == kind_id)
            .map(|(_, f)| *f)
    }

    /// Does a literal under this kind sit in a position that already
    /// names it — a const item, a parameter default, an index, a type,
    /// a pattern?
    pub fn exempts_literal(&self, kind: &str) -> bool {
        self.magic_exempt.contains(&kind)
    }

    /// Is this kind a binding site whose name could name a literal?
    pub fn binds_value(&self, kind: &str) -> bool {
        self.assign_kinds.contains(&kind)
    }

    /// Field holding a plain reassignment's target, if this kind is one.
    pub fn reassign_field(&self, kind_id: u16) -> Option<&'static str> {
        self.reassigns
            .iter()
            .find(|(k, _)| *k == kind_id)
            .map(|(_, f)| *f)
    }

    /// Table classification only; most callers want [`Pack::sem_of`].
    pub fn table_sem(&self, node: Node) -> Sem {
        self.sems
            .get(node.kind_id() as usize)
            .copied()
            .unwrap_or(Sem::None)
    }

    pub fn sem_of(&self, node: Node, src: &[u8]) -> Sem {
        (self.refine)(node, src, self.table_sem(node))
    }

    pub fn make_parser(&self) -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&self.ts)
            .expect("grammar/runtime version mismatch");
        parser
    }

    /// Every declared grammar name that does NOT resolve in the compiled
    /// grammar — kinds and fields alike. Healthy packs return empty.
    pub fn unresolved(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let kind = |bad: &mut Vec<String>, name: &str| {
            if self.ts.id_for_node_kind(name, true) == 0 {
                bad.push(format!("kind {name:?}"));
            }
        };
        let field = |bad: &mut Vec<String>, name: &str| {
            if !name.is_empty() && self.ts.field_id_for_name(name).is_none() {
                bad.push(format!("field {name:?}"));
            }
        };
        for (name, _) in self.kind_names.iter().copied().flatten() {
            kind(&mut bad, name);
        }
        for (k, f) in self.def_site_names {
            kind(&mut bad, k);
            field(&mut bad, f);
        }
        for (k, f) in self.reassign_names {
            kind(&mut bad, k);
            field(&mut bad, f);
        }
        if let Some((k, f)) = self.attr_name {
            kind(&mut bad, k);
            field(&mut bad, f);
        }
        for k in self.magic_exempt {
            kind(&mut bad, k);
        }
        for k in self.assign_kinds {
            kind(&mut bad, k);
        }
        field(&mut bad, self.return_type_field);
        field(&mut bad, self.bool_op_field);
        bad
    }
}

/// Resolve one (kind name, field) pair to a kind id.
fn attr_site(
    ts: &Language,
    kind: &'static str,
    field: &'static str,
) -> Option<(u16, &'static str)> {
    let id = ts.id_for_node_kind(kind, true);
    (id != 0).then_some((id, field))
}

/// Resolve (kind name, field) pairs to kind ids; unknown kinds drop out.
fn def_table(ts: &Language, sites: &[(&'static str, &'static str)]) -> Box<[(u16, &'static str)]> {
    sites
        .iter()
        .filter_map(|&(kind, field)| {
            let id = ts.id_for_node_kind(kind, true);
            (id != 0).then_some((id, field))
        })
        .collect()
}

/// Build the dense kind table from (kind name, Sem) pairs. Unknown names
/// are skipped so a grammar bump degrades instead of crashing — but they
/// are never harmless (an unmapped kind silently zeroes a metric), so
/// debug builds complain and [`Pack::unresolved`] fails CI.
fn sem_table(ts: &Language, kinds: &[&[(&str, Sem)]]) -> Box<[Sem]> {
    let mut sems = vec![Sem::None; ts.node_kind_count()];
    for &(name, sem) in kinds.iter().copied().flatten() {
        let id = ts.id_for_node_kind(name, true);
        if id != 0 {
            sems[id as usize] = sem;
        } else if cfg!(debug_assertions) {
            eprintln!("elegance: grammar has no node kind {name:?} — pack drift");
        }
    }
    sems.into_boxed_slice()
}

/// Every language that has async spells it the same way, at the front of
/// the declaration. Checking the head of the text rather than a field
/// keeps this working across `async fn`, `pub async fn`, `async def` and
/// `export async function` without four different node shapes.
fn declared_async(node: Node, src: &[u8]) -> bool {
    let Ok(text) = node.utf8_text(src) else {
        return false;
    };
    // Only the head matters, and it must be cut on a char boundary: a
    // multi-byte character straddling the cut panics, and one `'2✓'` in
    // a vscode string took the whole TypeScript run down.
    let head: String = text.chars().take(40).collect();
    head.split(|c: char| !c.is_alphanumeric() && c != '_')
        .take(4)
        .any(|word| word == "async")
}

/// A handler loses its context when it binds the error, throws a new
/// one, and never mentions the original: the stack that explains WHY is
/// gone, and the report says only that something failed at the top.
///
/// Shared across the languages that have exception chaining. `raiser`
/// names the kind that re-raises; the binding is the caught error's
/// name; a re-raise whose text mentions that name has kept the cause,
/// whether by `from e`, `cause: e`, or interpolation.
fn rethrows_without_cause(body: Node, raiser: &str, bound: &str, src: &[u8]) -> bool {
    let mut stack = vec![body];
    let mut rethrows = false;
    while let Some(n) = stack.pop() {
        if n.kind() == raiser {
            let text = n.utf8_text(src).unwrap_or("");
            if mentions(text, bound) {
                return false;
            }
            rethrows = true;
            continue;
        }
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    rethrows
}

/// Whole-word containment, so binding `e` is not found inside `err`.
fn mentions(text: &str, name: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|token| token == name)
}

/// Does an identifier read as an assertion helper? The convention across
/// ecosystems is an `assert` prefix or suffix, case-insensitively:
/// serverAssert, redisassert, ASSERT, assertEqual, assertEqualStrings.
/// Slices via `get` so a non-ASCII identifier cannot split a char.
fn assertish(name: &str) -> bool {
    const PAT: &str = "assert";
    name.get(..PAT.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PAT))
        || name
            .len()
            .checked_sub(PAT.len())
            .and_then(|at| name.get(at..))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(PAT))
}

/// `expect` or `expectSomething` — a test helper. The capital matters:
/// without it `expected_value()` in production code reads as an
/// assertion.
fn expectish(name: &str) -> bool {
    const PAT: &str = "expect";
    name == PAT
        || name
            .strip_prefix(PAT)
            .is_some_and(|rest| rest.starts_with(char::is_uppercase))
}

/// Does this declared type text name one of the language's escape
/// hatches? Split into identifier tokens so a hatch counts wherever it
/// appears — `dict[str, Any]` and `Record<string, any>` hide behind a
/// generic but assert exactly as little as the bare hatch does. Token
/// equality, not substring: `AnyOf` and `voidptr_t` are ordinary names.
fn is_loose(text: &str, hatches: &[&str]) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|token| hatches.contains(&token))
}

/// Shared helper: does `node`'s field hold a boolean-ish type name?
fn field_text_is<'a>(node: Node, field: &str, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)?.utf8_text(src).ok()
}

#[cfg(test)]
mod conformance {
    //! The "languages are very alike" claim, made executable: the same
    //! function written in each supported language must produce identical
    //! metrics. This suite is what keeps the Sem ontology honest as packs
    //! are added.

    use super::Lang;

    use crate::facts::{FileFacts, extract};
    use crate::metrics::complexity;
    use std::path::Path;

    fn facts(lang: Lang, source: &str) -> FileFacts {
        facts_at(lang, "t", source)
    }

    /// Facts with a real path, for behavior that depends on WHERE the
    /// file lives (test-file conventions).
    fn facts_at(lang: Lang, path: &str, source: &str) -> FileFacts {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), source)
    }

    /// (cognitive, cyclomatic, depth, params) of the first named unit.
    fn signature(lang: Lang, source: &str) -> (u32, u32, u16, u16) {
        let f = facts(lang, source);
        let u = &f.units[1];
        let (cog, cyc) = complexity(u);
        (cog, cyc, u.max_vis_depth, u.params.len() as u16)
    }

    const PY: &str = "
def classify(items, limit):
    total = 0
    for item in items:
        if item > 0 and item < limit:
            total += item
        elif item < 0:
            total -= item
        else:
            continue
    return total
";

    const RS: &str = "
fn classify(items: Vec<i64>, limit: i64) -> i64 {
    let mut total = 0;
    for item in items {
        if item > 0 && item < limit {
            total += item;
        } else if item < 0 {
            total -= item;
        } else {
            continue;
        }
    }
    total
}
";

    const TS: &str = "
function classify(items: number[], limit: number): number {
    let total = 0;
    for (const item of items) {
        if (item > 0 && item < limit) {
            total += item;
        } else if (item < 0) {
            total -= item;
        } else {
            continue;
        }
    }
    return total;
}
";

    const GO: &str = "
package main

func classify(items []int64, limit int64) int64 {
    total := int64(0)
    for _, item := range items {
        if item > 0 && item < limit {
            total += item
        } else if item < 0 {
            total -= item
        } else {
            continue
        }
    }
    return total
}
";

    const JS: &str = "
function classify(items, limit) {
    let total = 0;
    for (const item of items) {
        if (item > 0 && item < limit) {
            total += item;
        } else if (item < 0) {
            total -= item;
        } else {
            continue;
        }
    }
    return total;
}
";

    const ZIG: &str = "
fn classify(items: []const i64, limit: i64) i64 {
    var total: i64 = 0;
    for (items) |item| {
        if (item > 0 and item < limit) {
            total += item;
        } else if (item < 0) {
            total -= item;
        } else {
            continue;
        }
    }
    return total;
}
";

    const C: &str = "
long classify(const long *items, long limit) {
    long total = 0;
    for (int i = 0; items[i]; i++) {
        if (items[i] > 0 && items[i] < limit) {
            total += items[i];
        } else if (items[i] < 0) {
            total -= items[i];
        } else {
            continue;
        }
    }
    return total;
}
";

    /// OCaml's imperative loop. Idiomatic OCaml would iterate with
    /// `List.iter` and a lambda, which the ontology correctly reads as a
    /// CALL rather than a loop — so the conformance case uses the form
    /// that means the same thing the other eight languages mean.
    const ML: &str = "
let classify items limit =
  let total = ref 0 in
  for i = 0 to Array.length items - 1 do
    let item = items.(i) in
    if item > 0 && item < limit then total := !total + item
    else if item < 0 then total := !total - item
    else ()
  done;
  !total
";

    #[test]
    fn same_function_scores_identically_in_every_language() {
        // for +1, if +2 (nested), && +1, elif +1, else +1 = cognitive 6;
        // decisions for/if/&&/elif = cyclomatic 5; depth for>if = 2; 2 params.
        let expected = (6, 5, 2, 2);
        assert_eq!(signature(Lang::Python, PY), expected, "python");
        assert_eq!(signature(Lang::Rust, RS), expected, "rust");
        assert_eq!(signature(Lang::TypeScript, TS), expected, "typescript");
        assert_eq!(signature(Lang::Go, GO), expected, "go");
        assert_eq!(signature(Lang::JavaScript, JS), expected, "javascript");
        assert_eq!(signature(Lang::Zig, ZIG), expected, "zig");
        assert_eq!(signature(Lang::C, C), expected, "c");
        assert_eq!(signature(Lang::OCaml, ML), expected, "ocaml");
    }

    #[test]
    fn param_names_extracted_in_every_language() {
        for (lang, src) in [
            (Lang::Python, PY),
            (Lang::Rust, RS),
            (Lang::TypeScript, TS),
            (Lang::Go, GO),
            (Lang::JavaScript, JS),
            (Lang::Zig, ZIG),
            (Lang::C, C),
            (Lang::OCaml, ML),
        ] {
            let f = facts(lang, src);
            let names: Vec<&str> = f.units[1].params.iter().map(|p| &*p.name).collect();
            assert_eq!(names, ["items", "limit"], "{lang:?}");
        }
    }

    #[test]
    fn goto_costs_the_same_wherever_it_exists() {
        // Go mapped `goto` to Sem::Jump, which scores nothing — the same
        // construct cost 1 in C and 0 in Go. Sem::Goto exists for exactly
        // this: break/continue are free within their loop, but a goto
        // sends the reader hunting for a label.
        let c =
            "int f(int x) {\n    if (x < 0) goto fail;\n    return x;\nfail:\n    return -1;\n}\n";
        let go = "package main\n\nfunc f(x int) int {\n\tif x < 0 {\n\t\tgoto fail\n\t}\n\treturn x\nfail:\n\treturn -1\n}\n";
        assert_eq!(signature(Lang::C, c).0, 2, "c: if + goto");
        assert_eq!(signature(Lang::Go, go).0, 2, "go must agree with c");
    }

    #[test]
    fn receiver_declared_methods_are_methods_in_every_language() {
        // Go declares the receiver in its own field, so the method has no
        // enclosing type node. Feature Envy asks `is_method` first, which
        // made it structurally dead for Go: identical code scored 4 in
        // Python and 0 in Go.
        let envious = |lang: Lang, src: &str| {
            let f = facts(lang, src);
            let u = &f.units[1];
            (u.is_method, u.envy_count, u.qualname.to_string())
        };
        let py = envious(
            Lang::Python,
            "class Billing:\n    def total(self, order):\n        return order.base + order.tax + order.shipping + order.discount\n",
        );
        let go = envious(
            Lang::Go,
            "package main\n\nfunc (b *Billing) Total(order *Order) int {\n\treturn order.base + order.tax + order.shipping + order.discount\n}\n",
        );
        assert_eq!(py, (true, 4, "Billing.total".into()));
        assert_eq!(go, (true, 4, "Billing.Total".into()));

        // The receiver is the method's OWN object, not an envied one —
        // otherwise enabling this would fire on every Go method.
        let own = envious(
            Lang::Go,
            "package main\n\nfunc (b *Billing) Label() string {\n\treturn b.prefix + b.sep + b.suffix + b.tail\n}\n",
        );
        assert_eq!(own, (true, 0, "Billing.Label".into()));
    }

    #[test]
    fn escape_hatches_are_found_through_their_generics() {
        // A hatch hidden in a generic asserts exactly as little as the
        // bare hatch: `dict[str, Any]` and `Record<string, any>` are the
        // form real code actually takes.
        let loose = |lang: Lang, src: &str| {
            let f = facts(lang, src);
            f.units[1..]
                .iter()
                .map(|u| (u.untyped_params(), u.loose_params()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            loose(
                Lang::Python,
                "from typing import Any\n\ndef clean(name: str, n: int) -> str:\n    return name\n\ndef sloppy(payload, options):\n    return payload\n\ndef fake(payload: Any, options: dict[str, Any]) -> Any:\n    return payload\n",
            ),
            [(0, 0), (2, 0), (0, 2)]
        );
        assert_eq!(
            loose(
                Lang::TypeScript,
                "export function clean(name: string, n: number): string { return name; }\nexport function sloppy(payload, options) { return payload; }\nexport function fake(payload: any, o: Record<string, any>): any { return payload; }\nexport function safe(payload: unknown): string { return String(payload); }\n",
            ),
            [(0, 0), (2, 0), (0, 2), (0, 0)],
            "unknown is the SAFE alternative to any — flagging it would punish the fix"
        );
        // Statically-typed languages have no untyped surface at all, so
        // the metric must read zero rather than every parameter.
        assert_eq!(
            loose(
                Lang::Go,
                "package p\n\nfunc Clean(name string, n int) string { return name }\n\nfunc Fake(payload interface{}, opts []interface{}) any { return payload }\n",
            ),
            [(0, 0), (0, 2)]
        );
    }

    #[test]
    fn every_language_spells_its_own_cast_and_they_all_count() {
        // Each language overrules its type checker differently; the point
        // of Sem::Cast is that the metric never has to know which.
        let counted = |lang: Lang, src: &str| {
            let f = facts(lang, src);
            let casts: u16 = f.units.iter().map(|u| u.casts).sum();
            (casts, f.suppressions.len())
        };
        assert_eq!(
            counted(
                Lang::TypeScript,
                "export function loose(raw: unknown): string {\n  // @ts-ignore legacy shape\n  const a = raw as string;\n  const b = (raw as any).value!;\n  return a + b;\n}\n",
            ),
            (3, 1),
            "as, as any, and the non-null assertion"
        );
        assert_eq!(
            counted(
                Lang::Python,
                "from typing import cast\n\ndef convert(raw):\n    a = cast(str, raw)  # type: ignore[arg-type]\n    return a\n",
            ),
            (1, 1)
        );
        assert_eq!(
            counted(
                Lang::Go,
                "package p\n\nfunc Convert(raw interface{}) string {\n\ts := raw.(string)\n\treturn s\n}\n",
            ),
            (1, 0)
        );
        assert_eq!(
            counted(
                Lang::C,
                "void *dup(void *p, unsigned n) {\n    char *q = (char *)p;\n    return (void *)(q + n);\n}\n",
            ),
            (2, 0)
        );
        assert_eq!(
            counted(
                Lang::Rust,
                "fn narrow(n: u64) -> u32 {\n    let a = n as u32;\n    a as u32\n}\n",
            ),
            (2, 0),
            "numeric `as` is Rust's silent truncation"
        );
    }

    #[test]
    fn a_framework_declared_test_is_a_unit_in_js_and_ts() {
        // vitest, jest, mocha and node:test all spell a test as an
        // anonymous arrow passed to a call. Without recognising that,
        // every test in the JS/TS ecosystem is invisible: green-screen
        // reported 0% test units while having tests, and hono's gold
        // corpus read 53% when the real figure is 85%.
        let src = "import test from 'node:test';\n\ntest('resolves regional aliases', () => {\n  assert.equal(resolve('en-GB'), 'en');\n});\n\nconst helper = () => 1;\n";
        for lang in [Lang::TypeScript, Lang::JavaScript] {
            let f = facts(lang, src);
            let named: Vec<(&str, bool)> = f.units[1..]
                .iter()
                .map(|u| (&*u.name, u.named_test))
                .collect();
            assert_eq!(
                named,
                [("resolves regional aliases", true), ("helper", false)],
                "{lang:?}: the test takes its name from its first argument"
            );
        }

        // A callback that is not a test stays attributed to its caller.
        let cb = facts(
            Lang::TypeScript,
            "items.forEach((x) => { seen.push(x); });\n",
        );
        assert!(
            cb.units.len() == 1,
            "an ordinary callback is not promoted to a unit"
        );
    }

    #[test]
    fn a_test_is_declared_only_by_evidence_its_context_supports() {
        // One name used to do two jobs: a production `test_connection`
        // health check was judged as a lazy assertless test AND pardoned
        // from every production metric. Name conventions declare only
        // inside test files; attributes and structure declare anywhere;
        // cfg(test) membership exempts without declaring.
        type Case = (Lang, &'static str, &'static str, (bool, bool), &'static str);
        let cases: &[Case] = &[
            (
                Lang::Python,
                "prod/net.py",
                "def test_connection(h):\n    return ping(h, 9042)\n",
                (false, false),
                "a prod test_-name is neither judged nor pardoned",
            ),
            (
                Lang::Python,
                "tests/test_net.py",
                "def test_connection(h):\n    assert ping(h)\n",
                (true, true),
                "the same name in a test file is the convention at work",
            ),
            (
                Lang::Python,
                "tests/test_net.py",
                "def make_server():\n    return object()\n",
                (false, true),
                "a test-file helper is exempt but never judged",
            ),
            (
                Lang::Go,
                "svc/health.go",
                "package p\n\nfunc TestConnection() bool { return true }\n",
                (false, false),
                "a production TestConnection is a function",
            ),
            (
                Lang::Go,
                "svc/health_test.go",
                "package p\n\nfunc TestConnection(t *T) { assertOk(t) }\n",
                (true, true),
                "the same name in _test.go is a test",
            ),
            (
                Lang::Rust,
                "src/lib.rs",
                "#[test]\nfn rejects_bad_input() {\n    assert!(1 == 1);\n}\n",
                (true, true),
                "an attribute is context-free evidence",
            ),
            (
                Lang::Rust,
                "src/lib.rs",
                "#[cfg(test)]\nmod tests {\n    fn fixture() -> u32 {\n        1\n    }\n}\n",
                (false, true),
                "a cfg(test) fixture is exempt but never judged",
            ),
            (
                Lang::TypeScript,
                "src/anywhere.ts",
                "test('resolves aliases', () => { assert.ok(resolve('en')); });\n",
                (true, true),
                "structure is context-free evidence",
            ),
        ];
        for (lang, path, src, want, why) in cases {
            let f = facts_at(*lang, path, src);
            let u = &f.units[1];
            assert_eq!((u.named_test, u.is_test), *want, "{why}");
        }
    }

    /// The count-shaped detector metrics whose liveness is decided by
    /// pack hooks and tables — the exact surface where three confirmed
    /// bugs (secrets dead in Rust/Zig/C, Go's error family, four
    /// languages' wildcard arms) died in silence.
    const DETECTORS: &[&str] = &[
        "secrets",
        "swallowed",
        "broad catch",
        "lost context",
        "unwraps",
        "blocking async",
        "unmanaged",
        "dropped tasks",
        "casts",
        "suppressions",
        "spooky",
        "wildcard match",
        "negations",
        "demeter",
        "kw opacity",
        "flag params",
        "loose types",
        "untyped params",
        "lying name",
        "vacuous asserts",
        "test asserts",
        "conditional hook",
        "repurposed",
        "unawaited coroutine",
    ];

    /// Pairs that can NEVER fire, each with its reason. Deliberate
    /// deadness is a design decision stated here; ACCIDENTAL deadness
    /// is how detectors die in silence. Fixing a pack must shrink this
    /// list — the parity test refuses a pair that is both seeded alive
    /// and declared dead.
    const DECLARED_DEAD: &[(Lang, &str, &str)] = &[
        (
            Lang::Python,
            "unwraps",
            "exceptions are the error path; no panic-instead-of-error idiom",
        ),
        (
            Lang::Python,
            "dropped tasks",
            "create_task is the idiom; unrecognized — deferred, not denied",
        ),
        (
            Lang::Rust,
            "swallowed",
            "errors are values; misuse surfaces as unwraps, not empty handlers",
        ),
        (Lang::Rust, "broad catch", "no exceptions"),
        (
            Lang::Rust,
            "lost context",
            "no exceptions, no chain to lose",
        ),
        (
            Lang::Rust,
            "suppressions",
            "#[allow] is scoped lint config, not a checker-comment",
        ),
        (
            Lang::Rust,
            "unmanaged",
            "RAII drops on scope exit; there is no guard to omit",
        ),
        (Lang::Rust, "kw opacity", "no kwargs"),
        (
            Lang::Rust,
            "untyped params",
            "the language types every parameter",
        ),
        (
            Lang::Rust,
            "loose types",
            "no Any; the hatch is unsafe, judged as spooky and casts",
        ),
        (Lang::TypeScript, "unwraps", "no panic idiom"),
        (Lang::TypeScript, "unmanaged", "no scope-guard idiom"),
        (
            Lang::TypeScript,
            "kw opacity",
            "an options object is a declared shape, not a splat",
        ),
        (
            Lang::TypeScript,
            "broad catch",
            "catch binds everything by language design; there is no narrow catch to prefer",
        ),
        (
            Lang::JavaScript,
            "broad catch",
            "catch binds everything by language design",
        ),
        (Lang::Go, "broad catch", "errors are values"),
        (
            Lang::Go,
            "lost context",
            "no exception chain; %w-wrapping judgment deferred",
        ),
        (
            Lang::Go,
            "suppressions",
            "nolint is linter config, not a checker-comment",
        ),
        (
            Lang::Go,
            "unmanaged",
            "defer is the idiom; recognizing its absence needs the defer — deferred",
        ),
        (Lang::Go, "kw opacity", "no kwargs"),
        (Lang::Go, "untyped params", "every parameter typed"),
        (
            Lang::Go,
            "blocking async",
            "goroutines have no single-threaded executor to stall",
        ),
        (
            Lang::Go,
            "dropped tasks",
            "the go statement returns no handle to drop",
        ),
        (Lang::Go, "spooky", "no eval; reflection judgment deferred"),
        (
            Lang::JavaScript,
            "casts",
            "no cast syntax; coercion is invisible to the tree",
        ),
        (Lang::JavaScript, "unwraps", "no panic idiom"),
        (Lang::JavaScript, "unmanaged", "no scope-guard idiom"),
        (Lang::JavaScript, "kw opacity", "no kwargs"),
        (
            Lang::JavaScript,
            "loose types",
            "no type syntax to be loose in",
        ),
        (
            Lang::JavaScript,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::JavaScript,
            "lying name",
            "no declared return types to lie against",
        ),
        (
            Lang::Zig,
            "swallowed",
            "catch is an expression; empty-body judgment deferred",
        ),
        (Lang::Zig, "broad catch", "no typed catch to be broad"),
        (Lang::Zig, "lost context", "errors are values"),
        (
            Lang::Zig,
            "suppressions",
            "no checker-comment suppression exists",
        ),
        (
            Lang::Zig,
            "unmanaged",
            "defer is the idiom — same deferral as Go",
        ),
        (Lang::Zig, "kw opacity", "no kwargs"),
        (
            Lang::Zig,
            "untyped params",
            "every parameter typed; anytype is loose, not untyped",
        ),
        (Lang::Zig, "blocking async", "async is out of the language"),
        (
            Lang::Zig,
            "dropped tasks",
            "Thread.spawn returns !Thread; try-wrapping precludes the bare-statement shape",
        ),
        (Lang::Zig, "spooky", "no eval; @bitCast is judged as a cast"),
        (
            Lang::Zig,
            "negations",
            "the grammar parses !expr as an error-union type; negation is invisible until upstream disambiguates",
        ),
        (Lang::C, "swallowed", "no exceptions"),
        (Lang::C, "broad catch", "no exceptions"),
        (Lang::C, "lost context", "no exceptions"),
        (
            Lang::C,
            "suppressions",
            "no checker-comment suppression exists",
        ),
        (
            Lang::C,
            "unmanaged",
            "manual everywhere; the absence is universal and the finding would be noise",
        ),
        (Lang::C, "kw opacity", "no kwargs"),
        (Lang::C, "untyped params", "every parameter typed"),
        (Lang::C, "blocking async", "no async"),
        (Lang::C, "dropped tasks", "no spawn idiom"),
        (
            Lang::C,
            "vacuous asserts",
            "no test-declaration form; assert() is a production invariant",
        ),
        (Lang::C, "test asserts", "no test-declaration form"),
        (
            Lang::OCaml,
            "secrets",
            "let_binding doubles as the unit definition; value anchoring deferred",
        ),
        (
            Lang::OCaml,
            "swallowed",
            "no Catch mapped; try is measured as structure",
        ),
        (Lang::OCaml, "broad catch", "no Catch mapped"),
        (
            Lang::OCaml,
            "lost context",
            "no chain-keeping idiom to check",
        ),
        (
            Lang::OCaml,
            "suppressions",
            "no checker-comment suppression exists",
        ),
        (
            Lang::OCaml,
            "unmanaged",
            "let-scoped resources release with their binding",
        ),
        (Lang::OCaml, "kw opacity", "no kwargs"),
        (
            Lang::OCaml,
            "blocking async",
            "async is a library, not syntax",
        ),
        (Lang::OCaml, "dropped tasks", "no spawn idiom recognized"),
        (
            Lang::OCaml,
            "casts",
            "no cast syntax; Obj.magic judgment deferred",
        ),
        (Lang::OCaml, "spooky", "Obj.magic judgment deferred"),
        (
            Lang::OCaml,
            "negations",
            "not is an ordinary function application",
        ),
        (
            Lang::OCaml,
            "demeter",
            "field access is module access; no attr node mapped",
        ),
        (
            Lang::OCaml,
            "flag params",
            "types inferred; boolishness invisible in the signature",
        ),
        (Lang::OCaml, "loose types", "no hatch vocabulary"),
        (Lang::OCaml, "lying name", "no return-type annotations"),
        (
            Lang::OCaml,
            "vacuous asserts",
            "applications carry no arguments field to inspect",
        ),
        (
            Lang::OCaml,
            "test asserts",
            "no test-declaration form recognized",
        ),
        // Shell declares no parameters (`$1` is read from the caller's
        // frame) and no types, so the whole interface family is silent
        // by the language's own design rather than by omission.
        (
            Lang::Shell,
            "kw opacity",
            "a function takes $@; no keyword surface exists to obscure",
        ),
        (Lang::Shell, "flag params", "no declared parameters at all"),
        (Lang::Shell, "loose types", "no type syntax to be loose in"),
        (
            Lang::Shell,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::Shell,
            "lying name",
            "no declared return types to lie against",
        ),
        (
            Lang::Shell,
            "casts",
            "every value is a string; there is no cast to count",
        ),
        (
            Lang::Shell,
            "swallowed",
            "`cmd || true` is EXPLICIT silencing, which the Zen exempts",
        ),
        (Lang::Shell, "broad catch", "no exceptions"),
        (Lang::Shell, "lost context", "no exceptions"),
        (
            Lang::Shell,
            "unwraps",
            "`exit 1` is ordinary control flow in a script, not a panic",
        ),
        (
            Lang::Shell,
            "suppressions",
            "shellcheck directives are lint config, not a checker-comment",
        ),
        (
            Lang::Shell,
            "unmanaged",
            "`trap ... EXIT` is the idiom; recognizing its absence needs the trap — deferred",
        ),
        (Lang::Shell, "blocking async", "no async"),
        (
            Lang::Shell,
            "dropped tasks",
            "`cmd &` returns no handle to drop",
        ),
        (
            Lang::Shell,
            "demeter",
            "a value is a string, not an object; no member access exists",
        ),
        // bats and shunit2 assert with the `[` builtin, which is
        // indistinguishable from ordinary control flow, so every
        // test-named shell function reads as assertionless — the
        // corpus said so at 100%. Shell declares no tests at all.
        (
            Lang::Shell,
            "vacuous asserts",
            "no test-declaration form: `[` is both assertion and control flow",
        ),
        (
            Lang::Shell,
            "test asserts",
            "no test-declaration form: `[` is both assertion and control flow",
        ),
        // Hooks are React's idea, and the rule exists because React
        // identifies a hook by CALL ORDER. No other language here has
        // an order-identified construct to break.
        (Lang::Python, "conditional hook", "no call-order identity"),
        (Lang::Rust, "conditional hook", "no call-order identity"),
        (Lang::Go, "conditional hook", "no call-order identity"),
        (Lang::Zig, "conditional hook", "no call-order identity"),
        (Lang::C, "conditional hook", "no call-order identity"),
        (Lang::OCaml, "conditional hook", "no call-order identity"),
        (Lang::Shell, "conditional hook", "no call-order identity"),
        (
            Lang::OCaml,
            "repurposed",
            "a let is a fresh binding and `:=` writes through a ref, never rebinding the name",
        ),
        (
            Lang::Go,
            "unawaited coroutine",
            "a goroutine RUNS when spawned; there is no coroutine object to drop",
        ),
        (Lang::C, "unawaited coroutine", "no async"),
        (
            Lang::Zig,
            "unawaited coroutine",
            "async left the language; the grammar has no await to see",
        ),
        (
            Lang::OCaml,
            "unawaited coroutine",
            "no await syntax — concurrency is a library of ordinary functions",
        ),
        (Lang::Shell, "unawaited coroutine", "no async"),
        (
            Lang::TypeScript,
            "unawaited coroutine",
            "a promise is eagerly scheduled: the call RUNS and only its rejection goes unobserved — a weaker claim admired code violates deliberately (144 telemetry sends in gold), owned by no-floating-promises",
        ),
        (
            Lang::JavaScript,
            "unawaited coroutine",
            "a promise is eagerly scheduled: the call RUNS — same verdict as TypeScript",
        ),
    ];

    #[test]
    fn every_detector_is_seeded_alive_or_declared_dead() {
        // Deadness must be a decision, never an accident: for each
        // (detector, language) pair, either the recall fixtures PROVE
        // it alive on every run, or this table states why it cannot
        // fire. Both at once means the table is stale; neither means a
        // detector may be dying in silence right now.
        for lang in super::LANGS {
            // The TSX pack is the TS pack under a JSX-aware grammar;
            // its parity follows TypeScript's evidence.
            let evidence = match lang {
                Lang::Tsx => Lang::TypeScript,
                l => l,
            };
            for metric in DETECTORS {
                let seeded = crate::recall::SEEDS.iter().any(|(file, m, _)| {
                    m == metric
                        && crate::recall::FIXTURES
                            .iter()
                            .any(|(f, l, _)| f == file && *l == evidence)
                });
                let declared = DECLARED_DEAD
                    .iter()
                    .any(|(l, m, _)| *l == evidence && m == metric);
                assert!(
                    seeded || declared,
                    "{lang:?} x {metric}: neither seeded alive nor declared dead — a detector may be dying in silence"
                );
                assert!(
                    !(seeded && declared),
                    "{lang:?} x {metric}: seeded AND declared dead — the matrix is stale"
                );
            }
        }
    }

    #[test]
    fn every_pack_name_resolves_in_its_grammar() {
        // The Zig @import lesson: an unmapped kind name silently zeroes a
        // metric, calibration then pins that zero, and the zero-guard
        // skips it — invisible decay. A grammar bump must fail HERE.
        let mut broken = Vec::new();
        for lang in super::LANGS {
            for name in lang.pack().unresolved() {
                broken.push(format!("{lang:?}: {name}"));
            }
        }
        assert!(broken.is_empty(), "unresolved grammar names:\n{broken:#?}");
    }

    #[test]
    fn grammar_abi_versions_are_pinned() {
        // Bumping a tree-sitter grammar crate must be a conscious act:
        // update this table and re-run the conformance suite.
        let pinned: Vec<String> = super::LANGS
            .iter()
            .map(|l| format!("{} {}", l.name(), l.pack().ts.abi_version()))
            .collect();
        assert_eq!(
            pinned.join(", "),
            "py 15, rs 15, ts 14, tsx 14, go 15, js 15, zig 14, c 15, ml 14, sh 15"
        );
    }

    #[test]
    fn lang_discriminants_match_langs_order() {
        // Every per-language array indexes by `Lang as usize`; LANGS order
        // and enum declaration order must never drift apart.
        for (i, l) in super::LANGS.iter().enumerate() {
            assert_eq!(*l as usize, i, "{l:?}");
        }
    }

    #[test]
    fn identical_logic_produces_identical_clone_hashes_within_language() {
        // Type-2: renamed identifiers and changed literals must not matter.
        let f = facts(
            Lang::Rust,
            "
fn alpha(xs: Vec<i64>) -> i64 {
    let mut acc = 0;
    for x in xs {
        if x > 10 {
            acc += x;
        }
    }
    acc
}

fn beta(items: Vec<i64>) -> i64 {
    let mut total = 0;
    for value in items {
        if value > 999 {
            total += value;
        }
    }
    total
}
",
        );
        let dup: Vec<_> = f
            .clone_sites
            .iter()
            .filter(|s| f.clone_sites.iter().filter(|t| t.hash == s.hash).count() >= 2)
            .collect();
        assert!(!dup.is_empty(), "renamed twin functions must collide");
    }
}
