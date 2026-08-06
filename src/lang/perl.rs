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
    // `my $x = ...` is an assignment whose left side declares. Without
    // it the live map held no definition row for any lexical, so the
    // repurposing check had nothing to compare a rewrite against.
    ("assignment_expression", "left"),
];

/// `$x = ...` again. The compound operators (`+=`, `//=`) live on this
/// same node and stay exempt as collecting updates.
const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];

/// The ONLY member access an object has here. A blessed hash reaches
/// its fields with `->{k}`, whose receiver the grammar leaves unfielded,
/// and every accessor a class generates is a method — so `->` chains
/// ARE the data links Demeter is about, and there is no fluent-builder
/// spelling to confuse them with.
const ATTR: (&str, &str) = ("method_call_expression", "invocant");

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Perl,
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
        return_type_field: "",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: false,
        record_keys,
        // A filehandle closes when its lexical goes out of scope, and
        // that is the idiom; there is no scope-guard statement to miss.
        unguarded_resource: |_, _| false,
        // Future::AsyncAwait's `async sub` is the ecosystem's async and
        // the grammar reads it, so the keyword at the head decides —
        // the same rule every other language here uses.
        is_async: super::declared_async,
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
        docs_inside_body: false,
        file_level_scope: true,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin: |_, _| None,
        swallows_error,
        loses_context,
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
    node.child_by_field_name("name").or_else(|| {
        // A promoted `subtest` body takes its name from the string
        // beside it in the argument list — prose rather than an
        // identifier, the way a Zig test label is.
        let list = node.parent().filter(|p| p.kind() == "list_expression")?;
        list.named_child(0).filter(|n| n.kind() == "string_literal")
    })
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
    // Nothing declares a type, so a boolean DEFAULT is the only thing
    // in a signature that marks a parameter as a switch.
    let boolish = node
        .child_by_field_name("default")
        .and_then(|v| v.utf8_text(src).ok())
        .is_some_and(|t| matches!(t.trim(), "true" | "false"));
    let info = |name: String, optional: bool, kw_splat: bool| ParamInfo {
        name: name.into(),
        optional,
        typed: false,
        kw_splat,
        boolish,
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

/// `!$x`, and the low-precedence `not $x`. Both arrive as the generic
/// unary node — the grammar has a `logical_not_expression` kind and
/// does not use it for either spelling, which left this language with
/// no negations at all.
fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    let negated = matches!(node.kind(), "unary_expression" | "logical_not_expression")
        && (text.starts_with('!') || text.starts_with("not "));
    // The FIRST NAMED child, not the `operand` field: `!( ... )` keeps
    // its parentheses inside that field, so the field's first child is
    // the `(` token and every parenthesised negation read as one.
    negated.then(|| node.named_child(0))?
}

/// An `eval` whose error is never examined: no `$@` in the statements
/// that follow it. Deliberately shallow — two statements is what a
/// reader checks too.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "eval_expression" {
        return false;
    }
    // Only a BARE eval statement. `my $ok = eval { ... }` hands the
    // caller a value to test and `eval { ...; 1 } or do { ... }` handles
    // the failure in the same statement — both keep the error in the
    // story, and counting them put this rung-2 gate over its ceiling on
    // the gold corpus at 1.6%.
    let Some(parent) = node.parent().filter(|p| p.kind() == "expression_statement") else {
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

/// A `try { } catch ($e) { }` that raises a NEW error and never
/// mentions the one it caught. `die $e` and an interpolated `$e` both
/// keep the cause; only a fresh `die "..."` throws it away.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "try_statement" {
        return false;
    }
    let Some(bound) = node
        .child_by_field_name("catch_expr")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return false;
    };
    let Some(body) = node.child_by_field_name("catch_block") else {
        return false;
    };
    let mut stack = vec![body];
    let mut rethrows = false;
    while let Some(n) = stack.pop() {
        if callee_text(n, src).is_some_and(is_a_raise) {
            if n.utf8_text(src).is_ok_and(|t| t.contains(bound)) {
                return false;
            }
            rethrows = true;
            continue;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    rethrows
}

fn is_a_raise(name: &str) -> bool {
    matches!(name, "die" | "croak" | "confess" | "throw")
}

/// Test::More and its family. `subtest 'name' => sub { ... }` hands the
/// test to an anonymous sub, so the SUB is the unit to judge and the
/// evidence lives on the call it was passed to — the shape jest gives
/// TypeScript and busted gives Lua.
fn declaring_test<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let list = node.parent().filter(|p| p.kind() == "list_expression")?;
    let call = list.parent()?;
    matches!(callee_text(call, src), Some("subtest")).then_some(list)
}

fn declares_test(node: Node, src: &[u8]) -> bool {
    declaring_test(node, src).is_some()
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
        // The anonymous sub handed to `subtest` IS the test; without
        // promoting it there is no unit for the test metrics to judge.
        Sem::Lambda if declaring_test(node, src).is_some() => Sem::FnDef,
        Sem::BoolOp if node.kind() == "binary_expression" => {
            match super::field_text_is(node, "operator", src) {
                Some("&&" | "||" | "//") => Sem::BoolOp,
                _ => Sem::None,
            }
        }
        _ => sem,
    }
}
