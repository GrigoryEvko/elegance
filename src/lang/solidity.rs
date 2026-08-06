//! Solidity: the one language here where a finding is directly
//! financial, and the metrics are read accordingly.
//!
//! Two things are specific to it. Inline assembly (`assembly { ... }`,
//! Yul) drops beneath the type system and the checked-arithmetic rules
//! both, so it is `spooky` for the same reason `unsafe` and
//! `transmute` are. And visibility is written on every function because
//! the compiler demands it, which makes `public docs` a question about
//! an interface the whole world can call rather than one a package can.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("constructor_definition", Sem::FnDef),
    ("modifier_definition", Sem::FnDef),
    ("fallback_receive_definition", Sem::FnDef),
    ("yul_function_definition", Sem::FnDef),
    ("contract_declaration", Sem::TypeDef),
    ("interface_declaration", Sem::TypeDef),
    ("library_declaration", Sem::TypeDef),
    ("struct_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("error_declaration", Sem::TypeDef),
    ("event_definition", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("yul_if_statement", Sem::If),
    ("ternary_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_while_statement", Sem::Loop),
    ("yul_for_statement", Sem::Loop),
    ("yul_switch_statement", Sem::Match),
    ("try_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("binary_expression", Sem::BoolOp),
    ("call_expression", Sem::Call),
    ("yul_function_call", Sem::Call),
    ("new_expression", Sem::Call),
    ("type_cast_expression", Sem::Cast),
    ("comment", Sem::Comment),
    ("import_directive", Sem::Import),
    ("identifier", Sem::Ident),
    ("yul_identifier", Sem::Ident),
    ("number_literal", Sem::NumLit),
    ("yul_decimal_number", Sem::NumLit),
    ("yul_hex_number", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("hex_string_literal", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
    ("return_statement", Sem::Jump),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("revert_statement", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("function_definition", "name"),
    ("contract_declaration", "name"),
    ("interface_declaration", "name"),
    ("library_declaration", "name"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("member_expression", "object");

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_solidity::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Solidity,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: Some(ATTR),
        sems,
        def_sites,
        reassigns,
        attr,
        scope_sep: ".",
        return_type_field: "return_type",
        bool_op_field: "operator",
        types_declared: true,
        record_keys: |_, _| None,
        // There is no handle to leak: a contract's state is storage and
        // outlives every call into it.
        unguarded_resource: |_, _| false,
        // Execution is single-threaded and atomic per transaction.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["///", "/**"],
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
        test_path: |p| p.contains("/test/") || p.ends_with(".t.sol"),
        asserty,
        is_hook: |_, _| false,
        return_arity,
        interfaces,
        skips_test: |_, _| false,
        magic_exempt: &["enum_declaration"],
        assign_kinds: &[
            "variable_declaration_statement",
            "state_variable_declaration",
            "constant_variable_declaration",
        ],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let Some(target) = node
        .child_by_field_name("source")
        .and_then(|s| s.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    vec![super::ImportInfo {
        target: target.trim_matches(['"', '\'']).into(),
        names: Vec::new(),
    }]
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if node.kind() != "parameter" {
        return None;
    }
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    // A parameter may be typed and unnamed — `function f(uint256)` is
    // legal and common in interfaces.
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("");
    Some(ParamInfo {
        name: name.into(),
        typed: ty.is_some(),
        boolish: type_text == "bool",
        // `bytes memory` and `bytes calldata` carry arbitrary encoded
        // data, so what they accept is not written down anywhere.
        loose: type_text.starts_with("bytes"),
        type_name: type_text.into(),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('.').next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let f = call.child_by_field_name("function")?;
    let text = f.utf8_text(src).ok()?;
    Some(text.rsplit('.').next().unwrap_or(text).trim())
}

/// Inline assembly leaves both the type system and the overflow checks
/// behind, and a low-level call returns success as a boolean the caller
/// must remember to read. `delegatecall` runs another contract's code
/// against this contract's storage.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    if node.kind() == "assembly_statement" {
        return true;
    }
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some("delegatecall" | "callcode" | "selfdestruct" | "create2")
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "unary_expression" && text.starts_with('!')).then(|| node.named_child(0))?
}

/// `revert` unwinds the transaction, and `require` is the guard that
/// calls it. Both are the ordinary control flow of a contract rather
/// than a failure, so neither is a panic — `assert` is, because it
/// signals an invariant the code believed could not break.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("assert"))
}

/// An empty `catch` after an external call means the failure of another
/// contract is being ignored.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    node.child_by_field_name("body").is_none_or(|b| {
        b.utf8_text(src)
            .unwrap_or("")
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim()
            .is_empty()
    })
}

/// Forge names a test by prefix, which is how the runner finds it.
fn declares_test(node: Node, src: &[u8]) -> bool {
    node.kind() == "function_definition"
        && node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .is_some_and(|n| n.starts_with("test") || n.starts_with("invariant_"))
}

fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src)
        .is_some_and(|t| super::assertish(t) || t.starts_with("assertEq") || t == "require")
}

/// A declared return list has a width the caller must destructure.
fn return_arity(node: Node, _src: &[u8]) -> u16 {
    let Some(returns) = node.child_by_field_name("return_type") else {
        return 0;
    };
    let mut cursor = returns.walk();
    returns
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "parameter")
        .count() as u16
}

/// An interface here is a genuine contract: it has no bodies at all,
/// because the language forbids them.
fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    if node.kind() != "interface_declaration" {
        return Vec::new();
    }
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    let mut cursor = node.walk();
    let methods = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "contract_body")
        .map_or(0, |b| {
            let mut inner = b.walk();
            b.named_children(&mut inner)
                .filter(|c| c.kind() == "function_definition")
                .count()
        });
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

/// The compiler requires a visibility keyword, so this reads a decision
/// the author had to make rather than a default they inherited.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "visibility")
        .filter_map(|v| v.utf8_text(src).ok())
        .any(|v| v == "public" || v == "external")
}

fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev.filter(|p| p.kind() == "comment") {
        let Ok(text) = p.utf8_text(src) else { break };
        if !text.starts_with("///") && !text.starts_with("/**") {
            break;
        }
        lines += text.lines().count() as u32;
        prev = p.prev_named_sibling();
    }
    lines
}

fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        Sem::If
            if node
                .parent()
                .is_some_and(|p| p.child_by_field_name("else") == Some(node)) =>
        {
            Sem::ElseIf
        }
        _ => sem,
    }
}
