//! Lua: the embedded language, measured as the host language it became.
//!
//! Lua is where configuration grew into a program — nginx routing, game
//! logic, editor plugins — and that history shows in the shape of the
//! code. There are no classes, so the class metrics are structurally
//! silent rather than merely quiet: a "method" is a function stored in a
//! table and `self` is a calling convention (`a:b()`) rather than a
//! declaration. Visibility is the same story told with `local`, which is
//! the only privacy the language has.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    ("function_definition", Sem::Lambda),
    ("if_statement", Sem::If),
    ("elseif_statement", Sem::ElseIf),
    ("else_statement", Sem::Else),
    ("for_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("repeat_statement", Sem::Loop),
    // `and`/`or` share the binary kind with arithmetic; refine keeps
    // only the two that sequence a condition.
    ("binary_expression", Sem::BoolOp),
    ("function_call", Sem::Call),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("number", Sem::NumLit),
    ("string", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
    ("break_statement", Sem::Jump),
    ("return_statement", Sem::Jump),
    // The one language here with a real `goto`, and the metric that
    // names it exists because of languages like this one.
    ("goto_statement", Sem::Goto),
];

const DEF_SITES: &[(&str, &str)] = &[("function_declaration", "name")];

/// `x = ...` again. Lua has no compound assignment, so every rebinding
/// of a name that is already bound arrives through this one kind.
const REASSIGNS: &[(&str, &str)] = &[("assignment_statement", "left")];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_lua::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Lua,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: None,
        sems,
        def_sites,
        reassigns,
        attr: None,
        scope_sep: ".",
        return_type_field: "",
        bool_op_field: "operator",
        types_declared: false,
        record_keys,
        // `pcall` is the whole error story and there is no scope guard,
        // so a resource is released by the line that remembers to.
        unguarded_resource: |_, _| false,
        // Coroutines are a library, not syntax.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        // LuaDoc and its LDoc successor both mark a doc block this way.
        doc_markers: &["---", "--[["],
        is_public,
        unit_docs,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin: |_, _| None,
        swallows_error: |_, _| false,
        loses_context: |_, _| false,
        panicky,
        declares_test,
        names_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/spec/") || p.contains("/test/") || p.ends_with("_spec.lua"),
        asserty: |call, src| callee_text(call, src).is_some_and(super::assertish),
        is_hook: |_, _| false,
        // Multiple returns are the language's ordinary idiom and carry
        // no declaration, so there is no width to read without running
        // the function.
        return_arity: |_, _| 0,
        // No interface construct: a table either answers a call or it
        // does not, and nothing writes that contract down.
        interfaces: |_, _| Vec::new(),
        skips_test,
        magic_exempt: &["table_constructor"],
        assign_kinds: &["assignment_statement"],
    }
}

/// `function M.run(...)` and `local function helper(...)` both name the
/// unit; an anonymous `function()` assigned to a field does not, and is
/// measured as the lambda it is.
fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// `require "x"` and `require("x")` are the only import form.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if callee_text(node, src) != Some("require") {
        return Vec::new();
    }
    let Some(target) = first_string(node, src) else {
        return Vec::new();
    };
    vec![super::ImportInfo {
        target: target.into(),
        names: Vec::new(),
    }]
}

/// Parameters carry no types and no defaults; `...` is the variadic
/// tail, which is opaque at the call site the way `**kwargs` is.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    match node.kind() {
        "identifier" if node.parent().is_some_and(|p| p.kind() == "parameters") => {
            Some(ParamInfo {
                name: node.utf8_text(src).ok()?.into(),
                typed: false,
                ..Default::default()
            })
        }
        // `...` is POSITIONAL varargs, the `*args` of this language.
        // Lua has no keyword arguments; the idiom is to pass a table.
        "vararg_expression" if node.parent().is_some_and(|p| p.kind() == "parameters") => {
            Some(ParamInfo {
                name: "...".into(),
                typed: false,
                ..Default::default()
            })
        }
        _ => None,
    }
}

/// Direct recursion, and the `self:helper()` form that a colon call
/// desugars into.
fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    callee_text(call, src).is_some_and(|t| t == unit_name || t.ends_with(unit_name))
}

/// The callee text of a call, trailing component only: `M.run` reads as
/// `run` so a table-stored function is recognised by its own name.
fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let name = call.child_by_field_name("name")?;
    let text = name.utf8_text(src).ok()?;
    Some(text.rsplit(['.', ':']).next().unwrap_or(text))
}

fn first_string<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let s = args
        .named_children(&mut cursor)
        .find(|c| c.kind() == "string")?;
    Some(s.utf8_text(src).ok()?.trim_matches(['"', '\'']))
}

/// `error()` is the raise, and `assert(false)` is how a contract dies.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("error"))
}

/// Text compiled at run time, and the environment swapped underneath a
/// function: after either, the source stops predicting the run.
///
/// `rawget`/`rawset` are deliberately NOT here: they bypass a metatable
/// EXPLICITLY, and an object system written by hand is where that form
/// belongs. Counting them scored Penlight's class module at 22.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some("load" | "loadstring" | "dofile" | "setfenv" | "loadfile")
        )
}

/// `not x` is the only negation operator.
fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "unary_expression" && text.trim_start().starts_with("not"))
        .then(|| node.named_child(0))?
}

/// A table literal whose keys are all names is an undeclared shape: no
/// type catches a typo in one, and adding a field means finding every
/// site by hand.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "table_constructor" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "field")
        .filter_map(|f| f.child_by_field_name("name"))
        .filter_map(|n| n.utf8_text(src).ok())
        .map(Into::into)
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// busted and its ancestors all spell a test the same way: a call to
/// `it` or `describe` taking a name and a function.
fn declares_test(node: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(node, src),
        Some("it" | "describe" | "test" | "spec")
    ) && first_string(node, src).is_some()
}

fn names_test(node: Node, src: &[u8]) -> bool {
    declares_test(node, src)
}

/// `pending("...")` is busted's skip.
fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(callee_text(node, src), Some("pending"))
}

/// `local` is the only privacy the language has, so anything not
/// declared local is reachable by whoever holds the table.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| !t.trim_start().starts_with("local"))
}

/// A run of `---` or an `--[[ ]]` block immediately above the function.
fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev.filter(|p| p.kind() == "comment") {
        let Ok(text) = p.utf8_text(src) else { break };
        if !text.starts_with("---") && !text.starts_with("--[[") {
            break;
        }
        lines += text.lines().count() as u32;
        prev = p.prev_named_sibling();
    }
    lines
}

/// Two normalizations. `elseif` is its own kind here rather than a
/// nested if, so it needs no flattening — but `and`/`or` share the
/// binary kind with every arithmetic operator, and only those two
/// sequence a condition.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("and" | "or") => Sem::BoolOp,
            _ => Sem::None,
        },
        // `return` at the tail of a chunk is the module's export, not a
        // jump out of control flow.
        Sem::Jump
            if node.kind() == "return_statement"
                && node.parent().is_some_and(|p| p.kind() == "chunk") =>
        {
            Sem::None
        }
        _ => sem,
    }
}
