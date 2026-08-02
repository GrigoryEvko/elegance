//! JavaScript: the TypeScript grammar's ancestor. Kind names and hook
//! logic are shared with the TS pack; only the kind table (no type-only
//! kinds) and parameter shapes differ.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table, typescript};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    ("generator_function_declaration", Sem::FnDef),
    ("method_definition", Sem::FnDef),
    ("await_expression", Sem::Await),
    ("arrow_function", Sem::Lambda),
    ("function_expression", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("else_clause", Sem::Else),
    ("ternary_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("for_in_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    ("switch_case", Sem::CaseArm),
    ("switch_default", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("binary_expression", Sem::BoolOp),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("call_expression", Sem::Call),
    ("new_expression", Sem::Call),
    ("comment", Sem::Comment),
    ("import_statement", Sem::Import),
    ("identifier", Sem::Ident),
    ("property_identifier", Sem::Ident),
    ("shorthand_property_identifier", Sem::Ident),
    ("number", Sem::NumLit),
    ("string", Sem::StrLit),
    ("template_string", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("variable_declarator", "name"),
    ("for_in_statement", "left"),
];
/// Same shape as TypeScript: plain `=` only.
const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("member_expression", "object");

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::JavaScript,
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
        // JavaScript has no return-type syntax: the name-contract check
        // is structurally inapplicable, not merely unimplemented.
        return_type_field: "",
        bool_op_field: "operator",
        types_declared: false,
        refine: typescript::refine,
        name_node: typescript::test_label,
        composed_name: |_, _| None,
        imports: typescript::imports,
        param_info,
        is_self_call: typescript::is_self_call,
        is_doc: |_| false,
        doc_markers: &[],
        is_public: typescript::is_public,
        unit_docs: typescript::unit_docs,
        spooky: typescript::spooky,
        negation_operand: |node, src| {
            (node.kind() == "unary_expression"
                && node.utf8_text(src).is_ok_and(|t| t.starts_with('!')))
            .then(|| node.child_by_field_name("argument"))?
        },
        loses_context: typescript::loses_context,
        catch_sin: |node, _| {
            node.child_by_field_name("body")
                .is_some_and(|b| b.named_child_count() == 0)
                .then_some(super::CatchSin::Swallowed)
        },
        panicky: |_, _| false,
        swallows_error: |_, _| false,
        // No scope-guard idiom, so there is no absence to detect.
        record_keys: typescript::record_keys,
        unguarded_resource: |_, _| false,
        is_async: super::declared_async,
        declares_test: typescript::is_declared_test,
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path: typescript::test_path,
        asserty: typescript::asserty,
        is_hook: typescript::is_hook,
        // No type syntax: `return [a, b]` is one value, and the tuple
        // intent a TS annotation would state does not exist here.
        return_arity: |_, _| 0,
        // No interface declarations at all.
        interfaces: |_, _| Vec::new(),
        skips_test: typescript::skips_test,
        magic_exempt: &[
            "subscript_expression",
            "decorator",
            "switch_case",
            "assignment_pattern",
        ],
        assign_kinds: &["variable_declarator"],
    }
}

/// JS parameters: bare identifiers, defaults, rests, destructuring.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    match node.kind() {
        "identifier" => Some(ParamInfo {
            name: text(node).into(),
            ..Default::default()
        }),
        "assignment_pattern" => Some(ParamInfo {
            name: node
                .child_by_field_name("left")
                .map(text)
                .unwrap_or("")
                .into(),
            boolish: matches!(
                node.child_by_field_name("right").map(|r| r.kind()),
                Some("true" | "false")
            ),
            optional: true,
            ..Default::default()
        }),
        "rest_pattern" => Some(ParamInfo {
            name: node.named_child(0).map(text).unwrap_or("").into(),
            optional: true,
            ..Default::default()
        }),
        "object_pattern" | "array_pattern" => Some(ParamInfo {
            name: text(node).into(),
            ..Default::default()
        }),
        _ => None,
    }
}
