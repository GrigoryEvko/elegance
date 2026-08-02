use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, field_text_is, sem_table};
use crate::sem::Sem;

pub enum Dialect {
    Ts,
    Tsx,
}

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    ("generator_function_declaration", Sem::FnDef),
    ("method_definition", Sem::FnDef),
    ("await_expression", Sem::Await),
    ("arrow_function", Sem::Lambda),
    ("function_expression", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("abstract_class_declaration", Sem::TypeDef),
    ("interface_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    // `type X = {...}` declares a type as surely as `interface X`, and
    // an exported one is as much of the surface.
    ("type_alias_declaration", Sem::TypeDef),
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
    // All binary operators share one kind; refine keeps only `&&`/`||`/`??`.
    ("binary_expression", Sem::BoolOp),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("call_expression", Sem::Call),
    ("as_expression", Sem::Cast),
    ("non_null_expression", Sem::Cast),
    ("new_expression", Sem::Call),
    ("comment", Sem::Comment),
    ("import_statement", Sem::Import),
    ("identifier", Sem::Ident),
    ("property_identifier", Sem::Ident),
    ("shorthand_property_identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("number", Sem::NumLit),
    ("string", Sem::StrLit),
    ("template_string", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
];

/// `<X>expr` is a cast in .ts, but ambiguous with a JSX element in
/// .tsx — the TSX grammar has no such node at all.
const TS_ONLY: &[(&str, Sem)] = &[("type_assertion", Sem::Cast)];

const DEF_SITES: &[(&str, &str)] = &[
    ("variable_declarator", "name"),
    ("for_in_statement", "left"),
];
/// `x = ...` again; `augmented_assignment_expression` is its own kind
/// and stays exempt as a collecting update.
const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("member_expression", "object");

/// `unknown` is deliberately absent: it is the SAFE alternative to
/// `any`, forcing a narrowing before use.
const LOOSE: &[&str] = &["any", "Object", "Function"];

pub fn pack(dialect: Dialect) -> Pack {
    let (lang, ts): (Lang, tree_sitter::Language) = match dialect {
        Dialect::Ts => (
            Lang::TypeScript,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        Dialect::Tsx => (Lang::Tsx, tree_sitter_typescript::LANGUAGE_TSX.into()),
    };
    let kinds: &[&[(&str, Sem)]] = match lang {
        Lang::Tsx => &[KINDS],
        _ => &[KINDS, TS_ONLY],
    };
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang,
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
        refine,
        name_node: test_label,
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
            .then(|| node.child_by_field_name("argument"))?
        },
        record_keys,
        loses_context,
        catch_sin: |node, _| {
            // An empty catch body makes the error vanish.
            node.child_by_field_name("body")
                .is_some_and(|b| b.named_child_count() == 0)
                .then_some(super::CatchSin::Swallowed)
        },
        panicky: |_, _| false,
        swallows_error: |_, _| false,
        // No scope-guard idiom, so there is no absence to detect.
        unguarded_resource: |_, _| false,
        is_async: super::declared_async,
        declares_test: |node, src| declaring_test(node, src).is_some(),
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path,
        asserty,
        is_hook,
        return_arity,
        interfaces,
        skips_test,
        magic_exempt: &[
            "subscript_expression",
            "type_annotation",
            "decorator",
            "switch_case",
            "enum_declaration",
            "required_parameter",
            "optional_parameter",
        ],
        assign_kinds: &["variable_declarator"],
    }
}

/// An interface's width is its `method_signature` count. Property
/// signatures do not count even when function-typed: a TS interface
/// doubles as the language's record type, and billing a 20-field props
/// shape as a 20-method contract would flag every component's props.
/// Extends-clauses are composition and stay free, as in Go.
pub(super) fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    // `interface X { ... }` and `type X = { ... }` are the same
    // declaration in two spellings, and a contract does not change
    // width because its author preferred the newer keyword.
    let body = match node.kind() {
        "interface_declaration" => node.child_by_field_name("body"),
        "type_alias_declaration" => node
            .child_by_field_name("value")
            .filter(|v| v.kind() == "object_type"),
        _ => return Vec::new(),
    };
    let (Some(name), Some(body)) = (
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok()),
        body,
    ) else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let methods = body
        .named_children(&mut cursor)
        .filter(|m| m.kind() == "method_signature")
        .count() as u16;
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods,
    }]
}

/// A declared tuple return (`): [A, B, C]`) is the one place this
/// language states multi-value intent; an array VALUE alone stays a
/// single value, which is why the JS pack answers 0.
pub(super) fn return_arity(node: Node, src: &[u8]) -> u16 {
    let Some(annotation) = node.child_by_field_name("return_type") else {
        return 0;
    };
    let Some(t) = annotation.named_child(0) else {
        return 0;
    };
    if t.kind() == "tuple_type" {
        return t.named_child_count() as u16;
    }
    // `Promise<[A, B]>` is the async spelling of the same tuple, and
    // without it the entire async half of this language reads as 1.
    // Only Promise unwraps: `Array<[A, B]>` is a list OF tuples, which
    // is one value however many elements it holds.
    if t.kind() != "generic_type"
        || t.child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            != Some("Promise")
    {
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

/// `import d, { a, b as c }, * as ns from "./x"` — one edge, every
/// bound local collected.
pub(super) fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let Some(source) = node.child_by_field_name("source") else {
        return Vec::new();
    };
    let mut names: Vec<Box<str>> = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            match child.kind() {
                "import_clause" | "named_imports" => stack.push(child),
                "identifier" => names.push(text(child).into()),
                "namespace_import" => {
                    if let Some(id) = child.named_child(0) {
                        names.push(text(id).into());
                    }
                }
                "import_specifier" => {
                    let bound = child
                        .child_by_field_name("alias")
                        .or_else(|| child.child_by_field_name("name"));
                    if let Some(b) = bound {
                        names.push(text(b).into());
                    }
                }
                _ => {}
            }
        }
    }
    vec![super::ImportInfo {
        target: text(source).trim_matches(['"', '\'']).into(),
        names,
    }]
}

/// expect()/assert*() plus member styles: assert.equal, t.assertEqual.
pub(super) fn asserty(node: Node, src: &[u8]) -> bool {
    let Some(f) = node.child_by_field_name("function") else {
        return false;
    };
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    match f.kind() {
        "identifier" => {
            let n = text(f);
            n == "expect" || n.starts_with("assert")
        }
        "member_expression" => {
            f.child_by_field_name("object")
                .is_some_and(|o| text(o) == "assert")
                || f.child_by_field_name("property")
                    .is_some_and(|p| text(p).starts_with("assert") || text(p).starts_with("expect"))
        }
        _ => false,
    }
}

/// `it.skip(...)`, `describe.skip(...)`, `xit(...)` — the suite still
/// reports green and nothing records what the test would have said.
/// `it.only(...)` is deliberately absent: it silences its SIBLINGS
/// rather than itself, which is a different claim, and CI usually
/// catches it. A conditional skip is stated judgment.
pub(super) fn skips_test(node: Node, src: &[u8]) -> bool {
    let Some(f) = node.child_by_field_name("function") else {
        return false;
    };
    const RUNNERS: &[&str] = &["it", "test", "describe", "suite", "context"];
    match f.kind() {
        "identifier" => matches!(
            f.utf8_text(src),
            Ok("xit" | "xdescribe" | "xtest" | "xspecify" | "xcontext")
        ),
        "member_expression" => {
            let part = |field| {
                f.child_by_field_name(field)
                    .and_then(|n| n.utf8_text(src).ok())
            };
            part("property") == Some("skip") && part("object").is_some_and(|o| RUNNERS.contains(&o))
        }
        _ => false,
    }
}

/// A React-style hook: `useState`, `useEffect`, a custom `useThing`.
/// React identifies a hook by the ORDER it is called in, not by its
/// name — so one reached through a branch renumbers every hook after
/// it the moment the condition flips, and the component reads another
/// hook's state. The capital after `use` is what separates a hook from
/// `used()` or `useful()`, the same rule `expectish` uses.
///
/// Solid's `createSignal`/`createEffect` are deliberately absent: Solid
/// tracks dependencies at run time rather than by call order, so a
/// conditional one is legal there. The rule belongs to React's model,
/// not to JSX.
pub(super) fn is_hook(node: Node, src: &[u8]) -> bool {
    let Some(f) = node.child_by_field_name("function") else {
        return false;
    };
    // `useState(...)` or `React.useState(...)`.
    let name = match f.kind() {
        "identifier" => f.utf8_text(src).ok(),
        "member_expression" => f
            .child_by_field_name("property")
            .and_then(|p| p.utf8_text(src).ok()),
        _ => None,
    };
    name.and_then(|n| n.strip_prefix("use"))
        .is_some_and(|rest| rest.starts_with(char::is_uppercase))
}

/// eval, the Function constructor, and prototype surgery.
pub(super) fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    if sem != Sem::Call {
        return false;
    }
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    match node.child_by_field_name("function") {
        Some(f) if f.kind() == "identifier" => text(f) == "eval",
        Some(f) if f.kind() == "member_expression" => {
            // `Object.setPrototypeOf(this, X.prototype)` is the idiom
            // TypeScript MANDATES for subclassing Error when targeting
            // ES5 — prescribed boilerplate, not prototype surgery.
            let restores_own_prototype = node.child_by_field_name("arguments").is_some_and(|a| {
                a.named_child(0).is_some_and(|x| x.kind() == "this")
                    && a.named_child(1)
                        .is_some_and(|p| text(p).ends_with(".prototype"))
            });
            matches!(text(f), "Object.setPrototypeOf" | "Reflect.setPrototypeOf")
                && !restores_own_prototype
        }
        _ => {
            node.kind() == "new_expression"
                && node
                    .child_by_field_name("constructor")
                    .is_some_and(|c| text(c) == "Function")
        }
    }
}

/// Public: reachable from an `export` within a few syntactic levels, and
/// not access-restricted. Conservative on purpose.
pub(super) fn is_public(node: Node, src: &[u8]) -> bool {
    let restricted = node.named_child(0).is_some_and(|c| {
        c.kind() == "accessibility_modifier"
            && matches!(c.utf8_text(src), Ok("private" | "protected"))
    }) || node
        .child_by_field_name("name")
        .is_some_and(|n| n.kind() == "private_property_identifier");
    if restricted {
        return false;
    }
    let mut anc = Some(node);
    for _ in 0..4 {
        let Some(a) = anc else { break };
        if a.kind() == "export_statement" {
            return true;
        }
        // Methods of an exported class are surface too.
        if a.kind() == "class_declaration"
            && a.parent().is_some_and(|p| p.kind() == "export_statement")
        {
            return true;
        }
        anc = a.parent();
    }
    false
}

/// JSDoc block ending directly above the definition or its enclosing
/// declaration statement (promoted lambdas: `/** */ const f = () => ...`).
pub(super) fn unit_docs(node: Node, src: &[u8]) -> u32 {
    // Up to the export statement: arrow -> declarator -> declaration -> export.
    let mut carrier = Some(node);
    for _ in 0..4 {
        let Some(c) = carrier else { break };
        if let Some(p) = c.prev_named_sibling()
            && p.kind() == "comment"
            && p.utf8_text(src).is_ok_and(|t| t.starts_with("/**"))
            && p.end_position().row + 1 >= c.start_position().row
        {
            return (p.end_position().row - p.start_position().row) as u32 + 1;
        }
        carrier = c.parent();
    }
    0
}

/// An object literal's keys. Shorthand properties count too: `{ id, name }`
/// declares the same shape as `{ id: id, name: name }`.
pub(super) fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "object" {
        return None;
    }
    let mut keys = Vec::new();
    let mut cursor = node.walk();
    for member in node.named_children(&mut cursor) {
        let key = match member.kind() {
            "pair" => member.child_by_field_name("key")?,
            "shorthand_property_identifier" => member,
            _ => return None, // spreads and methods are not a fixed shape
        };
        let text = key.utf8_text(src).ok()?;
        keys.push(text.trim_matches(['"', '\'']).into());
    }
    Some(keys)
}

/// `catch (e) { throw new Error("...") }` drops the cause unless it is
/// forwarded — ES2022 `{ cause: e }`, or interpolated into the message.
pub(super) fn loses_context(node: Node, src: &[u8]) -> bool {
    let bound = node
        .child_by_field_name("parameter")
        .and_then(|p| p.utf8_text(src).ok());
    let Some(bound) = bound else {
        return false;
    };
    node.child_by_field_name("body")
        .is_some_and(|body| super::rethrows_without_cause(body, "throw_statement", bound, src))
}

/// Directories whose NAME means "tests live here" across the JS
/// ecosystem's runners and harnesses. Matched as whole path segments:
/// `contest/` and `latest/` are not test directories.
const TEST_DIRS: &[&str] = &[
    "__tests__",
    "tests",
    "test",
    "e2e",
    "spec",
    "cypress",
    "playwright",
];

/// Shared with the JavaScript pack. The ecosystem spells test files two
/// ways — suffix (`.test.ts`, `.spec.tsx`) and directory (`tests/`,
/// `e2e/`) — and every other pack already honored its directories.
/// Matching only suffixes left fixture credentials inside `tests/`
/// reading as production secrets.
pub(super) fn test_path(p: &str) -> bool {
    let file = p.rsplit('/').next().unwrap_or(p);
    [".test.", ".spec."].iter().any(|m| file.contains(m))
        || p.split('/').any(|seg| TEST_DIRS.contains(&seg))
}

/// Kinds whose named lambda child is really a function definition:
/// `const f = () => ...`, `f = () => ...`, `{ f: () => ... }`, class fields.
const LAMBDA_HOMES: &[&str] = &[
    "variable_declarator",
    "assignment_expression",
    "pair",
    "public_field_definition",
];

/// Functions that DECLARE a test by taking its name and its body. Every
/// JS and TS test framework in use — vitest, jest, mocha, node:test —
/// spells a test this way, so without this the entire ecosystem's tests
/// are anonymous arrows inside a call argument list and no test-quality
/// metric can ever fire for them.
///
/// `describe`/`suite` are deliberately absent. They take the same shape
/// but declare a NAMESPACE, not a test: the body is the whole suite, its
/// assertions belong to the nested tests, and counting it as a unit both
/// tripled the length p99 (ts 115 -> 189, tsx 227 -> 319) and made every
/// group read as a test that asserts nothing.
const TEST_DECLARERS: &[&str] = &["test", "it", "bench"];

/// The `test('name', () => {})` call this node is the body of, if it is.
fn declaring_test<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let args = node.parent().filter(|p| p.kind() == "arguments")?;
    let call = args.parent().filter(|c| c.kind() == "call_expression")?;
    let callee = call.child_by_field_name("function")?;
    // `test(...)` and `test.each(...)`/`it.skip(...)` alike: the leading
    // identifier is what names the framework's intent.
    let name = callee.utf8_text(src).ok()?;
    let head = name.split(['.', '(']).next()?;
    TEST_DECLARERS.contains(&head).then_some(args)
}

/// Shared with the JavaScript pack, which spells tests identically.
pub(super) fn is_declared_test(node: Node, src: &[u8]) -> bool {
    declaring_test(node, src).is_some()
}

/// A declared test's name is its first argument, which is prose rather
/// than an identifier — exactly as Zig's `test "label"` is handled.
pub(super) fn test_label(node: Node) -> Option<Node> {
    let args = node.parent().filter(|p| p.kind() == "arguments")?;
    args.named_child(0)
        .filter(|n| matches!(n.kind(), "string" | "template_string"))
}

pub(super) fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        // else if -> flat chain, exactly as in the Rust pack.
        Sem::If if node.parent().is_some_and(|p| p.kind() == "else_clause") => Sem::ElseIf,
        Sem::Else
            if node
                .named_child(0)
                .is_some_and(|c| c.kind() == "if_statement") =>
        {
            Sem::None
        }
        Sem::BoolOp => match field_text_is(node, "operator", src) {
            Some("&&" | "||" | "??") => Sem::BoolOp,
            _ => Sem::None,
        },
        Sem::Lambda
            if node
                .parent()
                .is_some_and(|p| LAMBDA_HOMES.contains(&p.kind()))
                || declaring_test(node, src).is_some() =>
        {
            Sem::FnDef
        }
        _ => sem,
    }
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    let boolish_type = |n: Node| {
        n.child_by_field_name("type")
            .and_then(|ann| ann.named_child(0))
            .and_then(|t| t.utf8_text(src).ok())
            == Some("boolean")
    };
    let boolish_default = |n: Node| {
        matches!(
            n.child_by_field_name("value").map(|v| v.kind()),
            Some("true" | "false")
        )
    };
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    match node.kind() {
        "identifier" => Some(ParamInfo {
            name: text(node).into(),
            ..Default::default()
        }),
        "required_parameter" | "optional_parameter" => Some(ParamInfo {
            name: node
                .child_by_field_name("pattern")
                .map(text)
                .unwrap_or("")
                .into(),
            boolish: boolish_type(node) || boolish_default(node),
            // `b?: number` or `b = 1` — a required_parameter with an
            // initializer is optional at every call site.
            optional: node.kind() == "optional_parameter"
                || node.child_by_field_name("value").is_some(),
            typed: node.child_by_field_name("type").is_some(),
            type_name: node
                .child_by_field_name("type")
                .map(|t| text(t).trim_start_matches(':').trim())
                .unwrap_or("")
                .into(),
            loose: node
                .child_by_field_name("type")
                .is_some_and(|t| super::is_loose(text(t), LOOSE)),
            ..Default::default()
        }),
        "rest_parameter" => Some(ParamInfo {
            name: node.named_child(0).map(text).unwrap_or("").into(),
            optional: true,
            ..Default::default()
        }),
        _ => None,
    }
}

pub(super) fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let Some(func) = call.child_by_field_name("function") else {
        return false;
    };
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    match func.kind() {
        "identifier" => text(func) == unit_name,
        // this.f(...)
        "member_expression" => {
            field_text_is(func, "property", src) == Some(unit_name)
                && func
                    .child_by_field_name("object")
                    .is_some_and(|o| o.kind() == "this")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_directories_are_segments_not_substrings() {
        let cases = [
            ("src/i18n/localeSupport.test.ts", true),
            ("src/x.spec.tsx", true),
            ("app/__tests__/y.ts", true),
            ("tests/helpers.ts", true),
            ("e2e/flow.spec.js", true),
            ("playwright/setup.ts", true),
            ("src/contest/vote.ts", false),
            ("src/latest/index.ts", false),
            ("src/protest/x.ts", false),
            ("src/attest.ts", false),
            ("src/app.ts", false),
        ];
        for (path, want) in cases {
            assert_eq!(super::test_path(path), want, "{path}");
        }
    }
}
