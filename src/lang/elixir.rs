//! Elixir: there is no syntax, so the ontology is built from names.
//!
//! `def`, `defmodule`, `if`, `case` and `try` are all ordinary calls
//! taking a block — the language is homoiconic and its grammar says so,
//! offering `call` where every other pack here reads a keyword. So this
//! pack does its whole classification in `refine`, dispatching on the
//! call's target text. A macro a library defines is indistinguishable
//! from one the language ships, which is the language working as
//! designed and a real bound on what can be read: `Enum.each` is a
//! function call and reads as one, so loops read low the way they do for
//! OCaml and Ruby.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    // Everything interesting arrives here and is sorted in `refine`.
    ("call", Sem::Call),
    ("anonymous_function", Sem::Lambda),
    // The `->` clause: a case arm, a rescue arm, a function head.
    ("stab_clause", Sem::CaseArm),
    ("binary_operator", Sem::BoolOp),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("alias", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("charlist", Sem::StrLit),
    ("atom", Sem::StrLit),
    ("boolean", Sem::BoolLit),
];

/// Nothing: a local is bound by `=`, which shares its node with every
/// comparison and arithmetic operator, so pointing at it would call
/// every `==` a definition.
const DEF_SITES: &[(&str, &str)] = &[];

/// Rebinding is the norm here and carries none of the meaning it does
/// elsewhere — `x = transform(x)` is a pipeline written without `|>`.
const REASSIGNS: &[(&str, &str)] = &[];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_elixir::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    Pack {
        lang: Lang::Elixir,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: None,
        sems,
        def_sites,
        reassigns: Box::new([]),
        attr: None,
        scope_sep: ".",
        return_type_field: "",
        bool_op_field: "operator",
        types_declared: false,
        record_keys,
        // A process owns its resources and dies with them; that is the
        // supervision tree's job rather than a scope guard's.
        unguarded_resource: |_, _| false,
        // Concurrency is processes and Task, never a keyword.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["@doc", "@moduledoc"],
        is_public,
        unit_docs,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin: |_, _| None,
        swallows_error,
        loses_context: |_, _| false,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/test/") || p.ends_with("_test.exs"),
        asserty,
        is_hook: |_, _| false,
        // A function returns a term, commonly `{:ok, value}`. The tuple
        // is a value rather than a declaration, so there is no width.
        return_arity: |_, _| 0,
        // A behaviour declares callbacks with `@callback` attributes,
        // which are module attributes rather than a body this pack can
        // count members of.
        interfaces: |_, _| Vec::new(),
        skips_test,
        magic_exempt: &["map", "keywords", "list"],
        assign_kinds: &[],
    }
}

/// The target of a call: `def` for a definition, `Enum.map` for a call.
fn target_text<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name("target")?.utf8_text(src).ok()
}

/// The argument list is a KIND here, not a field — the grammar labels
/// only `target`, `left`, `right`, `operator`, `key` and `value`.
fn args_of(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "arguments")
}

/// A definition's name lives one level down: `def run(x)` is a call to
/// `def` whose first argument is itself a call, named `run`.
fn name_node(node: Node) -> Option<Node> {
    let args = args_of(node)?;
    let first = args.named_child(0)?;
    match first.kind() {
        // `def run(x)` — a call whose target is the name.
        "call" => first.child_by_field_name("target"),
        // `def run do`, and `defmodule Foo do`.
        "identifier" | "alias" => Some(first),
        // `def run(x) when is_list(x)` — the guard wraps the head.
        "binary_operator" => first
            .child_by_field_name("left")
            .and_then(|l| l.child_by_field_name("target").or(Some(l))),
        _ => None,
    }
}

/// `import`, `alias`, `require` and `use` all pull a module in.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if !matches!(
        target_text(node, src),
        Some("import" | "alias" | "require" | "use")
    ) {
        return Vec::new();
    }
    let Some(target) = args_of(node)
        .and_then(|a| a.named_child(0))
        .filter(|c| c.kind() == "alias")
        .and_then(|c| c.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    vec![super::ImportInfo {
        target: target.into(),
        names: Vec::new(),
    }]
}

/// Parameters are the patterns in the head, and a pattern is not always
/// a name — `def handle(%User{id: id})` destructures. Only the plain
/// identifiers are read; the rest are shapes rather than parameters.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if node.kind() != "identifier" {
        return None;
    }
    let parent = node.parent()?;
    if parent.kind() != "arguments" {
        return None;
    }
    // The head of a `def`: its parent call sits under a `def` call.
    let head = parent.parent()?;
    let outer = head.parent()?.parent()?;
    if !matches!(target_text(outer, src), Some("def" | "defp" | "defmacro")) {
        return None;
    }
    Some(ParamInfo {
        name: node.utf8_text(src).ok()?.into(),
        typed: false,
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('.').next().unwrap_or(unit_name);
    target_text(call, src).is_some_and(|t| t.rsplit('.').next().unwrap_or(t) == bare)
}

/// Code compiled at run time, and a function named by a term.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && target_text(node, src).is_some_and(|t| {
            matches!(
                t.rsplit('.').next().unwrap_or(t),
                "eval_string" | "eval_quoted" | "apply" | "compile_string" | "compile_quoted"
            )
        })
}

/// `!x` and the word form `not x`.
fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    if node.kind() != "unary_operator" {
        return None;
    }
    let text = node.utf8_text(src).ok()?;
    (text.starts_with('!') || text.starts_with("not "))
        .then(|| node.child_by_field_name("operand"))?
}

/// `raise` leaves by the exception path; a bang-suffixed function is
/// the convention for one that raises rather than returning `:error`.
fn panicky(call: Node, src: &[u8]) -> bool {
    target_text(call, src).is_some_and(|t| matches!(t, "raise" | "throw" | "exit"))
}

/// A `rescue` block with an empty body, or one that matches everything
/// and does nothing with it.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "rescue_block" {
        return false;
    }
    node.utf8_text(src)
        .is_ok_and(|t| t.trim().trim_start_matches("rescue").trim().is_empty())
}

/// ExUnit declares a test with `test "name" do`.
fn declares_test(node: Node, src: &[u8]) -> bool {
    matches!(
        target_text(node, src),
        Some("test" | "property" | "describe")
    )
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| t.contains("@tag :skip") || t.contains("@tag :pending"))
}

fn asserty(call: Node, src: &[u8]) -> bool {
    target_text(call, src).is_some_and(|t| {
        super::assertish(t) || matches!(t, "assert" | "refute" | "assert_receive" | "assert_raise")
    })
}

/// `defp` is the private form; `def` is public and there is no third.
fn is_public(node: Node, src: &[u8]) -> bool {
    target_text(node, src) != Some("defp")
}

/// `@doc """..."""` above the definition. The attribute is a call, so
/// this reads the sibling rather than a comment run.
fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let Some(prev) = node.prev_named_sibling() else {
        return 0;
    };
    if !matches!(target_text(prev, src), Some("@doc" | "@moduledoc")) {
        return 0;
    }
    prev.utf8_text(src).map_or(0, |t| t.lines().count() as u32)
}

/// A map literal with atom keys is a shape nothing declares — the same
/// argument as a Ruby hash or a Lua table.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "map" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "map_content")
        .flat_map(|content| {
            let mut inner = content.walk();
            content
                .named_children(&mut inner)
                .filter_map(|p| p.child_by_field_name("key"))
                .filter_map(|k| k.utf8_text(src).ok())
                .map(Box::<str>::from)
                .collect::<Vec<_>>()
        })
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// The whole ontology, decided by the name being called. A `call` is
/// the only structural node the grammar offers, so every keyword this
/// language appears to have is recognised here or not at all.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::Call => match target_text(node, src) {
            Some("def" | "defp" | "defmacro" | "defmacrop" | "defdelegate" | "defguard") => {
                Sem::FnDef
            }
            Some("defmodule" | "defprotocol" | "defimpl" | "defstruct" | "defexception") => {
                Sem::TypeDef
            }
            Some("if" | "unless") => Sem::If,
            Some("case" | "cond" | "with" | "receive") => Sem::Match,
            // `for` is a comprehension, which is this language's loop.
            Some("for") => Sem::Loop,
            Some("try") => Sem::Try,
            Some("import" | "alias" | "require" | "use") => Sem::Import,
            _ => Sem::Call,
        },
        // `and`/`or`/`&&`/`||` sequence a condition; `|>`, `<>`, `++`
        // and every comparison share the node and do not.
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("and" | "or" | "&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        _ => sem,
    }
}
