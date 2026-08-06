//! Swift: optionals make the error path part of the type, so the
//! unwrap metrics read what the language was designed to prevent.
//!
//! `!` is force-unwrap, the deliberate assertion that a value is there,
//! and `try!` is the same bet on a throwing call. Both are what
//! `unwraps` counts. The grammar separates `&&` and `||` into their own
//! kinds, so the boolean sequence needs no operator lookup here.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    ("init_declaration", Sem::FnDef),
    ("deinit_declaration", Sem::FnDef),
    ("subscript_declaration", Sem::FnDef),
    ("protocol_function_declaration", Sem::FnDef),
    ("lambda_literal", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("protocol_declaration", Sem::TypeDef),
    ("typealias_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    // `guard` is the early exit: one branch that leaves.
    ("guard_statement", Sem::If),
    ("ternary_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("repeat_while_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    ("switch_entry", Sem::CaseArm),
    ("do_statement", Sem::Try),
    ("catch_block", Sem::Catch),
    // The grammar names the two boolean operators outright.
    ("conjunction_expression", Sem::BoolOp),
    ("disjunction_expression", Sem::BoolOp),
    ("call_expression", Sem::Call),
    ("as_expression", Sem::Cast),
    ("await_expression", Sem::Await),
    ("comment", Sem::Comment),
    ("multiline_comment", Sem::Comment),
    ("import_declaration", Sem::Import),
    ("simple_identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("integer_literal", Sem::NumLit),
    ("hex_literal", Sem::NumLit),
    ("oct_literal", Sem::NumLit),
    ("bin_literal", Sem::NumLit),
    ("real_literal", Sem::NumLit),
    ("line_string_literal", Sem::StrLit),
    ("multi_line_string_literal", Sem::StrLit),
    ("raw_string_literal", Sem::StrLit),
    ("boolean_literal", Sem::BoolLit),
    ("control_transfer_statement", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    // `let`/`var` is where a local is born. Without it the live map
    // held no definition row for any local, so the repurposing check
    // had nothing to compare a rewrite against.
    ("property_declaration", "name"),
    ("function_declaration", "name"),
    ("class_declaration", "name"),
    ("protocol_declaration", "name"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment", "target")];
const ATTR: (&str, &str) = ("navigation_expression", "target");

/// `Any` and `AnyObject` abandon the checker the way `object` does in
/// the languages that have one.
const LOOSE: &[&str] = &["Any", "AnyObject", "AnyHashable"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_swift::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Swift,
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
        bool_op_field: "",
        call_target_fields: &[],
        types_declared: true,
        record_keys: |_, _| None,
        // ARC releases on the last reference and `defer` covers the
        // rest, so there is no unclosed-handle shape to find.
        unguarded_resource: |_, _| false,
        is_async,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["///", "/**"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override,
        spooky,
        negation_operand,
        catch_sin,
        swallows_error,
        loses_context,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/Tests/") || p.ends_with("Tests.swift"),
        asserty,
        is_hook: |_, _| false,
        // A tuple return is declared in the type and read from there.
        return_arity: |_, _| 0,
        interfaces,
        skips_test,
        magic_exempt: &["enum_entry"],
        assign_kinds: &["property_declaration"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = node.utf8_text(src).unwrap_or("");
    let target = text.trim_start_matches("import").trim();
    if target.is_empty() {
        return Vec::new();
    }
    vec![super::ImportInfo {
        target: target.into(),
        names: Vec::new(),
    }]
}

/// Swift labels its arguments, so a parameter has an EXTERNAL name the
/// caller writes and an internal one the body uses. `_` suppresses the
/// label, and that is the form that makes a call site unreadable.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if node.kind() != "parameter" {
        return None;
    }
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    let name = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    let external = node
        .child_by_field_name("external_name")
        .and_then(|n| n.utf8_text(src).ok());
    Some(ParamInfo {
        name: name.into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text.trim_end_matches(['?', '!'])),
        boolish: type_text.starts_with("Bool"),
        optional: node.child_by_field_name("default_value").is_some() || type_text.ends_with('?'),
        // An underscored label means the call site shows a bare value.
        kw_splat: external == Some("_"),
        // `_ xs: Int...` — the ellipsis is a token of the parameter.
        splat: node
            .utf8_text(src)
            .is_ok_and(|t| t.trim_end().ends_with("...")),
        type_name: type_text.into(),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('.').next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let text = call.named_child(0)?.utf8_text(src).ok()?;
    Some(text.rsplit('.').next().unwrap_or(text).trim())
}

fn is_async(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src).is_ok_and(|t| t.contains("async"))
}

/// Reflection, and `unsafeBitCast`, which reinterprets memory the way
/// `transmute` does in Rust.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "unsafeBitCast"
                    | "withUnsafePointer"
                    | "withUnsafeMutablePointer"
                    | "perform"
                    | "value"
                    | "setValue"
            )
        )
        && node
            .utf8_text(src)
            .is_ok_and(|t| t.contains("unsafe") || t.contains("forKey"))
}

/// Swift parses `!(a && b)` as a CALL with a leading `bang` rather than
/// as a prefix expression, so both shapes have to be read.
fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    if !text.starts_with('!') {
        return None;
    }
    match node.kind() {
        "prefix_expression" => node.named_child(1),
        "call_expression" => {
            let mut cursor = node.walk();
            let suffix = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "call_suffix")?;
            let mut inner = suffix.walk();
            suffix
                .named_children(&mut inner)
                .find(|c| c.kind() == "value_arguments")?
                .named_child(0)?
                .child_by_field_name("value")
        }
        _ => None,
    }
}

/// `catch { }` with no pattern reaches every thrown error. Emptiness
/// is the wider sin and is asked first — the core consults
/// `swallows_error` on `if` nodes only, so a catch answers both here.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "catch_block" {
        return None;
    }
    if swallows_error(node, src) {
        return Some(super::CatchSin::Swallowed);
    }
    let text = node.utf8_text(src).unwrap_or("");
    let head = text.split('{').next().unwrap_or("").trim();
    (head == "catch").then_some(super::CatchSin::Broad)
}

fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_block" {
        return false;
    }
    let text = node.utf8_text(src).unwrap_or("");
    let Some(body) = text.find('{') else {
        return false;
    };
    text[body..]
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim()
        .is_empty()
}

/// A catch that throws a NEW error and never names the one it caught.
/// `catch { }` binds the error implicitly as `error`, so that is the
/// name to look for when no pattern is written.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_block" {
        return false;
    }
    let bound = node
        .child_by_field_name("error")
        .and_then(|p| p.utf8_text(src).ok())
        .map(|t| {
            t.trim_start_matches("let ")
                .trim_start_matches("var ")
                .trim()
        })
        .unwrap_or("error");
    let mut cursor = node.walk();
    let mut stack: Vec<Node> = node.named_children(&mut cursor).collect();
    let mut rethrows = false;
    while let Some(n) = stack.pop() {
        // `throw` and `return` share one kind; the keyword separates them.
        let text = n.utf8_text(src).unwrap_or("");
        if n.kind() == "control_transfer_statement" && text.starts_with("throw") {
            if super::mentions(text, bound) {
                return false;
            }
            rethrows = true;
            continue;
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    rethrows
}

/// `fatalError` stops the process, and a force-unwrap makes the same
/// bet with less ceremony.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(call, src),
        Some("fatalError" | "preconditionFailure")
    )
}

/// XCTest names a test by prefix; swift-testing marks it `@Test`.
fn declares_test(node: Node, src: &[u8]) -> bool {
    if node.kind() != "function_declaration" {
        return false;
    }
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| n.starts_with("test"))
        || node.utf8_text(src).is_ok_and(|t| t.contains("@Test"))
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| t.contains("XCTSkip") || t.contains(".disabled("))
}

fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src).is_some_and(|t| super::assertish(t) || t.starts_with("XCTAssert"))
}

/// A protocol's width is the surface an adopter must satisfy.
fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    if node.kind() != "protocol_declaration" {
        return Vec::new();
    }
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    let mut cursor = node.walk();
    let body = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "protocol_body");
    let methods = body.map_or(0, |b| {
        let mut c = b.walk();
        b.named_children(&mut c)
            .filter(|n| {
                matches!(
                    n.kind(),
                    "protocol_function_declaration" | "protocol_property_declaration"
                )
            })
            .count()
    });
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

/// `internal` is the default and is not the module's surface.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "modifiers")
        .and_then(|m| m.utf8_text(src).ok())
        .is_some_and(|m| m.contains("public") || m.contains("open"))
}

fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(
        node,
        &["comment", "multiline_comment"],
        &["///", "/**"],
        src,
    )
}

/// `else if` nests an if inside the parent's alternative and flattens
/// the same way it does everywhere else.
fn refine(node: Node, _src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::If
            if node.kind() == "if_statement"
                && node.parent().is_some_and(|p| p.kind() == "if_statement") =>
        {
            Sem::ElseIf
        }
        _ => sem,
    }
}

/// `override`, and a member of a protocol EXTENSION, which is how this
/// language spells a default implementation.
fn is_override(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    if node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "modifiers")
        .filter_map(|m| m.utf8_text(src).ok())
        .any(|m| m.contains("override"))
    {
        return true;
    }
    let mut anc = node.parent();
    while let Some(a) = anc {
        if a.kind() == "class_declaration" {
            return a
                .utf8_text(src)
                .is_ok_and(|t| t.trim_start().starts_with("extension"));
        }
        anc = a.parent();
    }
    false
}
