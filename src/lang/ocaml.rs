//! OCaml: the calibration reference for the bar this tool is named for.
//!
//! Jane Street's Base is in the gold corpus for a reason. An ML-family
//! language with exhaustive matching, no nulls and no exceptions in the
//! happy path is the closest thing to a control group for "what does
//! code look like when the type system is doing the work" — which is
//! the question the type-hygiene and wildcard-match metrics ask of
//! everyone else.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    // A let binding is a definition whether it names a function or a
    // value; `open_unit` measures the ones with parameters.
    ("let_binding", Sem::FnDef),
    ("fun_expression", Sem::Lambda),
    ("type_definition", Sem::TypeDef),
    ("module_definition", Sem::TypeDef),
    ("if_expression", Sem::If),
    // `&&`/`||` are infix applications; refine keeps only those two.
    ("infix_expression", Sem::BoolOp),
    ("else_clause", Sem::Else),
    ("for_expression", Sem::Loop),
    ("while_expression", Sem::Loop),
    ("match_expression", Sem::Match),
    ("match_case", Sem::CaseArm),
    ("try_expression", Sem::Try),
    ("application_expression", Sem::Call),
    ("comment", Sem::Comment),
    ("open_module", Sem::Import),
    ("value_name", Sem::Ident),
    ("type_constructor", Sem::Ident),
    ("constructor_name", Sem::Ident),
    ("module_name", Sem::Ident),
    ("number", Sem::NumLit),
    ("string", Sem::StrLit),
    ("character", Sem::StrLit),
    ("boolean", Sem::BoolLit),
];

const DEF_SITES: &[(&str, &str)] = &[];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_ocaml::LANGUAGE_OCAML.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    Pack {
        lang: Lang::OCaml,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        // A let is a fresh binding and `:=` writes through a ref cell
        // without rebinding the name — reassignment does not exist.
        reassign_names: &[],
        attr_name: None,
        sems,
        def_sites,
        reassigns: Box::new([]),
        attr: None,
        scope_sep: ".",
        return_type_field: "",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: true,
        // Anonymous records exist, but a record literal is checked
        // against a declared type — there is no undeclared shape.
        record_keys: |_, _| None,
        // No exceptions in the happy path and no scope-guard statement:
        // resources are released by the same `let ... in` structure that
        // scopes them.
        unguarded_resource: |_, _| false,
        // Async is a library (Lwt, Async), not syntax.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        // `(** ... *)` is OCaml's documentation comment.
        doc_markers: &["(**"],
        is_public,
        unit_docs,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky: |_, _, _| false,
        negation_operand: |_, _| None,
        catch_sin: |_, _| None,
        swallows_error: |_, _| false,
        loses_context: |_, _| false,
        panicky,
        declares_test: |_, _| false,
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/test/") || p.contains("/tests/"),
        asserty: |call, src| {
            callee_text(call, src).is_some_and(|t| super::assertish(t) || t.starts_with("[%test"))
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        // A tuple is the language's ordinary single value and the
        // result is just the tail expression — there is no declared
        // return position to read a width from.
        return_arity: |_, _| 0,
        // The interface unit is the module signature, whose width is
        // the module's surface — already measured, not a type's
        // method contract.
        interfaces: |_, _| Vec::new(),
        // No test-declaration form, so nothing to switch off.
        skips_test: |_, _| false,
        magic_exempt: &["type_definition"],
        assign_kinds: &[],
    }
}

/// `let f x = ...` names the binding; the pattern holds the name.
fn name_node(node: Node) -> Option<Node> {
    (node.kind() == "let_binding")
        .then(|| node.child_by_field_name("pattern"))?
        .filter(|p| p.kind() == "value_name")
}

/// `open Core` — the module becomes visible, so it is an import edge.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let Some(name) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "module_path" || c.kind() == "module_name")
    else {
        return Vec::new();
    };
    let text = name.utf8_text(src).unwrap_or("");
    vec![super::ImportInfo {
        target: text.into(),
        names: Vec::new(),
    }]
}

/// A let binding lists its parameters as direct children, so every
/// other child of the binding is offered here too and declined.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if node.kind() != "parameter" {
        return None;
    }
    let name = node.utf8_text(src).ok()?;
    // Labelled arguments carry their name with a leading `~` or `?`;
    // the `?` ones are optional at every call site.
    let bare = name.trim_start_matches(['~', '?']);
    (!bare.is_empty()).then(|| ParamInfo {
        name: bare.into(),
        optional: name.starts_with('?'),
        typed: false,
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    callee_text(call, src) == Some(unit_name)
}

/// The leftmost term of an application is what is being called.
fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.named_child(0)?.utf8_text(src).ok()
}

/// `failwith`/`assert false` are OCaml's panics; `raise` is a declared
/// error path and is not one.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("failwith" | "invalid_arg"))
}

/// Without an interface file the whole module is surface. A binding
/// prefixed with `_` is conventionally internal.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("pattern")
        .and_then(|p| p.utf8_text(src).ok())
        .is_some_and(|n| !n.starts_with('_'))
}

/// `(** ... *)` immediately above the binding.
fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let mut prev = node.prev_named_sibling();
    // A value_definition wraps the binding, so look outward once.
    if prev.is_none() {
        prev = node.parent().and_then(|p| p.prev_named_sibling());
    }
    prev.filter(|p| p.kind() == "comment")
        .and_then(|p| p.utf8_text(src).ok())
        .filter(|t| t.starts_with("(**"))
        .map_or(0, |t| t.lines().count() as u32)
}

/// Two normalizations. `else if` nests an if inside an else clause, as
/// in Rust and TypeScript, and flattens the same way. And `&&`/`||`
/// share the infix kind with every arithmetic and comparison operator,
/// so only those two count as boolean sequences.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::If if node.parent().is_some_and(|p| p.kind() == "else_clause") => Sem::ElseIf,
        Sem::Else
            if node
                .named_child(0)
                .is_some_and(|c| c.kind() == "if_expression") =>
        {
            Sem::None
        }
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        _ => sem,
    }
}
