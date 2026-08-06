//! Ruby: everything is an object and almost everything is a method call.
//!
//! Two facts shape what can be measured here. Blocks are the control
//! flow — `each`, `map`, `times` are calls taking a block, not loop
//! syntax — so a loop-based metric reads low the same way it does for
//! OCaml, and the block is measured as the lambda it is. And privacy is
//! a run-time toggle: `private` is a method call that changes the
//! default for everything after it, so visibility has to be tracked by
//! position within the class body rather than read off a keyword.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("method", Sem::FnDef),
    ("singleton_method", Sem::FnDef),
    ("class", Sem::TypeDef),
    ("singleton_class", Sem::TypeDef),
    ("module", Sem::TypeDef),
    ("block", Sem::Lambda),
    ("do_block", Sem::Lambda),
    ("lambda", Sem::Lambda),
    ("if", Sem::If),
    ("elsif", Sem::ElseIf),
    ("else", Sem::Else),
    ("unless", Sem::If),
    // Modifier forms: `x if y` is a branch wherever it is written.
    ("if_modifier", Sem::If),
    ("unless_modifier", Sem::If),
    ("while_modifier", Sem::Loop),
    ("until_modifier", Sem::Loop),
    ("conditional", Sem::Ternary),
    ("while", Sem::Loop),
    ("until", Sem::Loop),
    ("for", Sem::Loop),
    ("case", Sem::Match),
    ("case_match", Sem::Match),
    ("when", Sem::CaseArm),
    ("in_clause", Sem::CaseArm),
    ("begin", Sem::Try),
    ("rescue", Sem::Catch),
    ("rescue_modifier", Sem::Catch),
    ("ensure", Sem::With),
    ("binary", Sem::BoolOp),
    ("call", Sem::Call),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("constant", Sem::Ident),
    ("instance_variable", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("simple_symbol", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
    ("next", Sem::Jump),
    ("break", Sem::Jump),
    ("redo", Sem::Jump),
    ("retry", Sem::Jump),
    ("return", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("method", "name"),
    ("singleton_method", "name"),
    ("class", "name"),
    ("module", "name"),
    // A local is BORN at its first assignment — the language has no
    // declaration keyword. Without this the live map held no definition
    // row for any Ruby local, so every span read zero and the
    // repurposing check had nothing to compare a rewrite against.
    ("assignment", "left"),
];

/// `x = ...` again; `x += ...` is `operator_assignment` and stays exempt
/// as a collecting update.
const REASSIGNS: &[(&str, &str)] = &[("assignment", "left")];
const ATTR: (&str, &str) = ("call", "receiver");

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Ruby,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: Some(ATTR),
        sems,
        def_sites,
        reassigns,
        attr,
        scope_sep: "#",
        return_type_field: "",
        bool_op_field: "operator",
        call_target_fields: &["method"],
        types_declared: false,
        record_keys,
        // `File.open` with a block closes at the end of it, and that is
        // the idiom; the bare form is rare enough not to guess at.
        // Concurrency is Thread, Fiber and gems — never syntax.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["##", "#"],
        is_public,
        doc_span,
        docs_inside_body: true,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin,
        swallows_error,
        loses_context,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/spec/") || p.contains("/test/") || p.ends_with("_spec.rb"),
        asserty,
        is_hook: |_, _| false,
        // A method returns its last expression and declares nothing, so
        // returning three values is a bare array with no width to read.
        return_arity: |_, _| 0,
        // A module is a mixin, not a contract: it carries the method
        // bodies with it, so its width is an implementation's size
        // rather than the surface an implementor must satisfy.
        interfaces: |_, _| Vec::new(),
        skips_test,
        magic_exempt: &["hash"],
        assign_kinds: &["assignment", "operator_assignment"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// `require`, `require_relative` and `autoload` are the import forms.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if !requires(node, src) {
        return Vec::new();
    }
    let Some(target) = first_string(node, src) else {
        return Vec::new();
    };
    vec![super::ImportInfo {
        target: target.into(),
        // `require_relative` is a path from this file and must land on
        // one. `require` searches the load path, where a gem is an
        // ordinary answer.
        reach: match callee_text(node, src) {
            Some("require_relative") => super::Reach::Project,
            _ => super::Reach::Anywhere,
        },
        names: Vec::new(),
    }]
}

/// Ruby's parameter kinds are a taxonomy in themselves: required,
/// optional, keyword, splat and block. Keyword arguments are named at
/// the call site and are the opposite of opaque; `**opts` is the one
/// that hides what it accepts.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    // A bare identifier is a parameter only where a parameter list is
    // what encloses it; elsewhere it is any other name in the file.
    let positional = node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|p| p.kind() == "method_parameters");
    let (optional, kw_splat) = match node.kind() {
        _ if positional => (false, false),
        "optional_parameter" | "splat_parameter" | "block_parameter" => (true, false),
        // A keyword parameter with a default is optional; without one
        // it is required, and still named at every call site.
        "keyword_parameter" => (node.child_by_field_name("value").is_some(), false),
        // `**opts` is the one that hides what it accepts.
        "hash_splat_parameter" => (true, true),
        // `def each((key, value))` — the grammar has one node for it,
        // in a method head and in a block's `|(k, v)|` alike.
        "destructured_parameter" => (false, false),
        _ => return None,
    };
    let holder = node.child_by_field_name("name").unwrap_or(node);
    let text = holder.utf8_text(src).ok()?;
    Some(ParamInfo {
        name: text.trim_matches(':').into(),
        optional,
        kw_splat,
        typed: false,
        destructured: node.kind() == "destructured_parameter",
        splat: matches!(node.kind(), "splat_parameter" | "hash_splat_parameter"),
        // Nothing declares a type here, so the DEFAULT is the only
        // evidence a parameter is a switch — `def write(data,
        // dry_run: false)` says as plainly as an annotation would.
        boolish: defaults_to_a_boolean(node, src),
        ..Default::default()
    })
}

/// Does this parameter default to `true` or `false`?
fn defaults_to_a_boolean(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("value")
        .and_then(|v| v.utf8_text(src).ok())
        .is_some_and(|t| matches!(t.trim(), "true" | "false"))
}

/// Direct recursion, and `self.name` which is the same call written out.
fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('#').next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("method")?.utf8_text(src).ok()
}

fn first_string<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let s = args
        .named_children(&mut cursor)
        .find(|c| c.kind() == "string")?;
    let mut inner = s.walk();
    let content = s
        .named_children(&mut inner)
        .find(|c| c.kind() == "string_content")?;
    content.utf8_text(src).ok()
}

/// `raise` is the ordinary error path and is not a panic; `exit!` and
/// `abort` end the process without unwinding.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("abort" | "exit!"))
}

/// The metaprogramming that makes a name unfindable: text evaluated at
/// run time, methods answered by `method_missing`, constants and sends
/// assembled from strings.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "eval"
                    | "instance_eval"
                    | "class_eval"
                    | "module_eval"
                    | "define_method"
                    | "method_missing"
                    | "const_get"
                    | "const_set"
                    | "instance_variable_get"
                    | "instance_variable_set"
                    | "send"
                    | "__send__"
                    | "public_send"
                    | "binding"
            )
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    let negated = node.kind() == "unary" && (text.starts_with('!') || text.starts_with("not "));
    negated.then(|| node.child_by_field_name("operand"))?
}

/// `rescue => e` with no class catches StandardError, and bare `rescue`
/// with no class at all is the same reach with less written down.
///
/// Emptiness is asked FIRST, and answered here rather than through
/// `swallows_error` — that hook is consulted on `if` nodes, for the
/// languages where an error is a value, and Ruby's rescue never
/// reaches it. The wider sin is the one that vanishes the error.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "rescue" {
        return None;
    }
    if swallows_error(node, src) {
        return Some(super::CatchSin::Swallowed);
    }
    let has_class = node.child_by_field_name("exceptions").is_some();
    let text = node.utf8_text(src).unwrap_or("");
    // `rescue Exception` reaches past StandardError to SignalException
    // and NoMemoryError — the widest net the language offers.
    match has_class {
        false => Some(super::CatchSin::Broad),
        true if text.contains("rescue Exception") => Some(super::CatchSin::Broad),
        true => None,
    }
}

/// A rescue whose body is empty, or is only `nil`, swallows the error.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "rescue" {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return true;
    };
    let text = body.utf8_text(src).unwrap_or("").trim();
    text.is_empty() || text == "nil"
}

/// A rescue that binds the error, raises a NEW one, and never mentions
/// the original. `raise` with no arguments re-raises `$!` and keeps
/// everything; `raise Wrapped, "...: #{e.message}"` mentions the
/// binding and keeps the cause; only the third form loses it.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "rescue" {
        return false;
    }
    let Some(bound) = node
        .child_by_field_name("variable")
        .and_then(|v| v.utf8_text(src).ok())
        .map(|t| t.trim_start_matches("=>").trim())
    else {
        return false;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut stack = vec![body];
    let mut rethrows = false;
    while let Some(n) = stack.pop() {
        if n.kind() == "call" && callee_text(n, src) == Some("raise") {
            let Some(args) = n.child_by_field_name("arguments") else {
                return false;
            };
            if args.utf8_text(src).is_ok_and(|t| super::mentions(t, bound)) {
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

/// RSpec and minitest between them cover the corpus: `it`, `describe`
/// and `context` take a name and a block, and a minitest method is any
/// method whose name begins `test_`.
fn declares_test(node: Node, src: &[u8]) -> bool {
    match node.kind() {
        "method" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .is_some_and(|n| n.starts_with("test_")),
        "call" => {
            matches!(
                callee_text(node, src),
                Some("it" | "describe" | "context" | "specify" | "scenario" | "feature")
            ) && node.child_by_field_name("block").is_some()
        }
        _ => false,
    }
}

/// `xit`/`xdescribe` and `skip` are the standard disablings.
fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(node, src),
        Some("xit" | "xdescribe" | "xcontext" | "skip" | "pending")
    )
}

/// RSpec's `expect(...)`, minitest's `assert_*`, and the plain `assert`.
fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src).is_some_and(|t| super::assertish(t) || t == "expect" || t == "should")
}

/// Visibility is positional: `private` with no argument switches the
/// default for every method after it in the class body. Reading it
/// needs a walk back through the siblings, since nothing on the method
/// itself records what was in force when it was defined.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        if p.kind() == "call" || p.kind() == "identifier" {
            match p.utf8_text(src).map(str::trim) {
                Ok("private" | "protected") => return false,
                Ok("public") => return true,
                _ => {}
            }
        }
        prev = p.prev_named_sibling();
    }
    // A leading underscore is the convention for "internal anyway".
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_none_or(|n| !n.starts_with('_'))
}

/// The `#` comment run immediately above the definition.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &[], src)
}

/// A hash literal with symbol or string keys is an undeclared shape,
/// the same way an object literal is in TypeScript.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "hash" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "pair")
        .filter_map(|p| p.child_by_field_name("key"))
        .filter_map(|k| k.utf8_text(src).ok())
        .map(|k| k.trim_matches([':', '"', '\'']).into())
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// Three normalizations. `elsif` is its own kind and needs no
/// flattening. `&&`/`||`/`and`/`or` share the binary kind with every
/// arithmetic operator. And a `block` attached to `each`/`map` is the
/// language's loop, so it is measured as one rather than as a lambda —
/// otherwise Ruby would appear to contain no iteration at all.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||" | "and" | "or") => Sem::BoolOp,
            _ => Sem::None,
        },
        Sem::Lambda if iterates(node, src) => Sem::Loop,
        // Ruby's import is a CALL, and the core asks about imports at
        // `Sem::Import` nodes — so until this arm existed the pack's
        // `imports` hook was written, tested and never once asked, and
        // Ruby had no module graph at all.
        Sem::Call if requires(node, src) => Sem::Import,
        _ => sem,
    }
}

/// Is this call the import form? A RECEIVER disqualifies it: `require`
/// and `load` are Kernel methods called bare, and `config.load(path)` is
/// somebody's own method that happens to share the name.
fn requires(call: Node, src: &[u8]) -> bool {
    call.child_by_field_name("receiver").is_none()
        && matches!(
            callee_text(call, src),
            Some("require" | "require_relative" | "load")
        )
}

/// Blocks given to the iteration methods. The list is deliberately
/// short: these are the ones that mean "again for each element", and a
/// block given to `File.open` or `synchronize` genuinely is not a loop.
fn iterates(block: Node, src: &[u8]) -> bool {
    let Some(call) = block.parent().filter(|p| p.kind() == "call") else {
        return false;
    };
    matches!(
        callee_text(call, src),
        Some(
            "each"
                | "each_with_index"
                | "each_with_object"
                | "each_pair"
                | "each_key"
                | "each_value"
                | "each_slice"
                | "each_cons"
                | "each_line"
                | "each_char"
                | "map"
                | "map!"
                | "flat_map"
                | "collect"
                | "select"
                | "filter"
                | "filter_map"
                | "reject"
                | "detect"
                | "find"
                | "find_all"
                | "reduce"
                | "inject"
                | "times"
                | "upto"
                | "downto"
                | "step"
                | "sort_by"
                | "group_by"
                | "partition"
                | "count"
                | "sum"
                | "all?"
                | "any?"
                | "none?"
                | "one?"
        )
    )
}
