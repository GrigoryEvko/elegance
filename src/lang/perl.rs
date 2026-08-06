//! Perl: the language that made "only perl can parse Perl" a proverb.
//!
//! That reputation is earned and it bounds what this pack can claim. A
//! sigil decides how a name is read, `/` is division or a regex
//! depending on what came before, and a source filter can rewrite the
//! file before the interpreter ever sees it. tree-sitter reads the
//! common shape of the language and gives up locally on the rest, so
//! Perl numbers are floors the way C's are — the caveat is the same one
//! and it is here for the same reason.
//!
//! Two facts make the modern language measurable in a way the 1990s one
//! was not. Signatures (`sub f ($x, $y = 1)`) declare parameters, so the
//! whole interface family works on code written since 5.20 and reads
//! nothing at all from a sub that unpacks `@_` by hand — which is itself
//! the most informative thing this pack measures about a Perl codebase's
//! age. And 5.38's `class`/`method` give real declarations where there
//! were once blessed hash references and a naming convention.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("subroutine_declaration_statement", Sem::FnDef),
    ("method_declaration_statement", Sem::FnDef),
    ("anonymous_subroutine_expression", Sem::Lambda),
    ("anonymous_method_expression", Sem::Lambda),
    ("package_statement", Sem::TypeDef),
    ("class_statement", Sem::TypeDef),
    ("role_statement", Sem::TypeDef),
    ("conditional_statement", Sem::If),
    // `return $x if $cond` — a branch written after its consequence.
    ("postfix_conditional_expression", Sem::If),
    ("elsif", Sem::ElseIf),
    ("else", Sem::Else),
    ("conditional_expression", Sem::Ternary),
    ("loop_statement", Sem::Loop),
    ("for_statement", Sem::Loop),
    ("cstyle_for_statement", Sem::Loop),
    ("postfix_for_expression", Sem::Loop),
    ("postfix_loop_expression", Sem::Loop),
    // `map`/`grep` run a block per element, which is this language's
    // iteration however it is spelled.
    ("map_grep_expression", Sem::Loop),
    ("try_statement", Sem::Try),
    // `eval { }` is the older try, and still the common one.
    ("eval_expression", Sem::Try),
    ("binary_expression", Sem::BoolOp),
    ("lowprec_logical_expression", Sem::BoolOp),
    ("function_call_expression", Sem::Call),
    ("method_call_expression", Sem::Call),
    ("ambiguous_function_call_expression", Sem::Call),
    ("coderef_call_expression", Sem::Call),
    ("func0op_call_expression", Sem::Call),
    ("func1op_call_expression", Sem::Call),
    ("comment", Sem::Comment),
    ("pod", Sem::Comment),
    ("use_statement", Sem::Import),
    ("require_expression", Sem::Import),
    ("identifier", Sem::Ident),
    ("varname", Sem::Ident),
    ("scalar", Sem::Ident),
    ("array", Sem::Ident),
    ("hash", Sem::Ident),
    ("package", Sem::Ident),
    ("bareword", Sem::Ident),
    ("number", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("interpolated_string_literal", Sem::StrLit),
    ("command_string", Sem::StrLit),
    ("boolean", Sem::BoolLit),
    ("return_expression", Sem::Jump),
    ("loopex_expression", Sem::Jump),
    ("goto_expression", Sem::Goto),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("subroutine_declaration_statement", "name"),
    ("method_declaration_statement", "name"),
    ("class_statement", "name"),
    ("package_statement", "name"),
];

/// `$x = ...` again. The compound operators (`+=`, `//=`) live on this
/// same node and stay exempt as collecting updates.
const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Perl,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: None,
        sems,
        def_sites,
        reassigns,
        attr: None,
        scope_sep: "::",
        return_type_field: "",
        bool_op_field: "operator",
        types_declared: false,
        record_keys,
        // A filehandle closes when its lexical goes out of scope, and
        // that is the idiom; there is no scope-guard statement to miss.
        unguarded_resource: |_, _| false,
        is_async: |node, _| node.kind() == "async_block_expression",
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["=head", "=pod", "##"],
        is_public,
        unit_docs,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin: |_, _| None,
        swallows_error,
        loses_context: |_, _| false,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/t/") || p.ends_with(".t") || p.contains("/xt/"),
        asserty,
        is_hook: |_, _| false,
        // A sub returns a list and declares nothing about its width.
        return_arity: |_, _| 0,
        // No interface construct. A role carries its method bodies, so
        // its width measures an implementation rather than a contract.
        interfaces: |_, _| Vec::new(),
        skips_test,
        magic_exempt: &["anonymous_hash_expression", "anonymous_array_expression"],
        assign_kinds: &["assignment_expression"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// `use Foo::Bar;` and `require Foo::Bar;`. A `use` of a pragma —
/// `strict`, `warnings`, `utf8` — turns a compiler switch on and is not
/// a dependency, so those are dropped rather than counted as edges.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let Some(text) = node
        .child_by_field_name("module")
        .and_then(|m| m.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    const PRAGMAS: &[&str] = &[
        "strict",
        "warnings",
        "utf8",
        "vars",
        "lib",
        "constant",
        "parent",
        "base",
        "feature",
        "overload",
        "integer",
        "bytes",
        "experimental",
        "builtin",
    ];
    if text.is_empty() || PRAGMAS.contains(&text) {
        return Vec::new();
    }
    vec![super::ImportInfo {
        target: text.into(),
        names: Vec::new(),
    }]
}

/// Signatures, where the code has them. A sub that unpacks `@_` by hand
/// declares nothing and correctly reads as taking no parameters — the
/// difference between the two is the age of the code.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    let name_of = |n: Node| -> Option<String> {
        let text = n
            .child_by_field_name("name")
            .unwrap_or(n)
            .utf8_text(src)
            .ok()?;
        Some(text.trim_start_matches(['$', '@', '%', ':']).to_string())
    };
    let info = |name: String, optional: bool, kw_splat: bool| ParamInfo {
        name: name.into(),
        optional,
        typed: false,
        kw_splat,
        ..Default::default()
    };
    match node.kind() {
        "mandatory_parameter" => Some(info(name_of(node)?, false, false)),
        "optional_parameter" => Some(info(name_of(node)?, true, false)),
        // `sub f (:$x)` — named at the call site, so not opaque.
        "named_parameter" => Some(info(name_of(node)?, true, false)),
        // `@rest` / `%opts` swallow whatever is left, which is exactly
        // the opacity `kw opacity` names.
        "slurpy_parameter" => Some(info(name_of(node)?, true, true)),
        _ => None,
    }
}

/// Direct recursion, and the `$self->name(...)` method form.
fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit("::").next().unwrap_or(unit_name);
    callee_text(call, src).is_some_and(|t| t == bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let named = call.child_by_field_name("function");
    let callee = match named {
        Some(f) => f,
        None => call.child_by_field_name("method")?,
    };
    let text = callee.utf8_text(src).ok()?;
    let bare = text.rsplit("::").next().unwrap_or(text);
    Some(bare.trim())
}

/// `die` is the raise and `croak` is the same from the caller's view.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(call, src),
        Some("die" | "croak" | "confess" | "exit")
    )
}

/// The constructs that make a name unfindable: text compiled at run
/// time, and a method resolved from a string.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    // `eval "string"` compiles text; `eval { }` is a try and is not
    // spooky, so the two forms are told apart by what they wrap.
    if node.kind() == "eval_expression" {
        return node.named_child(0).is_some_and(|c| c.kind() != "block");
    }
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some("AUTOLOAD" | "can" | "symbolic" | "glob")
        )
}

/// `!$x`, and the low-precedence `not $x`.
fn negation_operand<'t>(node: Node<'t>, _src: &[u8]) -> Option<Node<'t>> {
    (node.kind() == "logical_not_expression").then(|| node.child_by_field_name("operand"))?
}

/// An `eval` whose error is never examined: no `$@` in the statements
/// that follow it. Deliberately shallow — two statements is what a
/// reader checks too.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "eval_expression" {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let mut sib = parent.next_named_sibling();
    for _ in 0..2 {
        let Some(s) = sib else { break };
        if s.utf8_text(src).is_ok_and(|t| t.contains("$@")) {
            return false;
        }
        sib = s.next_named_sibling();
    }
    true
}

/// Test::More and its family. `subtest` names a group; the individual
/// assertions are counted by `asserty` rather than as declarations.
fn declares_test(node: Node, src: &[u8]) -> bool {
    matches!(callee_text(node, src), Some("subtest"))
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(callee_text(node, src), Some("skip" | "todo_skip" | "plan"))
}

fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src).is_some_and(|t| {
        super::assertish(t)
            || matches!(
                t,
                "ok" | "is"
                    | "isnt"
                    | "like"
                    | "unlike"
                    | "cmp_ok"
                    | "is_deeply"
                    | "isa_ok"
                    | "can_ok"
                    | "pass"
                    | "fail"
                    | "done_testing"
            )
    })
}

/// A leading underscore is the convention for "internal"; the language
/// enforces nothing, so convention is all there is to read.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_none_or(|n| !n.starts_with('_'))
}

/// POD immediately above the sub, or a `#` comment run. POD is usually
/// gathered at the end of the file rather than sitting above each sub,
/// so the comment run is what most code actually offers.
fn unit_docs(node: Node, src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev.filter(|p| matches!(p.kind(), "comment" | "pod")) {
        let Ok(text) = p.utf8_text(src) else { break };
        lines += text.lines().count() as u32;
        prev = p.prev_named_sibling();
    }
    lines
}

/// A hash reference literal with bareword keys is an undeclared shape:
/// nothing catches a typo in one, which is the whole reason `use strict`
/// cannot help here.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "anonymous_hash_expression" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| matches!(c.kind(), "autoquoted_bareword" | "bareword"))
        .filter_map(|k| k.utf8_text(src).ok())
        .map(Into::into)
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// `&&`/`||`/`//` share the binary kind with arithmetic, string
/// concatenation and every comparison; `and`/`or`/`not` are the
/// low-precedence spellings and arrive as their own kind.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp if node.kind() == "binary_expression" => {
            match super::field_text_is(node, "operator", src) {
                Some("&&" | "||" | "//") => Sem::BoolOp,
                _ => Sem::None,
            }
        }
        _ => sem,
    }
}
