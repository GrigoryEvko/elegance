//! PHP: the language that grew types from the outside in.
//!
//! Almost alone here it can be read at two ages at once. The same
//! codebase may hold an untyped array-shaped function from 2009 and a
//! `final readonly class` with union types and `match` from last year,
//! and both are ordinary PHP. That makes the type-hygiene family unusually
//! informative — `untyped params` is measuring a migration rather than a
//! habit — and it is why the modern corpus here is deliberately code that
//! made the move: Composer, PHPStan, PHP-Parser.
//!
//! One structural caveat. A `.php` file may hold HTML with islands of
//! code, and the grammar reads both. Text outside `<?php` is `text` and
//! contributes no units, so a template-heavy file measures as small
//! rather than as wrong.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("method_declaration", Sem::FnDef),
    ("anonymous_function", Sem::Lambda),
    ("arrow_function", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("interface_declaration", Sem::TypeDef),
    ("trait_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("else_if_clause", Sem::ElseIf),
    ("else_clause", Sem::Else),
    ("conditional_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("foreach_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    ("match_expression", Sem::Match),
    ("case_statement", Sem::CaseArm),
    // The catch-all arm is its own kind here, and leaving it unmapped
    // left every `switch` looking exhaustive to the wildcard check.
    ("default_statement", Sem::CaseArm),
    ("match_conditional_expression", Sem::CaseArm),
    ("match_default_expression", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("finally_clause", Sem::With),
    ("binary_expression", Sem::BoolOp),
    ("function_call_expression", Sem::Call),
    ("member_call_expression", Sem::Call),
    ("scoped_call_expression", Sem::Call),
    ("object_creation_expression", Sem::Call),
    ("cast_expression", Sem::Cast),
    ("comment", Sem::Comment),
    ("namespace_use_declaration", Sem::Import),
    ("name", Sem::Ident),
    ("variable_name", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("encapsed_string", Sem::StrLit),
    ("boolean", Sem::BoolLit),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("return_statement", Sem::Jump),
    ("goto_statement", Sem::Goto),
    ("throw_expression", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("function_definition", "name"),
    ("method_declaration", "name"),
    ("class_declaration", "name"),
    ("interface_declaration", "name"),
    ("trait_declaration", "name"),
    // A variable is BORN at its first assignment: there is no `var`.
    // Without this the live map held no definition row for any local,
    // so the repurposing check had nothing to compare a rewrite
    // against and every span read zero.
    ("assignment_expression", "left"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("member_access_expression", "object");

/// `mixed` is the type that says "I gave up"; `array` without a shape
/// says it more politely, and is the pre-generics idiom this language
/// spent a decade escaping.
const LOOSE: &[&str] = &["mixed", "array", "object", "iterable"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Php,
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
        call_target_fields: &["function", "name"],
        types_declared: true,
        record_keys,
        // A resource closes when the last reference drops; there is no
        // scope-guard statement whose absence would be the finding.
        // Fibers exist but concurrency is a library concern here.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc,
        // The docblock is the language's whole documentation culture,
        // and static analysers read its annotations as types.
        doc_markers: &["/**"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin,
        swallows_error,
        loses_context,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/tests/") || p.contains("/test/") || p.ends_with("Test.php"),
        asserty: |call, src| callee_text(call, src).is_some_and(super::assertish),
        is_hook: |_, _| false,
        return_arity,
        interfaces,
        skips_test,
        magic_exempt: &["array_creation_expression", "enum_declaration"],
        assign_kinds: &[
            "assignment_expression",
            "augmented_assignment_expression",
            // `const API_KEY = '...'` and `private string $token = '...'`
            // are the two places a class keeps a value under a name, and
            // both are where a pasted credential lands.
            "const_element",
            "property_element",
        ],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// `use Foo\Bar;` — the namespace path is the dependency.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "namespace_use_clause")
        .filter_map(|clause| {
            let mut inner = clause.walk();
            let name = clause
                .named_children(&mut inner)
                .find(|c| c.kind() == "qualified_name" || c.kind() == "name")?;
            let text = name.utf8_text(src).ok()?;
            Some(super::ImportInfo {
                target: text.trim_start_matches('\\').into(),
                names: Vec::new(),
                reach: super::Reach::Anywhere,
            })
        })
        .collect()
}

/// A parameter carries a declared type or it does not, and that is the
/// single most informative bit about a PHP codebase's age.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if !matches!(
        node.kind(),
        "simple_parameter" | "property_promotion_parameter" | "variadic_parameter"
    ) {
        return None;
    }
    let name = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    Some(ParamInfo {
        name: name.trim_start_matches('$').into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text.trim_start_matches('?')),
        optional: node.child_by_field_name("default_value").is_some()
            || node.kind() == "variadic_parameter",
        splat: node.kind() == "variadic_parameter",
        boolish: type_text.contains("bool"),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit("::").next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("name")
        .or_else(|| call.child_by_field_name("function"))?
        .utf8_text(src)
        .ok()
}

/// `eval`, variable functions and variable variables: after any of
/// them the text stops predicting which code runs.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    if node.kind() == "variable_variable_expression" {
        return true;
    }
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "eval"
                    | "extract"
                    | "compact"
                    | "call_user_func"
                    | "call_user_func_array"
                    | "create_function"
                    | "assert"
                    | "__call"
                    | "__get"
                    | "__set"
            )
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "unary_op_expression" && text.starts_with('!')).then(|| node.named_child(0))?
}

/// `catch (\Throwable $e)` is the widest net the language has, and
/// `\Exception` is the one below it.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "catch_clause" {
        return None;
    }
    // Emptiness is the wider sin and is asked first. It is answered
    // here rather than through `swallows_error`, which the core
    // consults on `if` nodes for the languages where an error is a
    // value — a catch clause never reaches it.
    if swallows_error(node, src) {
        return Some(super::CatchSin::Swallowed);
    }
    let types = node.child_by_field_name("type")?;
    let text = types.utf8_text(src).ok()?;
    let broad = text.contains("Throwable") || text.trim_matches('\\') == "Exception";
    broad.then_some(super::CatchSin::Broad)
}

/// `die()` and `exit()` stop the process where an error belonged: no
/// caller gets a chance to answer, and nothing above sees why.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("die" | "exit"))
}

/// A catch that binds the error, throws a NEW one, and never mentions
/// the original: the chain that explains WHY is gone, and `getPrevious`
/// has nothing to return.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    let Some(bound) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return false;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    super::rethrows_without_cause(body, "throw_expression", bound.trim_start_matches('$'), src)
}

/// A catch whose body is empty silences the error entirely.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    node.child_by_field_name("body")
        .and_then(|b| b.utf8_text(src).ok())
        .is_some_and(|t| t.trim().trim_matches(['{', '}']).trim().is_empty())
}

/// PHPUnit: a test is a method named `test*` or one carrying the
/// `#[Test]` attribute or `@test` annotation.
fn declares_test(node: Node, src: &[u8]) -> bool {
    if node.kind() != "method_declaration" {
        return false;
    }
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| n.starts_with("test"))
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(node, src),
        Some("markTestSkipped" | "markTestIncomplete")
    )
}

/// A declared return type of `array` says a list comes back but not how
/// wide, so only a tuple-shaped docblock could answer it — and that is
/// an annotation, not the language. Nothing to read.
fn return_arity(_node: Node, _src: &[u8]) -> u16 {
    0
}

/// An interface's width is its declared method count — the one place
/// here where the contract is written down separately from any
/// implementation, which is exactly what the metric wants to measure.
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
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let methods = body
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "method_declaration")
        .count();
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

/// `private` and `protected` are real here, unlike in most of the
/// scripting languages this pack sits beside.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    let modifier = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "visibility_modifier")
        .and_then(|m| m.utf8_text(src).ok());
    !matches!(modifier, Some("private" | "protected"))
}

fn is_doc(node: Node) -> bool {
    node.kind() == "comment"
}

/// The `/** ... */` docblock immediately above the declaration.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &["/**"], src)
}

/// `&&`/`||`/`and`/`or` share the binary kind with arithmetic and every
/// comparison; only those four sequence a condition. `??` is a default,
/// not a branch on truth, and is deliberately excluded.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||" | "and" | "or" | "xor") => Sem::BoolOp,
            _ => Sem::None,
        },
        _ => sem,
    }
}

/// An array literal with string keys is PHP's record, and the shape it
/// declares is nothing at all — the idiom the language spent a decade
/// replacing with typed properties.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "array_creation_expression" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "array_element_initializer")
        .filter_map(|e| e.named_child(0))
        .filter(|k| matches!(k.kind(), "string" | "encapsed_string"))
        .filter_map(|k| k.utf8_text(src).ok())
        .map(|k| k.trim_matches(['"', '\'']).into())
        .collect();
    (!keys.is_empty()).then_some(keys)
}
