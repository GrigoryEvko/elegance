use tree_sitter::Node;

use super::{CatchSin, Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("lambda", Sem::Lambda),
    ("class_definition", Sem::TypeDef),
    ("await", Sem::Await),
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
/// `x = ...` again; `x += ...` is its own kind and stays exempt as a
/// collecting update.
const REASSIGNS: &[(&str, &str)] = &[("assignment", "left")];
const ATTR: (&str, &str) = ("attribute", "object");

/// `Any` says "I gave up"; bare `object` says it more politely.
const LOOSE: &[&str] = &["Any", "object"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Python,
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
        call_target_fields: &["function"],
        types_declared: true,
        refine,
        name_node: |_| None,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc,
        // Sphinx attribute docs and section banners are documentation.
        doc_markers: &["#:", "##"],
        is_public,
        doc_span,
        docs_inside_body: true,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        negation_operand: |node, _| {
            (node.kind() == "not_operator").then(|| node.child_by_field_name("argument"))?
        },
        catch_sin,
        swallows_error: |_, _| false,
        loses_context,
        panicky: |_, _| false,
        record_keys,
        is_async: super::declared_async,
        declares_test: |_, _| false,
        names_test,
        is_test_code: |_, _| false,
        test_path,
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
        skips_test,
        // An interface here is a convention — Protocol and ABC are
        // imports, not syntax — and a class body full of defs cannot
        // tell a contract from an implementation. Measured before
        // being left undone: the whole gold corpus holds nine files
        // declaring a Protocol, which is two orders of magnitude below
        // the 200-sample floor a budget needs, so the metric could only
        // ever have reported against a compiled-in default.
        interfaces: |_, _| Vec::new(),
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

/// Both spellings of the directory, and both filename conventions.
///
/// `cuda/cutlass/test/python/` is cutlass's Python suite — conftest.py,
/// run_all_tests.py, and 28 modules whose docstrings open "Tests ..." —
/// and `ts/vscode/extensions/copilot/**/test/` holds its notebook and
/// Python fixtures. Gold files 250 Python and notebook files under a
/// directory named `test`, in exactly seven trees, and every one of the
/// seven is a suite. Ruby and Lua already read `/test/`; this is the
/// three packs agreeing.
fn test_path(p: &str) -> bool {
    let file = p.rsplit('/').next().unwrap_or(p);
    file.starts_with("test_")
        || file.ends_with("_test.py")
        || p.contains("/tests/")
        || p.contains("/test/")
}

/// `@pytest.mark.skip` and `@unittest.skip` switch a test off for
/// good. `skipif` is deliberately excluded — a platform or version
/// guard is stated judgment, and the test still runs where it applies.
fn skips_test(node: Node, src: &[u8]) -> bool {
    if node.kind() != "function_definition" {
        return false;
    }
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        if p.kind() != "decorator" {
            break;
        }
        if names_a_skip(p, src) {
            return true;
        }
        prev = p.prev_named_sibling();
    }
    false
}

/// The decorator must NAME the skip, not merely contain the word.
/// `@pytest.mark.skipif` is conditional, and an ALIAS hides the
/// condition behind a name — rich binds five of them
/// (`skip_py38 = pytest.mark.skipif(...)`), and every one read as an
/// unconditional skip while this matched on substring.
fn names_a_skip(decorator: Node, src: &[u8]) -> bool {
    let text = decorator.utf8_text(src).unwrap_or("");
    let head = text
        .trim_start_matches('@')
        .split(['(', ' ', '\n'])
        .next()
        .unwrap_or("");
    head.rsplit('.').next() == Some("skip")
}

/// The widest tuple any `return` in this unit ships — `return a, b, c`
/// is the language's multi-value idiom, annotation or not — or what a
/// `-> tuple[...]` annotation declares, whichever is wider. Nested defs
/// keep their own returns: the walk stops at inner scope-formers.
fn return_arity(node: Node, src: &[u8]) -> u16 {
    let declared = node
        .child_by_field_name("return_type")
        .map_or(0, |t| annotated_arity(t, src));
    declared.max(returned_arity(node))
}

/// `-> tuple[int, str, bool]` is 3. A variadic `tuple[int, ...]` is a
/// homogeneous sequence, which is ONE value however long it runs.
fn annotated_arity(t: Node, src: &[u8]) -> u16 {
    let Some(generic) = t.named_child(0).filter(|g| g.kind() == "generic_type") else {
        return 0;
    };
    let base = generic
        .named_child(0)
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("");
    if !matches!(base, "tuple" | "Tuple") {
        return 0;
    }
    let Some(args) = generic
        .named_child(1)
        .filter(|a| a.kind() == "type_parameter")
    else {
        return 0;
    };
    let mut cursor = args.walk();
    let parts: Vec<Node> = args.named_children(&mut cursor).collect();
    match parts.iter().any(|p| p.utf8_text(src) == Ok("...")) {
        true => 1,
        false => parts.len() as u16,
    }
}

fn returned_arity(node: Node) -> u16 {
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
    if let Some(package) = imported_package(node, src) {
        return vec![super::ImportInfo {
            target: format!("{package}.*").into(),
            names: Vec::new(),
            // The file states a package, not a module: which one it
            // loads is settled at run time, so this earns an edge and
            // never a tally entry.
            reach: super::Reach::Mention,
        }];
    }
    match node.kind() {
        "import_statement" => plain_imports(node, src),
        "import_from_statement" => vec![from_import(node, src)],
        _ => Vec::new(),
    }
}

/// `import a.b, c as d` — one edge per module named.
fn plain_imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|child| one_import(child, src))
        .collect()
}

/// One clause of an `import` statement, and the single local it binds:
/// the ROOT of a dotted path, or the alias where one is written.
fn one_import(child: Node, src: &[u8]) -> Option<super::ImportInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let (target, bound) = match child.kind() {
        "dotted_name" => (child, child.named_child(0)),
        "aliased_import" => (
            child.child_by_field_name("name")?,
            child.child_by_field_name("alias"),
        ),
        _ => return None,
    };
    Some(super::ImportInfo {
        target: text(target).into(),
        names: super::binds(bound, src),
        reach: super::Reach::Anywhere,
    })
}

/// `from ..pkg import x, y as z` — ONE edge, because one module is
/// named; every local it binds rides along. The leading dots ride along
/// in the target text. A wildcard binds names nothing can enumerate, so
/// it contributes none.
fn from_import(node: Node, src: &[u8]) -> super::ImportInfo {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let module = node.child_by_field_name("module_name");
    let mut cursor = node.walk();
    let names = node
        .named_children(&mut cursor)
        .filter(|c| module.is_none_or(|m| m.id() != c.id()))
        .filter_map(|c| match c.kind() {
            "dotted_name" => Some(text(c).into()),
            "aliased_import" => c.child_by_field_name("alias").map(|a| text(a).into()),
            _ => None,
        })
        .collect();
    super::ImportInfo {
        target: module.map(text).unwrap_or("").into(),
        names,
        reach: super::Reach::Anywhere,
    }
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

/// The docstring: first statement of the body when it is a bare string.
fn doc_span(node: Node, _src: &[u8]) -> Option<(u32, u32)> {
    let body = node.child_by_field_name("body")?;
    super::node_span(body.named_child(0).filter(|first| is_doc(*first))?)
}

/// `f = lambda x: ...` is a named function in disguise — measure it as a
/// unit. `cast(T, x)` is Python's only way to overrule the checker.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::Lambda if node.parent().is_some_and(|p| p.kind() == "assignment") => Sem::FnDef,
        Sem::Call if callee_leaf(node, src) == Some("cast") => Sem::Cast,
        // `import_module(name, package)` loads a name built at run time
        // from a package the source DOES state. rich computes
        // `f".unicode{version}"` and passes `"rich._unicode_data"`,
        // whose 23 modules are the candidates.
        Sem::Call if imported_package(node, src).is_some() => Sem::Import,
        _ => sem,
    }
}

/// The package an `import_module` names, where its module argument is
/// computed and its package argument is a literal.
///
/// `rich/_unicode_data/__init__.py:90` writes
/// `import_module(f".unicode{version}", "rich._unicode_data")`. The
/// module cannot be read and the package can, so every module under it
/// is a candidate -- the directory fan-out, resolved against the
/// package the call names. 22 of Python's 28 orphans sit in that one
/// package.
fn imported_package<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    if callee_leaf(call, src)? != "import_module" {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let package = args.named_children(&mut cursor).nth(1)?;
    let text = package.utf8_text(src).ok()?.trim_matches(['"', '\'']);
    (package.kind() == "string" && !text.is_empty() && !text.starts_with('.')).then_some(text)
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
        "typed_parameter" => annotated(node, src),
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
            splat: true,
            ..Default::default()
        },
        "dictionary_splat_pattern" => ParamInfo {
            name: inner(node).into(),
            kw_splat: true,
            optional: true,
            splat: true,
            ..Default::default()
        },
        // `def f((a, b), c)` — legal in Python 2 and still parsed here,
        // because a repository old enough to write it is exactly the
        // one whose documentation drifted.
        "tuple_pattern" | "list_pattern" => ParamInfo {
            name: text(node).into(),
            destructured: true,
            ..Default::default()
        },
        _ => return None,
    };
    Some(info)
}

/// `x: int`, and the two splats written with an annotation.
///
/// An ANNOTATED splat wraps the other way round: `*args: str` is a
/// typed_parameter holding a list_splat_pattern, where the bare `*args`
/// IS the splat. Reading the typed one's first child as a name gave
/// `*args` — a name no caller can write and no documentation names —
/// and left a typed `**kwargs` unmarked as a splat at all, so `kw
/// opacity` was blind to every annotated one.
fn annotated(node: Node, src: &[u8]) -> ParamInfo {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let inner = |n: Node| n.named_child(0).map(text).unwrap_or("");
    let declared = node.child_by_field_name("type");
    let bound = node.named_child(0).filter(|c| splat_kind(*c));
    ParamInfo {
        name: bound.map_or_else(|| inner(node), inner).into(),
        boolish: declared.is_some_and(|t| text(t) == "bool"),
        selfish: matches!(inner(node), "self" | "cls"),
        kw_splat: bound.is_some_and(|b| b.kind() == "dictionary_splat_pattern"),
        optional: bound.is_some(),
        splat: bound.is_some(),
        typed: true,
        loose: declared.is_some_and(|t| super::is_loose(text(t), LOOSE)),
        type_name: declared.map(text).unwrap_or("").into(),
        ..Default::default()
    }
}

/// Is this node the splat itself — `*args` or `**kwargs`?
fn splat_kind(node: Node) -> bool {
    matches!(
        node.kind(),
        "list_splat_pattern" | "dictionary_splat_pattern"
    )
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

#[cfg(test)]
mod tests {
    use crate::lang::Lang;

    #[test]
    fn both_spellings_of_the_test_directory_are_test_code() {
        // `cuda/cutlass/test/python/` is cutlass's Python suite --
        // conftest.py, run_all_tests.py, and 28 modules whose
        // docstrings open "Tests ..." -- and vscode's copilot extension
        // files 90 notebook and 54 Python fixtures under `test/`. Gold
        // holds 250 Python and notebook files under a directory named
        // `test`, in exactly seven trees, and all seven are suites.
        let is_test = Lang::Python.pack().test_path;
        for p in [
            "cutlass/test/python/cutlass/emit/pytorch.py",
            "cutlass/operators/test/conftest.py",
            "copilot/src/platform/notebook/test/fixture.ipynb",
            "attrs/tests/test_make.py",
            "rich/tests/conftest.py",
        ] {
            assert!(is_test(p), "{p}");
        }
        for p in [
            "cutlass/python/cutlass_cppgen/emit/pytorch.py",
            "rich/rich/console.py",
            // A substring is not a segment.
            "src/latest/config.py",
        ] {
            assert!(!is_test(p), "{p}");
        }
    }
}
