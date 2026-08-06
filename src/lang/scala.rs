//! Scala: expressions all the way down, so the branch metrics count
//! things that also produce values.
//!
//! `if` is an expression and so is `match`, which means a branch here
//! can sit inside an argument list. That is idiomatic rather than
//! suspicious, and the budgets absorb it: they are pinned to what cats
//! and zio do, not to what an imperative language would.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("function_declaration", Sem::FnDef),
    ("class_definition", Sem::TypeDef),
    ("object_definition", Sem::TypeDef),
    ("trait_definition", Sem::TypeDef),
    ("enum_definition", Sem::TypeDef),
    ("type_definition", Sem::TypeDef),
    ("given_definition", Sem::TypeDef),
    ("lambda_expression", Sem::Lambda),
    ("if_expression", Sem::If),
    ("match_expression", Sem::Match),
    ("case_clause", Sem::CaseArm),
    ("for_expression", Sem::Loop),
    ("while_expression", Sem::Loop),
    ("do_while_expression", Sem::Loop),
    ("try_expression", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("finally_clause", Sem::With),
    ("infix_expression", Sem::BoolOp),
    ("call_expression", Sem::Call),
    ("instance_expression", Sem::Call),
    ("comment", Sem::Comment),
    ("block_comment", Sem::Comment),
    ("import_declaration", Sem::Import),
    ("identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("integer_literal", Sem::NumLit),
    ("floating_point_literal", Sem::NumLit),
    ("string", Sem::StrLit),
    ("interpolated_string", Sem::StrLit),
    ("character_literal", Sem::StrLit),
    ("boolean_literal", Sem::BoolLit),
    ("return_expression", Sem::Jump),
    ("throw_expression", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("function_definition", "name"),
    ("function_declaration", "name"),
    ("class_definition", "name"),
    ("object_definition", "name"),
    ("trait_definition", "name"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("field_expression", "value");

/// `Any` and `AnyRef` sit at the top of the hierarchy and say nothing;
/// `asInstanceOf` is the cast that goes with them.
const LOOSE: &[&str] = &["Any", "AnyRef", "AnyVal"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_scala::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Scala,
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
        // A case class declares the shape, and that is the idiom.
        record_keys: |_, _| None,
        unguarded_resource: |_, _| false,
        // Concurrency is Future, ZIO and cats-effect — library, and the
        // library is the whole point.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["/**"],
        is_public,
        unit_docs,
        docs_inside_body: false,
        file_level_scope: false,
        is_override,
        spooky,
        negation_operand,
        catch_sin: |_, _| None,
        swallows_error: |_, _| false,
        loses_context: |_, _| false,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| {
            p.contains("/test/") || p.ends_with("Spec.scala") || p.ends_with("Suite.scala")
        },
        asserty,
        is_hook: |_, _| false,
        return_arity: |_, _| 0,
        interfaces,
        skips_test,
        magic_exempt: &["enum_definition"],
        assign_kinds: &["val_definition", "var_definition"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = node.utf8_text(src).unwrap_or("");
    let target = text.trim_start_matches("import").trim();
    // `import foo.bar.{a, b}` — the dependency is the path before the
    // selector list.
    let target = target
        .split('{')
        .next()
        .unwrap_or(target)
        .trim_end_matches('.')
        .trim();
    if target.is_empty() {
        return Vec::new();
    }
    vec![super::ImportInfo {
        target: target.trim_end_matches("._").into(),
        names: Vec::new(),
    }]
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if !matches!(node.kind(), "parameter" | "class_parameter") {
        return None;
    }
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    let name = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    Some(ParamInfo {
        name: name.into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text),
        boolish: type_text == "Boolean",
        optional: node.child_by_field_name("default_value").is_some(),
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

/// Runtime reflection and the cast that skips the checker.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some("asInstanceOf" | "getClass" | "getDeclaredMethod" | "reflect")
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "prefix_expression" && text.starts_with('!')).then(|| node.named_child(0))?
}

/// `sys.error` and a thrown exception both leave by the same door.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("error" | "require" | "assume"))
}

/// ScalaTest and munit name a test with a string, so a declaration is
/// a call taking one.
fn declares_test(node: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(node, src),
        Some("test" | "it" | "property" | "check")
    )
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(callee_text(node, src), Some("ignore" | "pending"))
}

fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src)
        .is_some_and(|t| super::assertish(t) || matches!(t, "shouldBe" | "shouldEqual" | "expect"))
}

/// A trait carries method bodies, so only its ABSTRACT members are the
/// contract an implementor must satisfy.
fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    if node.kind() != "trait_definition" {
        return Vec::new();
    }
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let methods = body
        .named_children(&mut cursor)
        .filter(|c| matches!(c.kind(), "function_declaration" | "val_declaration"))
        .count();
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

/// Public is the DEFAULT here, so the surface is everything without an
/// access modifier saying otherwise.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    !node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "modifiers")
        .filter_map(|m| m.utf8_text(src).ok())
        .any(|m| m.contains("private") || m.contains("protected"))
}

fn unit_docs(node: Node, src: &[u8]) -> u32 {
    node.prev_named_sibling()
        .filter(|p| p.kind() == "block_comment")
        .and_then(|p| p.utf8_text(src).ok())
        .filter(|t| t.starts_with("/**"))
        .map_or(0, |t| t.lines().count() as u32)
}

/// `infix_expression` is every operator in the language, and in Scala
/// that includes every METHOD called without a dot. Only the two that
/// sequence a condition are boolean.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        Sem::If
            if node
                .parent()
                .is_some_and(|p| p.child_by_field_name("alternative") == Some(node)) =>
        {
            Sem::ElseIf
        }
        _ => sem,
    }
}

/// `override`, and a member of a trait, which carries defaults the way
/// an interface with bodies does.
fn is_override(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "modifiers")
        .filter_map(|m| m.utf8_text(src).ok())
        .any(|m| m.contains("override"))
}
