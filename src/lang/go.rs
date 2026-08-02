use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, field_text_is, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    ("method_declaration", Sem::FnDef),
    ("func_literal", Sem::Lambda),
    ("type_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    // Go's only loop; range loops share the kind's role.
    ("for_statement", Sem::Loop),
    ("expression_switch_statement", Sem::Match),
    ("type_switch_statement", Sem::Match),
    ("select_statement", Sem::Match),
    ("expression_case", Sem::CaseArm),
    ("type_case", Sem::CaseArm),
    ("communication_case", Sem::CaseArm),
    ("default_case", Sem::CaseArm),
    // All binary operators share one kind; refine keeps only `&&`/`||`.
    ("binary_expression", Sem::BoolOp),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    // Not Jump: break/continue are free within their loop, but a goto
    // sends the reader hunting for a label — the flat +1 Sem::Goto is for.
    ("goto_statement", Sem::Goto),
    ("call_expression", Sem::Call),
    ("type_assertion_expression", Sem::Cast),
    ("comment", Sem::Comment),
    ("import_declaration", Sem::Import),
    ("identifier", Sem::Ident),
    ("field_identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("package_identifier", Sem::Ident),
    ("int_literal", Sem::NumLit),
    ("float_literal", Sem::NumLit),
    ("interpreted_string_literal", Sem::StrLit),
    ("raw_string_literal", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("short_var_declaration", "left"),
    ("var_spec", "name"),
    ("range_clause", "left"),
];
const ATTR: (&str, &str) = ("selector_expression", "operand");

/// `interface` covers `interface{}` and `[]interface{}` alike once
/// tokenized; `any` is Go 1.18's alias for the same empty interface.
const LOOSE: &[&str] = &["interface", "any"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Go,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        attr_name: Some(ATTR),
        sems,
        def_sites,
        attr,
        scope_sep: ".",
        return_type_field: "result",
        bool_op_field: "operator",
        types_declared: true,
        refine,
        name_node: |_| None,
        imports,
        param_info,
        is_self_call,
        // Godoc comments are ordinary comments; counted as commentary.
        is_doc: |_| false,
        doc_markers: &[],
        is_public,
        unit_docs,
        spooky: |_, _, _| false,
        negation_operand: |node, src| {
            (node.kind() == "unary_expression"
                && node.utf8_text(src).is_ok_and(|t| t.starts_with('!')))
            .then(|| node.child_by_field_name("operand"))?
        },
        catch_sin: |_, _| None,
        swallows_error,
        // No exceptions, so no chain to break.
        loses_context: |_, _| false,
        panicky,
        // No async in this language; goroutines and threads are not it.
        // Recognising `defer f.Close()` needs the defer, not the open; unimplemented rather than wrong.
        // Composite literals carry their type; a map[string]any is a map, not a record.
        record_keys: |_, _| None,
        unguarded_resource: |_, _| false,
        is_async: |_, _| false,
        declares_test: |_, _| false,
        // Go's convention is names, and a name means "test" only in a
        // _test.go file — a production TestConnection is a function.
        names_test: |node, src| {
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
                .is_some_and(|n| n.starts_with("Test") || n.starts_with("Benchmark"))
        },
        is_test_code: |_, _| false,
        test_path: |p| p.ends_with("_test.go"),
        // Go's stdlib has no assert, so projects grow their own helpers
        // (esbuild: assertEqual, assertEqualStrings, assertLog) and
        // testify is called through an `assert`/`require` package name.
        // Without this the assertion metrics read 0 for every Go unit.
        asserty: |call, src| {
            let Some(f) = call.child_by_field_name("function") else {
                return false;
            };
            let text = |n: Node| n.utf8_text(src).unwrap_or("");
            // `expectXxx` too: it is Go's dominant table-test convention
            // (esbuild's tests are 4200 expectPrinted/expectParseError
            // calls against 2 files using assert* directly). The capital
            // is required, or a production `expectedValue()` would read
            // as an assertion — same rule the Zig pack uses.
            let helper = |n: &str| super::assertish(n) || super::expectish(n);
            match f.kind() {
                "identifier" => helper(text(f)),
                "selector_expression" => f
                    .child_by_field_name("operand")
                    .is_some_and(|pkg| helper(text(pkg))),
                _ => false,
            }
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        magic_exempt: &[
            "const_declaration",
            "var_declaration",
            "expression_case",
            "index_expression",
            "type_arguments",
            "array_type",
        ],
        // Both spellings: `password := "..."` and `var password = "..."`.
        assign_kinds: &["short_var_declaration", "var_spec"],
    }
}

/// `import ( alias "path/pkg" )` — the binding is the alias or the last
/// path segment; `_` and `.` imports bind nothing usable.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            match child.kind() {
                "import_spec_list" => stack.push(child),
                "import_spec" => {
                    let target = child
                        .child_by_field_name("path")
                        .map(text)
                        .unwrap_or("")
                        .trim_matches('"')
                        .to_string();
                    let name = child
                        .child_by_field_name("name")
                        .map(text)
                        .or_else(|| target.rsplit('/').next())
                        .filter(|n| *n != "_" && *n != ".");
                    out.push(super::ImportInfo {
                        names: name.map(Into::into).into_iter().collect(),
                        target: target.into(),
                    });
                }
                _ => {}
            }
        }
    }
    out
}

/// Go has no `elif` kind: `else if` nests an if_statement in the else
/// clause — same normalization as Rust and TS. `&&`/`||` disambiguated
/// from the shared binary_expression kind.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::If if node.parent().is_some_and(|p| p.kind() == "if_statement") => {
            // Go's grammar puts `else if` directly under the outer if via
            // the `alternative` field.
            Sem::ElseIf
        }
        Sem::BoolOp => match field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        // Go has no else_clause kind: a plain `else { }` is a block in the
        // if's alternative field.
        Sem::None
            if node.kind() == "block"
                && node.parent().is_some_and(|p| {
                    p.kind() == "if_statement"
                        && p.child_by_field_name("alternative")
                            .is_some_and(|alt| alt.id() == node.id())
                }) =>
        {
            Sem::Else
        }
        _ => sem,
    }
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    match node.kind() {
        "parameter_declaration" | "variadic_parameter_declaration" => Some(ParamInfo {
            name: node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
                .unwrap_or("")
                .into(),
            boolish: field_text_is(node, "type", src) == Some("bool"),
            typed: true,
            type_name: field_text_is(node, "type", src).unwrap_or("").into(),
            loose: field_text_is(node, "type", src).is_some_and(|t| super::is_loose(t, LOOSE)),
            ..Default::default()
        }),
        _ => None,
    }
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(src).ok())
        == Some(unit_name)
}

/// Go convention: exported means capitalized.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| n.starts_with(|c: char| c.is_uppercase()))
}

/// Godoc: comment run directly above the declaration.
fn unit_docs(node: Node, _src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    let mut expected_row = node.start_position().row;
    while let Some(p) = prev {
        if p.kind() != "comment" || p.end_position().row + 1 != expected_row {
            break;
        }
        lines += (p.end_position().row - p.start_position().row) as u32 + 1;
        expected_row = p.start_position().row;
        prev = p.prev_named_sibling();
    }
    lines
}

/// `if err != nil { }` — the error vanished. Go has no Catch node, so
/// without this the whole error-discipline family read zero for the
/// language whose central discipline is error handling. A comment in
/// the body is EXPLICIT silencing (Zen: "unless explicitly silenced")
/// and does not count; neither does any non-nil comparison.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    let Some(cond) = node.child_by_field_name("condition") else {
        return false;
    };
    let checks_error = cond.kind() == "binary_expression"
        && field_text_is(cond, "operator", src) == Some("!=")
        && [
            cond.child_by_field_name("left"),
            cond.child_by_field_name("right"),
        ]
        .into_iter()
        .flatten()
        .any(|side| side.utf8_text(src) == Ok("nil"));
    checks_error
        && node
            .child_by_field_name("consequence")
            .is_some_and(|b| b.named_child_count() == 0)
}

/// `panic(...)` where an error return belonged.
fn panicky(call: Node, src: &[u8]) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(src).ok())
        == Some("panic")
}
