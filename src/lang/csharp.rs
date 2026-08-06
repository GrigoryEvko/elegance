//! C#: Java's declarations plus the escape hatches Java refused.
//!
//! Two things separate it from the pack it otherwise resembles. `async`
//! and `await` are syntax rather than library, so `blocking async` and
//! `dropped tasks` are live here and dead in Java — a `.Result` on a Task
//! is the deadlock this codebase names. And a preprocessor survives, so
//! `#if` is real control flow the reader must follow, exactly as in C.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("method_declaration", Sem::FnDef),
    ("constructor_declaration", Sem::FnDef),
    ("destructor_declaration", Sem::FnDef),
    ("operator_declaration", Sem::FnDef),
    ("local_function_statement", Sem::FnDef),
    ("accessor_declaration", Sem::FnDef),
    ("lambda_expression", Sem::Lambda),
    ("anonymous_method_expression", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("interface_declaration", Sem::TypeDef),
    ("struct_declaration", Sem::TypeDef),
    ("record_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("delegate_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("conditional_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("foreach_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    ("switch_expression", Sem::Match),
    ("switch_section", Sem::CaseArm),
    ("switch_expression_arm", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("finally_clause", Sem::With),
    ("using_statement", Sem::With),
    ("lock_statement", Sem::With),
    ("binary_expression", Sem::BoolOp),
    ("invocation_expression", Sem::Call),
    ("object_creation_expression", Sem::Call),
    ("implicit_object_creation_expression", Sem::Call),
    ("cast_expression", Sem::Cast),
    ("as_expression", Sem::Cast),
    ("await_expression", Sem::Await),
    ("comment", Sem::Comment),
    ("using_directive", Sem::Import),
    ("identifier", Sem::Ident),
    ("integer_literal", Sem::NumLit),
    ("real_literal", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("verbatim_string_literal", Sem::StrLit),
    ("raw_string_literal", Sem::StrLit),
    ("interpolated_string_expression", Sem::StrLit),
    ("character_literal", Sem::StrLit),
    ("boolean_literal", Sem::BoolLit),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("return_statement", Sem::Jump),
    ("throw_statement", Sem::Jump),
    ("yield_statement", Sem::Jump),
    ("goto_statement", Sem::Goto),
    // Conditional compilation is control flow the reader must follow,
    // for the same reason it counts in C.
    ("preproc_if", Sem::If),
    ("preproc_elif", Sem::ElseIf),
    ("preproc_else", Sem::Else),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("method_declaration", "name"),
    ("constructor_declaration", "name"),
    ("local_function_statement", "name"),
    ("class_declaration", "name"),
    ("interface_declaration", "name"),
    ("struct_declaration", "name"),
    ("record_declaration", "name"),
    ("enum_declaration", "name"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("member_access_expression", "expression");

/// `object` and `dynamic` both abandon the checker; `dynamic` does it
/// loudly enough to deserve the same name.
const LOOSE: &[&str] = &["object", "dynamic", "var"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::CSharp,
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
        return_type_field: "returns",
        bool_op_field: "operator",
        types_declared: true,
        record_keys: |_, _| None,
        unguarded_resource,
        is_async,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        // `///` is the XML documentation comment.
        doc_markers: &["///"],
        is_public,
        unit_docs,
        docs_inside_body: false,
        file_level_scope: false,
        is_override,
        spooky,
        negation_operand,
        catch_sin,
        swallows_error,
        loses_context,
        panicky: |_, _| false,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/test") || p.ends_with("Tests.cs") || p.ends_with("Test.cs"),
        asserty,
        is_hook: |_, _| false,
        // One return value; a tuple return is declared as a type and
        // read from it rather than counted here.
        return_arity: |_, _| 0,
        interfaces,
        skips_test,
        magic_exempt: &["enum_declaration", "attribute"],
        assign_kinds: &["variable_declarator"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = node.utf8_text(src).unwrap_or("");
    let target = text
        .trim_start_matches("global ")
        .trim_start_matches("using")
        .trim_start_matches(" static")
        .trim()
        .trim_end_matches(';')
        .trim();
    // `using X = Y;` is an alias, and the dependency is the right side.
    let target = target.rsplit('=').next().unwrap_or(target).trim();
    if target.is_empty() {
        return Vec::new();
    }
    vec![super::ImportInfo {
        target: target.into(),
        names: Vec::new(),
    }]
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if node.kind() != "parameter" {
        return None;
    }
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    let name = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    Some(ParamInfo {
        name: name.into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text.trim_end_matches('?')),
        boolish: type_text.starts_with("bool"),
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
    Some(text.rsplit('.').next().unwrap_or(text))
}

fn is_async(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| t.trim_start().starts_with("async ") || t.contains(" async "))
}

/// Reflection and `dynamic` dispatch: after either, the text stops
/// predicting which member is reached.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "GetMethod"
                    | "GetProperty"
                    | "GetField"
                    | "GetType"
                    | "Invoke"
                    | "CreateInstance"
                    | "GetCustomAttributes"
            )
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "prefix_unary_expression" && text.starts_with('!'))
        .then(|| node.named_child(0))?
}

/// `catch (Exception)` reaches everything the runtime raises, and a
/// bare `catch` writes the same net with less down.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "catch_clause" {
        return None;
    }
    let Some(decl) = node.child_by_field_name("type").or_else(|| {
        let mut c = node.walk();
        node.named_children(&mut c)
            .find(|n| n.kind() == "catch_declaration")
    }) else {
        return Some(super::CatchSin::Broad);
    };
    let text = decl.utf8_text(src).unwrap_or("");
    (text.contains("Exception") && !text.contains("Exception.")).then_some(super::CatchSin::Broad)
}

fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return true;
    };
    let text = body.utf8_text(src).unwrap_or("").trim();
    text.trim_start_matches('{')
        .trim_end_matches('}')
        .trim()
        .is_empty()
}

/// `throw new X(...)` inside a catch, with the caught exception never
/// passed on, drops the stack that explains the failure. `throw;` alone
/// preserves it and is the correct form.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "throw_statement" {
        return false;
    }
    let text = node.utf8_text(src).unwrap_or("");
    text.contains("new ") && !text.contains(", e") && !text.contains("(e)") && in_catch(node)
}

fn in_catch(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "catch_clause" {
            return true;
        }
        if n.kind() == "method_declaration" {
            return false;
        }
        cur = n.parent();
    }
    false
}

/// A disposable created outside `using` has nothing releasing it on the
/// error path.
fn unguarded_resource(node: Node, src: &[u8]) -> bool {
    if node.kind() != "local_declaration_statement" {
        return false;
    }
    let text = node.utf8_text(src).unwrap_or("");
    let opens = [
        "new FileStream",
        "new StreamReader",
        "new StreamWriter",
        "new SqlConnection",
        "new HttpClient",
        "new MemoryStream",
    ];
    opens.iter().any(|o| text.contains(o)) && !text.trim_start().starts_with("using")
}

/// xUnit, NUnit and MSTest all mark a test with an attribute.
fn declares_test(node: Node, src: &[u8]) -> bool {
    if node.kind() != "method_declaration" {
        return false;
    }
    attrs(node, src).is_some_and(|a| {
        a.contains("[Fact]")
            || a.contains("[Theory]")
            || a.contains("[Test")
            || a.contains("[TestMethod]")
    })
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    attrs(node, src).is_some_and(|a| a.contains("Skip =") || a.contains("[Ignore"))
}

fn attrs<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "attribute_list")?
        .utf8_text(src)
        .ok()
}

fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src).is_some_and(|t| super::assertish(t) || t.starts_with("Should"))
}

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
        .filter(|c| matches!(c.kind(), "method_declaration" | "property_declaration"))
        .count();
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    let modifiers: String = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "modifier")
        .filter_map(|m| m.utf8_text(src).ok())
        .collect::<Vec<_>>()
        .join(" ");
    modifiers.contains("public") || modifiers.contains("protected")
}

/// The `///` run immediately above the declaration.
fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev.filter(|p| p.kind() == "comment") {
        let Ok(text) = p.utf8_text(src) else { break };
        if !text.starts_with("///") {
            break;
        }
        lines += text.lines().count() as u32;
        prev = p.prev_named_sibling();
    }
    lines
}

/// `&&`/`||` share the binary kind with arithmetic; `??` is a default
/// rather than a branch on truth and is deliberately excluded.
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

/// `override` and `virtual` both mark a member of the inheritance
/// contract: one supplies the default, the other replaces it.
fn is_override(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "modifier")
        .filter_map(|m| m.utf8_text(src).ok())
        .any(|m| m == "override" || m == "virtual" || m == "abstract")
}
