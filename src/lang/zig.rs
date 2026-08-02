//! Zig: the TigerStyle language. No exceptions, no hidden control flow —
//! errors are values, `catch` is an expression, and assertion culture is
//! the point (std.debug.assert feeds the asserts metric).

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, field_text_is, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    // `test "label" { .. }` blocks are first-class units.
    ("test_declaration", Sem::FnDef),
    ("struct_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("union_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("if_expression", Sem::If),
    ("else_clause", Sem::Else),
    ("while_statement", Sem::Loop),
    ("while_expression", Sem::Loop),
    ("for_statement", Sem::Loop),
    ("for_expression", Sem::Loop),
    ("switch_expression", Sem::Match),
    ("switch_case", Sem::CaseArm),
    // Error handling is an expression; a catch still branches.
    ("catch_expression", Sem::Catch),
    ("defer_statement", Sem::With),
    ("errdefer_statement", Sem::With),
    ("comptime_statement", Sem::With),
    ("comptime_declaration", Sem::With),
    // `and`/`or` keywords arrive via binary_expression; refine filters.
    ("binary_expression", Sem::BoolOp),
    ("break_expression", Sem::Jump),
    ("continue_expression", Sem::Jump),
    ("call_expression", Sem::Call),
    // @builtin(...) calls; refine turns @import into Sem::Import.
    ("builtin_function", Sem::Call),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("builtin_identifier", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("multiline_string", Sem::StrLit),
    ("character", Sem::StrLit),
    ("boolean", Sem::BoolLit),
];

// The grammar exposes no name field on variable_declaration, so
// live-span tracking is off for Zig rather than wrong.
const DEF_SITES: &[(&str, &str)] = &[];
const ATTR: (&str, &str) = ("field_expression", "object");

/// `anytype` defers the type to the call site: comptime-checked, but
/// the signature states nothing.
const LOOSE: &[&str] = &["anytype"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_zig::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Zig,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        attr_name: Some(ATTR),
        sems,
        def_sites,
        attr,
        scope_sep: ".",
        return_type_field: "type",
        bool_op_field: "operator",
        types_declared: true,
        refine,
        // `test "label" { .. }`: the prose label names the unit.
        name_node: |node| {
            (node.kind() == "test_declaration").then(|| {
                let mut cursor = node.walk();
                node.named_children(&mut cursor)
                    .find(|c| matches!(c.kind(), "string" | "identifier" | "builtin_identifier"))
            })?
        },
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &[],
        is_public,
        unit_docs,
        spooky: |_, _, _| false,
        negation_operand: |node, src| {
            (node.kind() == "unary_expression" && field_text_is(node, "operator", src) == Some("!"))
                .then(|| node.child_by_field_name("argument"))?
        },
        catch_sin: |_, _| None,
        swallows_error: |_, _| false,
        // No exceptions, so no chain to break.
        loses_context: |_, _| false,
        panicky: |call, src| {
            builtin_name(call, src) == Some("@panic")
                || call
                    .child_by_field_name("function")
                    .and_then(|f| f.utf8_text(src).ok())
                    .is_some_and(|t| t.ends_with("panic"))
        },
        // No async in this language; goroutines and threads are not it.
        // Same as Go: the guard is a `defer` elsewhere in the block.
        // Anonymous struct literals are inferred against a declared type.
        record_keys: |_, _| None,
        unguarded_resource: |_, _| false,
        is_async: |_, _| false,
        declares_test: |node, _| node.kind() == "test_declaration",
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path: |p| p.ends_with("_test.zig") || p.contains("/test/"),
        // std.debug.assert, wrapper assert_* fns, std.testing.expect and
        // its expectXxx family. Not `expected_value()`-style lookalikes.
        asserty: |call, src| {
            call.child_by_field_name("function")
                .and_then(|f| f.utf8_text(src).ok())
                .map(|t| t.rsplit('.').next().unwrap_or(t))
                .is_some_and(|seg| {
                    seg == "assert" || seg.starts_with("assert_") || super::expectish(seg)
                })
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        // Multiple values come back as a named struct — the remedy the
        // metric would recommend, already applied by the language.
        return_arity: |_, _| 0,
        magic_exempt: &[
            "variable_declaration",
            "switch_case",
            "index_expression",
            "array_type",
        ],
        // `const password = "..."` — the secrets anchor. The grammar
        // fields nothing here; bound_name and assigns_to fall back to
        // the identifier child and the last named child.
        assign_kinds: &["variable_declaration"],
    }
}

/// `const std = @import("std");` — refine reclassifies the call; the
/// binding is the declaration's leading identifier.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let mut cursor = node.walk();
    let args = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "arguments");
    let target = args.and_then(|a| {
        let mut cursor = a.walk();
        a.named_children(&mut cursor).find(|c| c.kind() == "string")
    });
    let Some(target) = target else {
        return Vec::new();
    };
    let name = node
        .parent()
        .filter(|p| p.kind() == "variable_declaration")
        .and_then(|p| {
            let mut cursor = p.walk();
            p.named_children(&mut cursor)
                .find(|c| c.kind() == "identifier")
        });
    vec![super::ImportInfo {
        target: text(target).trim_matches('"').into(),
        names: name.map(|n| text(n).into()).into_iter().collect(),
    }]
}

fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        // else-if chains flatten, as in Rust/TS/Go.
        Sem::If if node.parent().is_some_and(|p| p.kind() == "else_clause") => Sem::ElseIf,
        Sem::Else
            if node
                .named_child(0)
                .is_some_and(|c| c.kind().starts_with("if_")) =>
        {
            Sem::None
        }
        Sem::BoolOp => match field_text_is(node, "operator", src) {
            Some("and" | "or" | "orelse") => Sem::BoolOp,
            _ => Sem::None,
        },
        // defer/errdefer/comptime indent only in block form; expression
        // forms (`defer x.deinit();`, `comptime T == u8`) add no depth.
        Sem::With if !has_block_child(node) => Sem::None,
        // @import is the module system, not a call.
        Sem::Call if builtin_name(node, src) == Some("@import") => Sem::Import,
        // The @xCast family: every one overrules the type checker.
        Sem::Call if builtin_name(node, src).is_some_and(is_cast_builtin) => Sem::Cast,
        _ => sem,
    }
}

/// The `@name` of a builtin_function node (its identifier is unfielded).
fn builtin_name<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    if node.kind() != "builtin_function" {
        return None;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "builtin_identifier")?
        .utf8_text(src)
        .ok()
}

/// `@intCast`, `@ptrCast`, `@bitCast`, `@enumFromInt`, ... — Zig spells
/// each conversion out, which is exactly why they are countable.
fn is_cast_builtin(name: &str) -> bool {
    name.ends_with("Cast") || name.contains("FromInt") || name.contains("FromPtr")
}

fn has_block_child(node: Node) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|c| c.kind() == "block")
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    (node.kind() == "parameter").then(|| ParamInfo {
        name: field_text_is(node, "name", src).unwrap_or("").into(),
        boolish: field_text_is(node, "type", src) == Some("bool"),
        typed: true,
        type_name: field_text_is(node, "type", src).unwrap_or("").into(),
        loose: field_text_is(node, "type", src).is_some_and(|t| super::is_loose(t, LOOSE)),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(src).ok())
        == Some(unit_name)
}

/// Zig: `pub fn` — a leading `pub` keyword on the declaration.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| t.starts_with("pub ") || t.starts_with("pub\n"))
}

/// Doc comments: `///` runs directly above (comment kind is unified).
fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        if p.kind() != "comment" || !p.utf8_text(src).is_ok_and(|t| t.starts_with("///")) {
            break;
        }
        lines += 1;
        prev = p.prev_named_sibling();
    }
    lines
}
