use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, field_text_is, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_item", Sem::FnDef),
    ("closure_expression", Sem::Lambda),
    ("await_expression", Sem::Await),
    ("struct_item", Sem::TypeDef),
    ("enum_item", Sem::TypeDef),
    ("union_item", Sem::TypeDef),
    ("trait_item", Sem::TypeDef),
    ("impl_item", Sem::TypeDef),
    ("if_expression", Sem::If),
    ("else_clause", Sem::Else),
    ("while_expression", Sem::Loop),
    ("loop_expression", Sem::Loop),
    ("for_expression", Sem::Loop),
    ("match_expression", Sem::Match),
    ("match_arm", Sem::CaseArm),
    // All binary operators share one kind; refine keeps only `&&`/`||`.
    ("binary_expression", Sem::BoolOp),
    ("break_expression", Sem::Jump),
    ("continue_expression", Sem::Jump),
    ("call_expression", Sem::Call),
    ("type_cast_expression", Sem::Cast),
    // Macros are call-shaped: they carry logic and assert in tests.
    ("macro_invocation", Sem::Call),
    ("line_comment", Sem::Comment),
    ("block_comment", Sem::Comment),
    ("use_declaration", Sem::Import),
    ("identifier", Sem::Ident),
    ("field_identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("integer_literal", Sem::NumLit),
    ("float_literal", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("raw_string_literal", Sem::StrLit),
    ("boolean_literal", Sem::BoolLit),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("let_declaration", "pattern"),
    ("for_expression", "pattern"),
];
/// `x = ...` on a `mut` binding. A shadowing `let x` is a NEW binding —
/// the idiomatic remedy, and scope-blind judging would flag sibling
/// blocks — and `compound_assignment_expr` is a collecting update.
const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("field_expression", "value");

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Rust,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: Some(ATTR),
        sems,
        def_sites,
        reassigns,
        attr,
        scope_sep: "::",
        return_type_field: "return_type",
        bool_op_field: "operator",
        types_declared: true,
        refine,
        name_node: |_| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &[],
        is_public,
        unit_docs,
        spooky,
        negation_operand: |node, src| {
            (node.kind() == "unary_expression"
                && node.utf8_text(src).is_ok_and(|t| t.starts_with('!')))
            .then(|| node.named_child(0))?
        },
        catch_sin: |_, _| None,
        swallows_error: |_, _| false,
        // No exceptions, so no chain to break.
        loses_context: |_, _| false,
        panicky,
        // RAII: a File closes when it drops, so there is no guard to omit.
        // Struct literals name a declared type; there is no anonymous shape to catch.
        record_keys: |_, _| None,
        unguarded_resource: |_, _| false,
        is_async: super::declared_async,
        declares_test,
        names_test: |_, _| false,
        is_test_code,
        test_path: |p| p.contains("/tests/") || p.ends_with("_test.rs"),
        asserty: |node, src| {
            node.child_by_field_name("macro")
                .and_then(|m| m.utf8_text(src).ok())
                .is_some_and(|m| m.starts_with("assert") || m.starts_with("debug_assert"))
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        return_arity,
        interfaces,
        magic_exempt: &[
            "const_item",
            "static_item",
            "match_arm",
            "index_expression",
            "type_arguments",
            "array_type",
            "attribute_item",
            "enum_variant",
        ],
        // `let password = "..."` — the secrets anchor. (Magic-number
        // SCREAMING exemption reads left/name fields, which
        // let_declaration lacks, so that check is untouched.)
        assign_kinds: &["let_declaration"],
    }
}

/// A trait's width is its method count — signatures and defaulted
/// bodies alike, since an implementer answers to both. Associated
/// types and consts are not methods and do not count.
fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    if node.kind() != "trait_item" {
        return Vec::new();
    }
    let (Some(name), Some(body)) = (
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok()),
        node.child_by_field_name("body"),
    ) else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let methods = body
        .named_children(&mut cursor)
        .filter(|m| matches!(m.kind(), "function_signature_item" | "function_item"))
        .count() as u16;
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods,
    }]
}

/// A declared tuple return widens the result: `-> (A, B, C)` is 3, and
/// so is `-> Result<(A, B, C), E>` — the fallible wrapper is not one of
/// the values a caller destructures. One level only: past that the
/// nesting is a type to resolve, not a shape to read.
fn return_arity(node: Node, src: &[u8]) -> u16 {
    match node.child_by_field_name("return_type") {
        Some(t) => type_arity(t, src),
        None => 0,
    }
}

fn type_arity(t: Node, src: &[u8]) -> u16 {
    if t.kind() == "tuple_type" {
        return t.named_child_count() as u16;
    }
    if t.kind() != "generic_type" {
        return 1;
    }
    // `std::result::Result<..>` and `Result<..>` are the same wrapper.
    let base = t
        .child_by_field_name("type")
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("");
    let unwrapped = matches!(
        base.rsplit("::").next().unwrap_or(base),
        "Result" | "Option"
    );
    if !unwrapped {
        return 1;
    }
    match t
        .child_by_field_name("type_arguments")
        .and_then(|a| a.named_child(0))
    {
        Some(inner) if inner.kind() == "tuple_type" => inner.named_child_count() as u16,
        _ => 1,
    }
}

fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let mut out = Vec::new();
    if node.kind() == "use_declaration"
        && !in_cfg_test_mod(node, src)
        && let Some(arg) = node.child_by_field_name("argument")
    {
        use_edges(arg, "", src, &mut out);
    }
    out
}

/// Imports inside a `#[cfg(test)]` module are test scaffolding living in
/// a production file — they must not become architecture edges.
fn in_cfg_test_mod(node: Node, src: &[u8]) -> bool {
    let mut anc = node.parent();
    while let Some(a) = anc {
        if a.kind() == "mod_item" && preceding_attr_contains(a, src, "cfg(test") {
            return true;
        }
        anc = a.parent();
    }
    false
}

/// Does an attribute directly above the item (comments allowed between)
/// contain the needle?
fn preceding_attr_contains(item: Node, src: &[u8], needle: &str) -> bool {
    let mut prev = item.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                if p.utf8_text(src).is_ok_and(|t| t.contains(needle)) {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_named_sibling();
    }
    false
}

/// Flatten a use tree: `use a::{b, c::d as e}` yields a::b binding b and
/// a::c::d binding e.
fn use_edges(node: Node, prefix: &str, src: &[u8], out: &mut Vec<super::ImportInfo>) {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let join = |rest: &str| -> String {
        if prefix.is_empty() {
            rest.to_string()
        } else {
            format!("{prefix}::{rest}")
        }
    };
    match node.kind() {
        "identifier" | "scoped_identifier" | "crate" | "super" | "self" => {
            let full = join(text(node));
            let leaf = full.rsplit("::").next().unwrap_or("").to_string();
            out.push(super::ImportInfo {
                target: full.into(),
                names: (!leaf.is_empty())
                    .then(|| leaf.into())
                    .into_iter()
                    .collect(),
            });
        }
        "use_as_clause" => out.push(super::ImportInfo {
            target: join(node.child_by_field_name("path").map(text).unwrap_or("")).into(),
            names: node
                .child_by_field_name("alias")
                .map(|a| text(a).into())
                .into_iter()
                .collect(),
        }),
        "scoped_use_list" => {
            let deeper = join(node.child_by_field_name("path").map(text).unwrap_or(""));
            if let Some(list) = node.child_by_field_name("list") {
                use_edges(list, &deeper, src, out);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                use_edges(child, prefix, src, out);
            }
        }
        "use_wildcard" => out.push(super::ImportInfo {
            target: format!("{}::*", join(node.named_child(0).map(text).unwrap_or(""))).into(),
            names: Vec::new(),
        }),
        _ => {}
    }
}

/// `.unwrap()` / `.expect()` and the panicking macros — panics where
/// errors belonged (Rust API guidelines). `unreachable!` stays exempt:
/// it documents an invariant, not an error path.
fn panicky(call: Node, src: &[u8]) -> bool {
    if call.kind() == "macro_invocation" {
        return call
            .child_by_field_name("macro")
            .and_then(|m| m.utf8_text(src).ok())
            .is_some_and(|m| matches!(m, "panic" | "todo" | "unimplemented"));
    }
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "field_expression")
        .and_then(|f| f.child_by_field_name("field"))
        .and_then(|m| m.utf8_text(src).ok())
        .is_some_and(|m| m == "unwrap" || m == "expect")
}

/// An attribute that MAKES this function a test: `#[test]`,
/// `#[tokio::test]`, `#[rstest]`. `#[cfg(test)]` is excluded — it
/// compiles code for tests without making the code a test.
fn declares_test(node: Node, src: &[u8]) -> bool {
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let text = p.utf8_text(src).unwrap_or("");
                if text.contains("test") && !text.contains("cfg(") {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        prev = p.prev_named_sibling();
    }
    false
}

/// Test code by construction, judged by no test-quality metric: a
/// fixture builder inside a `#[cfg(test)] mod` carries no attribute of
/// its own, and judging it as a lazy assertless test was 44 findings
/// of pure noise on this very repository.
fn is_test_code(node: Node, src: &[u8]) -> bool {
    preceding_attr_contains(node, src, "test") || in_cfg_test_mod(node, src)
}

/// `transmute` reinterprets memory behind the type system's back — the
/// one call where the text reliably stops predicting the run.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && node
            .child_by_field_name("function")
            .and_then(|f| f.utf8_text(src).ok())
            .is_some_and(|t| t == "transmute" || t.ends_with("::transmute"))
}

/// Public: a bare `pub` visibility modifier. `pub(crate)`/`pub(super)` are
/// internal surface and stay unmeasured.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.named_child(0)
        .filter(|c| c.kind() == "visibility_modifier")
        .and_then(|c| c.utf8_text(src).ok())
        == Some("pub")
}

/// `///` run (one node per line) directly above the item, with attribute
/// items allowed in between; block `/** */` docs count by span.
fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {}
            "line_comment" if p.utf8_text(src).is_ok_and(|t| t.starts_with("///")) => lines += 1,
            "block_comment" if p.utf8_text(src).is_ok_and(|t| t.starts_with("/**")) => {
                lines += (p.end_position().row - p.start_position().row) as u32 + 1;
            }
            _ => break,
        }
        prev = p.prev_named_sibling();
    }
    lines
}

/// Rust has no `elif` kind: `else if` parses as `else_clause(if_expression)`.
/// Normalize to the chain shape Python gives us natively — the wrapper clause
/// turns transparent and the inner `if` becomes a flat `ElseIf`.
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
        Sem::BoolOp => match field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        _ => sem,
    }
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    match node.kind() {
        "parameter" => Some(ParamInfo {
            name: field_text_is(node, "pattern", src).unwrap_or("").into(),
            boolish: field_text_is(node, "type", src) == Some("bool"),
            typed: true,
            type_name: field_text_is(node, "type", src).unwrap_or("").into(),
            ..Default::default()
        }),
        "self_parameter" => Some(ParamInfo {
            name: "self".into(),
            selfish: true,
            mut_receiver: node.utf8_text(src).is_ok_and(|t| t.contains("mut")),
            ..Default::default()
        }),
        _ => None,
    }
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let Some(func) = call.child_by_field_name("function") else {
        return false;
    };
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    match func.kind() {
        "identifier" => text(func) == unit_name,
        // self.f(...)
        "field_expression" => {
            field_text_is(func, "field", src) == Some(unit_name)
                && func
                    .child_by_field_name("value")
                    .is_some_and(|v| v.kind() == "self")
        }
        _ => false,
    }
}
