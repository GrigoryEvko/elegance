use tree_sitter::Node;

use super::{CatchSin, Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("lambda", Sem::Lambda),
    ("class_definition", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("elif_clause", Sem::ElseIf),
    ("else_clause", Sem::Else),
    ("conditional_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("match_statement", Sem::Match),
    ("case_clause", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("except_clause", Sem::Catch),
    ("with_statement", Sem::With),
    ("boolean_operator", Sem::BoolOp),
    ("if_clause", Sem::Filter),
    ("assert_statement", Sem::Assert),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("call", Sem::Call),
    ("comment", Sem::Comment),
    ("import_statement", Sem::Import),
    ("import_from_statement", Sem::Import),
    ("identifier", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("assignment", "left"),
    ("for_statement", "left"),
    ("named_expression", "name"),
    ("as_pattern", "alias"),
];
const ATTR: (&str, &str) = ("attribute", "object");

/// `Any` says "I gave up"; bare `object` says it more politely.
const LOOSE: &[&str] = &["Any", "object"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Python,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        attr_name: Some(ATTR),
        sems,
        def_sites,
        attr,
        scope_sep: ".",
        return_type_field: "return_type",
        bool_op_field: "operator",
        types_declared: true,
        refine,
        name_node: |_| None,
        imports,
        param_info,
        is_self_call,
        is_doc,
        // Sphinx attribute docs and section banners are documentation.
        doc_markers: &["#:", "##"],
        is_public,
        unit_docs,
        spooky,
        negation_operand: |node, _| {
            (node.kind() == "not_operator").then(|| node.child_by_field_name("argument"))?
        },
        catch_sin,
        swallows_error: |_, _| false,
        loses_context,
        panicky: |_, _| false,
        record_keys,
        unguarded_resource,
        is_async: super::declared_async,
        declares_test: |_, _| false,
        names_test,
        is_test_code: |_, _| false,
        test_path: |p| {
            let file = p.rsplit('/').next().unwrap_or(p);
            file.starts_with("test_") || file.ends_with("_test.py") || p.contains("/tests/")
        },
        asserty: |node, src| {
            node.child_by_field_name("function")
                .and_then(|f| match f.kind() {
                    "identifier" => f.utf8_text(src).ok(),
                    "attribute" => f
                        .child_by_field_name("attribute")
                        .and_then(|a| a.utf8_text(src).ok()),
                    _ => None,
                })
                .is_some_and(|n| n.starts_with("assert"))
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        return_arity,
        magic_exempt: &[
            "default_parameter",
            "typed_default_parameter",
            "subscript",
            "slice",
            "type",
            "decorator",
            "case_clause",
        ],
        assign_kinds: &["assignment"],
    }
}

/// The widest tuple any `return` in this unit ships — `return a, b, c`
/// is the language's multi-value idiom, annotation or not. Nested defs
/// keep their own returns: the walk stops at inner scope-formers.
fn return_arity(node: Node, _src: &[u8]) -> u16 {
    let mut widest = 0u16;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.id() != node.id() && matches!(n.kind(), "function_definition" | "lambda") {
            continue;
        }
        if n.kind() == "return_statement" {
            let width = match n.named_child(0) {
                Some(v) if matches!(v.kind(), "expression_list" | "tuple") => {
                    v.named_child_count() as u16
                }
                Some(_) => 1,
                None => 0,
            };
            widest = widest.max(width);
            continue;
        }
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    widest
}

/// `import a.b, c as d` and `from ..pkg import x, y as z` — one edge per
/// module; relative dots ride along in the target text.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let mut out = Vec::new();
    let mut cursor = node.walk();
    match node.kind() {
        "import_statement" => {
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    // `import a.b` binds the root `a`.
                    "dotted_name" => out.push(super::ImportInfo {
                        target: text(child).into(),
                        names: child
                            .named_child(0)
                            .map(|r| text(r).into())
                            .into_iter()
                            .collect(),
                    }),
                    "aliased_import" => out.push(super::ImportInfo {
                        target: child
                            .child_by_field_name("name")
                            .map(text)
                            .unwrap_or("")
                            .into(),
                        names: child
                            .child_by_field_name("alias")
                            .map(|a| text(a).into())
                            .into_iter()
                            .collect(),
                    }),
                    _ => {}
                }
            }
        }
        "import_from_statement" => {
            let module = node.child_by_field_name("module_name");
            let target: Box<str> = module.map(text).unwrap_or("").into();
            let mut names: Vec<Box<str>> = Vec::new();
            for child in node.named_children(&mut cursor) {
                if module.is_some_and(|m| m.id() == child.id()) {
                    continue;
                }
                match child.kind() {
                    "dotted_name" => names.push(text(child).into()),
                    "aliased_import" => {
                        if let Some(a) = child.child_by_field_name("alias") {
                            names.push(text(a).into());
                        }
                    }
                    _ => {} // wildcard binds unknowable names
                }
            }
            out.push(super::ImportInfo { target, names });
        }
        _ => {}
    }
    out
}

/// Public: conventionally-named (no underscore) and not local to a function.
fn is_public(node: Node, src: &[u8]) -> bool {
    let named_public = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| !n.starts_with('_'));
    let mut anc = node.parent();
    while let Some(a) = anc {
        if a.kind() == "function_definition" {
            return false;
        }
        anc = a.parent();
    }
    named_public
}

/// Docstring lines: first statement of the body when it is a bare string.
fn unit_docs(node: Node, _src: &[u8]) -> u32 {
    node.child_by_field_name("body")
        .and_then(|b| b.named_child(0))
        .filter(|first| is_doc(*first))
        .map_or(0, |d| {
            (d.end_position().row - d.start_position().row) as u32 + 1
        })
}

/// `f = lambda x: ...` is a named function in disguise — measure it as a
/// unit. `cast(T, x)` is Python's only way to overrule the checker.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::Lambda if node.parent().is_some_and(|p| p.kind() == "assignment") => Sem::FnDef,
        Sem::Call if callee_leaf(node, src) == Some("cast") => Sem::Cast,
        _ => sem,
    }
}

/// Trailing name of a call's target: `cast` for both `cast(..)` and
/// `typing.cast(..)`.
fn callee_leaf<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let f = call.child_by_field_name("function")?;
    match f.kind() {
        "identifier" => f.utf8_text(src).ok(),
        "attribute" => f.child_by_field_name("attribute")?.utf8_text(src).ok(),
        _ => None,
    }
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let boolish_default = |n: Option<Node>| matches!(n.map(|v| v.kind()), Some("true" | "false"));
    let boolish_type = |n: Option<Node>| n.is_some_and(|t| text(t) == "bool");
    let selfish = |name: &str| name == "self" || name == "cls";
    let named_field = |n: Node| n.child_by_field_name("name").map(text).unwrap_or("");
    let inner = |n: Node| n.named_child(0).map(text).unwrap_or("");
    let loose = |n: Option<Node>| n.is_some_and(|t| super::is_loose(text(t), LOOSE));

    let info = match node.kind() {
        "identifier" => ParamInfo {
            name: text(node).into(),
            selfish: selfish(text(node)),
            ..Default::default()
        },
        "typed_parameter" => ParamInfo {
            name: inner(node).into(),
            boolish: boolish_type(node.child_by_field_name("type")),
            selfish: selfish(inner(node)),
            typed: true,
            loose: loose(node.child_by_field_name("type")),
            type_name: node
                .child_by_field_name("type")
                .map(text)
                .unwrap_or("")
                .into(),
            ..Default::default()
        },
        "default_parameter" => ParamInfo {
            name: named_field(node).into(),
            boolish: boolish_default(node.child_by_field_name("value")),
            mutable_default: mutable_default(node),
            optional: true,
            ..Default::default()
        },
        "typed_default_parameter" => ParamInfo {
            name: named_field(node).into(),
            boolish: boolish_type(node.child_by_field_name("type"))
                || boolish_default(node.child_by_field_name("value")),
            mutable_default: mutable_default(node),
            optional: true,
            typed: true,
            loose: loose(node.child_by_field_name("type")),
            type_name: node
                .child_by_field_name("type")
                .map(text)
                .unwrap_or("")
                .into(),
            ..Default::default()
        },
        "list_splat_pattern" => ParamInfo {
            name: inner(node).into(),
            optional: true,
            ..Default::default()
        },
        "dictionary_splat_pattern" => ParamInfo {
            name: inner(node).into(),
            kw_splat: true,
            optional: true,
            ..Default::default()
        },
        _ => return None,
    };
    Some(info)
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let Some(func) = call.child_by_field_name("function") else {
        return false;
    };
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    match func.kind() {
        "identifier" => text(func) == unit_name,
        // self.f(...) / cls.f(...)
        "attribute" => {
            func.child_by_field_name("attribute")
                .is_some_and(|a| text(a) == unit_name)
                && func
                    .child_by_field_name("object")
                    .is_some_and(|o| matches!(text(o), "self" | "cls"))
        }
        _ => false,
    }
}

/// The classic footgun: a list/dict/set literal default is one shared
/// object across every call.
fn mutable_default(param: Node) -> bool {
    matches!(
        param.child_by_field_name("value").map(|v| v.kind()),
        Some("list" | "dictionary" | "set")
    )
}

/// eval/exec always; getattr-family only with a computed (non-literal)
/// attribute name; metaclasses on class definitions.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let computed_first_arg = |n: Node| {
        n.child_by_field_name("arguments")
            .and_then(|args| args.named_child(0))
            .is_some_and(is_computed)
    };
    match sem {
        Sem::Call => {
            let Some(func) = node.child_by_field_name("function") else {
                return false;
            };
            // `importlib.import_module(f"cmd_{name}")` is `__import__`
            // in its modern spelling; a literal module name is explicit.
            if func.kind() == "attribute" {
                return func
                    .child_by_field_name("attribute")
                    .is_some_and(|a| text(a) == "import_module")
                    && computed_first_arg(node);
            }
            if func.kind() != "identifier" {
                return false;
            }
            match text(func) {
                "eval" | "exec" | "__import__" | "globals" | "locals" => true,
                "import_module" => computed_first_arg(node),
                "getattr" | "setattr" | "delattr" => {
                    let Some(args) = node.child_by_field_name("arguments") else {
                        return false;
                    };
                    // A literal name is explicit; a third argument is an
                    // explicit fallback, making the lookup guarded rather
                    // than a leap in the dark; and a class that declares
                    // __getattr__ has made dynamic access its interface.
                    args.named_child(1).is_some_and(is_computed)
                        && args.named_child_count() < 3
                        && !in_dynamic_protocol(node, src)
                }
                _ => false,
            }
        }
        Sem::TypeDef => node
            .child_by_field_name("superclasses")
            .is_some_and(|args| {
                let mut cursor = args.walk();
                args.named_children(&mut cursor).any(|c| {
                    c.kind() == "keyword_argument"
                        && c.child_by_field_name("name")
                            .is_some_and(|n| text(n) == "metaclass")
                })
            }),
        _ => false,
    }
}

/// Is this argument computed rather than spelled out? An f-string
/// parses as a `string` node; its interpolation children are what make
/// it computed — `getattr(o, f"h_{x}")` is as dynamic as `getattr(o, x)`.
fn is_computed(arg: Node) -> bool {
    if arg.kind() != "string" {
        return true;
    }
    let mut cursor = arg.walk();
    arg.named_children(&mut cursor)
        .any(|c| c.kind() == "interpolation")
}

/// Is this call inside a dunder that IS the dynamic-attribute protocol?
/// `def __getattr__(self, name): return getattr(self._inner, name)` is a
/// declared proxy, not action at a distance.
fn in_dynamic_protocol(node: Node, src: &[u8]) -> bool {
    const PROTOCOL: &[&str] = &[
        "__getattr__",
        "__getattribute__",
        "__setattr__",
        "__delattr__",
    ];
    let mut anc = node.parent();
    while let Some(a) = anc {
        if a.kind() == "function_definition" {
            return a
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
                .is_some_and(|n| PROTOCOL.contains(&n));
        }
        anc = a.parent();
    }
    false
}

/// pytest's collection convention: `test`-prefixed functions. Honored
/// only inside test files (the `names_test` contract): a production
/// `test_connection` health check is neither a test to judge nor test
/// code to pardon, and a Test*-classed harness in production is a
/// harness. Test-file helpers (setUp, fixtures) are exempted by the
/// FILE — declaring them via their class judged every setUp as an
/// assertless test.
fn names_test(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| n.starts_with("test"))
}

/// `except: pass` swallows; `except Exception:`/bare except is too broad.
fn catch_sin(node: Node, src: &[u8]) -> Option<CatchSin> {
    let body = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "block")?;
    let silent = body.named_children(&mut body.walk()).all(|s| {
        s.kind() == "pass_statement"
            || (s.kind() == "expression_statement"
                && s.named_child(0).is_some_and(|e| e.kind() == "ellipsis"))
    });
    if silent {
        return Some(CatchSin::Swallowed);
    }
    let typed = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() != "block");
    match typed {
        None => Some(CatchSin::Broad),
        Some(mut t) => {
            // `Exception as e` arrives wrapped in an as_pattern.
            if t.kind() == "as_pattern"
                && let Some(inner) = t.named_child(0)
            {
                t = inner;
            }
            let text = t.utf8_text(src).unwrap_or("");
            matches!(text, "Exception" | "BaseException").then_some(CatchSin::Broad)
        }
    }
}

/// A dict literal's string keys. Only literal keys count: a dict built
/// from variables is a map, not a record wearing a dict.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "dictionary" {
        return None;
    }
    let mut keys = Vec::new();
    let mut cursor = node.walk();
    for pair in node.named_children(&mut cursor) {
        let key = pair.child_by_field_name("key")?;
        let text = key.utf8_text(src).ok()?;
        keys.push(text.trim_matches(['"', '\'']).into());
    }
    Some(keys)
}

/// `open()` outside a `with`. Closing it then becomes a promise made in
/// prose, and an early return or a raise is a path that did not keep it.
/// Assignment counts as unguarded even when a `.close()` follows: the
/// exception path is exactly the one that skips it.
fn unguarded_resource(call: Node, src: &[u8]) -> bool {
    let opens = call
        .child_by_field_name("function")
        .and_then(|f| f.utf8_text(src).ok())
        .is_some_and(|f| f == "open");
    if !opens {
        return false;
    }
    let mut anc = call.parent();
    for _ in 0..3 {
        let Some(a) = anc else { return true };
        if a.kind() == "with_statement" || a.kind() == "with_clause" || a.kind() == "with_item" {
            return false;
        }
        anc = a.parent();
    }
    true
}

/// PEP 3134: `raise Wrapped(...) from err` keeps the chain. A handler
/// that binds the error and raises without mentioning it has thrown the
/// traceback away.
fn loses_context(node: Node, src: &[u8]) -> bool {
    let bound = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "as_pattern")
        .and_then(|p| p.child_by_field_name("alias"))
        .and_then(|a| a.utf8_text(src).ok());
    let Some(bound) = bound else {
        return false; // nothing bound means nothing to forward
    };
    node.named_children(&mut node.walk())
        .find(|c| c.kind() == "block")
        .is_some_and(|body| super::rethrows_without_cause(body, "raise_statement", bound, src))
}

/// Docstring: an expression statement wrapping a bare string.
fn is_doc(node: Node) -> bool {
    node.kind() == "expression_statement"
        && node.named_child_count() == 1
        && node.named_child(0).is_some_and(|c| c.kind() == "string")
}
