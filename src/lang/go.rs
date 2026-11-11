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
    // sends the reader hunting for a label, which is what Sem::Goto's
    // flat +1 charges for.
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
/// `x = ...`; `:=` declares and stays a def site, and the compound
/// operators are filtered at the check by their spelled operator.
const REASSIGNS: &[(&str, &str)] = &[("assignment_statement", "left")];
const ATTR: (&str, &str) = ("selector_expression", "operand");

/// `interface` covers `interface{}` and `[]interface{}` alike once
/// tokenized; `any` is Go 1.18's alias for the same empty interface.
const LOOSE: &[&str] = &["interface", "any"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Go,
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
        return_type_field: "result",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: true,
        refine,
        name_node: |_| None,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        // Godoc comments are ordinary comments; counted as commentary.
        is_doc: |_| false,
        doc_markers: &[],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        unparsed_ctrl: |_, _| Vec::new(),
        negation_operand,
        catch_sin: |_, _| None,
        swallows_error,
        // No exceptions, so no chain to break.
        loses_context: |_, _| false,
        panicky,
        // Recognising `defer f.Close()` needs the defer, not the open;
        // unimplemented rather than wrong.
        // Composite literals carry their type; a map[string]any is a
        // map, not a record.
        record_keys: |_, _| None,
        // No async in this language; goroutines and threads are not it.
        is_async: |_, _| false,
        declares_test: |_, _| false,
        names_test,
        is_test_code: |_, _| false,
        // cmd/go excludes every path element beginning with `_`, so the
        // 20 .go files under `chi/_examples/` and `toml/_example/`
        // belong to no package. Honouring that exclusion costs more
        // than it pays: their imports are the only production evidence
        // in the repository that `chi/middleware` is used at all, and
        // dropping them turns all 30 of its files into orphans against
        // 2 recovered. Example programs are consumers, and the graph
        // reads them as such.
        test_path: |p| p.ends_with("_test.go"),
        asserty,
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        return_arity,
        interfaces,
        skips_test,
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

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    (node.kind() == "unary_expression" && node.utf8_text(src).is_ok_and(|t| t.starts_with('!')))
        .then(|| node.child_by_field_name("operand"))?
}

/// Go's convention is names, and a name means "test" only in a
/// _test.go file: a production TestConnection is a function.
fn names_test(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| n.starts_with("Test") || n.starts_with("Benchmark"))
}

/// `t.Skip()` / `t.Skipf()` / `t.SkipNow()`. Guarded skips are the
/// common and legitimate form, so the BRANCH context decides: the
/// extractor counts only unbranched ones.
fn skips_test(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("function")
        .filter(|f| f.kind() == "selector_expression")
        .and_then(|f| f.child_by_field_name("field"))
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|m| matches!(m, "Skip" | "Skipf" | "SkipNow"))
}

/// Every `interface` under one `type` declaration; a grouped
/// `type (...)` block declares several. Width counts `method_elem`
/// only: an embedded interface (`io.Reader`) is composition, the cure
/// for width, and must not be billed as the disease.
fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for spec in node.named_children(&mut cursor) {
        if spec.kind() != "type_spec" {
            continue;
        }
        let Some(body) = spec
            .child_by_field_name("type")
            .filter(|t| t.kind() == "interface_type")
        else {
            continue;
        };
        let Some(name) = spec
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
        else {
            continue;
        };
        let mut c = body.walk();
        let methods = body
            .named_children(&mut c)
            .filter(|m| m.kind() == "method_elem" || m.kind() == "method_spec")
            .count() as u16;
        out.push(crate::facts::InterfaceFact {
            name: name.into(),
            line: spec.start_position().row as u32 + 1,
            methods,
        });
    }
    out
}

/// A result list's width: `(int, error)` is 2 and so is `(a, b int)`.
/// Grouped names share one declaration, but each is a value the caller
/// must place. A single bare type is 1.
fn return_arity(node: Node, _src: &[u8]) -> u16 {
    let Some(result) = node.child_by_field_name("result") else {
        return 0;
    };
    if result.kind() != "parameter_list" {
        return 1;
    }
    let mut cursor = result.walk();
    result
        .named_children(&mut cursor)
        .filter(|d| d.kind() == "parameter_declaration")
        .map(|d| {
            let mut c = d.walk();
            let names = d
                .named_children(&mut c)
                .filter(|n| n.kind() == "identifier")
                .count() as u16;
            names.max(1)
        })
        .sum()
}

/// `import ( alias "path/pkg" )`: the binding is the alias or the last
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
                        reach: super::Reach::Anywhere,
                    });
                }
                _ => {}
            }
        }
    }
    out
}

/// Go has no `elif` kind: `else if` nests an if_statement in the else
/// clause, the same normalization as Rust and TS. `&&`/`||` is
/// disambiguated from the shared binary_expression kind.
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
            splat: node.kind() == "variadic_parameter_declaration",
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

/// Go's stdlib has no assert, so projects grow their own helpers
/// (esbuild: assertEqual, assertEqualStrings, assertLog) and testify is
/// called through an `assert`/`require` package name. Without this the
/// assertion metrics read 0 for every Go unit.
///
/// `expectXxx` counts too: it is Go's dominant table-test convention
/// (esbuild's tests are 4200 expectPrinted/expectParseError calls
/// against 2 files using assert* directly). The capital is required, or
/// a production `expectedValue()` would read as an assertion, the same
/// rule the Zig pack uses.
///
/// `require` is named on its own because `assertish` matches `assert`
/// and nothing matches `require`, testify's stop-on-first-failure half
/// and the more common one in the user's trees (224 calls against 0 in
/// gold). It is accepted as a package name only; a bare `require(...)`
/// is not a Go assertion.
///
/// The stdlib assertion is `t.Errorf(...)`. Unrecognised, it leaves 458
/// of gold's 527 Go tests — 86.9% — and 12,530 of the user's 12,640
/// reporting that they check nothing. A name rule cannot find it: the
/// same verbs are `fmt.Errorf`, `err.Error()` and `log.Fatal` in
/// production code, 66 times in gold's test files alone. The RECEIVER
/// decides, so the receiver is resolved: an identifier that some
/// enclosing function declares with a `testing.T`/`B`/`TB`/`F` parameter
/// type. That reads a subtest's own `func(t *testing.T)` correctly.
const HANDLE_FAILS: &[&str] = &["Error", "Errorf", "Fatal", "Fatalf", "Fail", "FailNow"];

fn asserty(call: Node, src: &[u8]) -> bool {
    let Some(f) = call.child_by_field_name("function") else {
        return false;
    };
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let helper = |n: &str| super::assertish(n) || super::expectish(n);
    match f.kind() {
        "identifier" => helper(text(f)),
        "selector_expression" => {
            let Some(recv) = f.child_by_field_name("operand") else {
                return false;
            };
            helper(text(recv))
                || text(recv) == "require"
                || (f
                    .child_by_field_name("field")
                    .is_some_and(|m| HANDLE_FAILS.contains(&text(m)))
                    && recv.kind() == "identifier"
                    && names_the_test_handle(call, text(recv), src))
        }
        _ => false,
    }
}

/// Is `name` bound, by an enclosing function, to the testing handle?
fn names_the_test_handle(call: Node, name: &str, src: &[u8]) -> bool {
    let mut anc = call.parent();
    while let Some(n) = anc {
        if let Some(params) = n.child_by_field_name("parameters")
            && let Some(declared) = handle_param(params, name, src)
        {
            return declared;
        }
        anc = n.parent();
    }
    false
}

/// `Some(true)` when this parameter list binds `name` to a testing type,
/// `Some(false)` when it binds `name` to something else. A nearer
/// binding wins, so a closure taking its own `t` is answered by that
/// closure and the walk stops.
fn handle_param(params: Node, name: &str, src: &[u8]) -> Option<bool> {
    let mut cursor = params.walk();
    let mut names = params.walk();
    for p in params.named_children(&mut cursor) {
        // One declaration may bind several names: `func f(a, b *testing.T)`.
        let binds = p
            .children_by_field_name("name", &mut names)
            .any(|n| n.utf8_text(src).unwrap_or("") == name);
        if !binds {
            continue;
        }
        let ty = p
            .child_by_field_name("type")
            .and_then(|t| t.utf8_text(src).ok())
            .unwrap_or("");
        return Some(ty.trim_start_matches('*').starts_with("testing."));
    }
    None
}

/// Go convention: exported means capitalized.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| n.starts_with(|c: char| c.is_uppercase()))
}

/// Godoc: comment run directly above the declaration.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &[], src)
}

/// `if err != nil { }`: the error vanished. Go has no Catch node, so
/// without this the error-discipline family reads zero for the
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

/// Where Go stops predicting its own run, in the language's own words:
/// `unsafe.Pointer` and its neighbours convert a value to a shape the
/// type system never checked, and `FieldByName`/`MethodByName` pick a
/// member by a string. go-cmp reads an unexported field with
/// `reflect.NewAt(f.Type, unsafe.Pointer(uintptr(...)+f.Offset))`,
/// Rust's `transmute` in a different alphabet.
///
/// The string decides the second one. `t.MethodByName("Equal")` names
/// its member in the source and a reader can follow it; `t.FieldByName(
/// name)` cannot be followed at all. That is the rule Python's pack
/// already applies to `import_module`, in the same words.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    if sem != Sem::Call {
        return false;
    }
    let Some(func) = node.child_by_field_name("function") else {
        return false;
    };
    let Ok(text) = func.utf8_text(src) else {
        return false;
    };
    if matches!(
        text,
        "unsafe.Pointer" | "unsafe.Slice" | "unsafe.String" | "unsafe.Add"
    ) {
        return true;
    }
    let by_name = text.ends_with(".FieldByName") || text.ends_with(".MethodByName");
    by_name
        && node
            .child_by_field_name("arguments")
            .and_then(|a| a.named_child(0))
            .is_some_and(|a| a.kind() != "interpreted_string_literal")
}

/// `panic(...)` where an error return belonged.
fn panicky(call: Node, src: &[u8]) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(src).ok())
        == Some("panic")
}
