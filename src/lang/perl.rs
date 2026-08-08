//! Perl: the language that made "only perl can parse Perl" a proverb.
//!
//! That reputation is earned and it bounds what this pack can claim. A
//! sigil decides how a name is read, `/` is division or a regex
//! depending on what came before, and a source filter can rewrite the
//! file before the interpreter ever sees it. tree-sitter reads the
//! common shape of the language and gives up locally on the rest, so
//! Perl numbers are floors the way C's are, and the caveat is the same
//! one for the same reason.
//!
//! Signatures (`sub f ($x, $y = 1)`) declare parameters, so the whole
//! interface family works on code written since 5.20 and reads nothing
//! at all from a sub that unpacks `@_` by hand, which is itself the most
//! informative thing this pack measures about a Perl codebase's age.
//! 5.38's `class`/`method` give real declarations where there were once
//! blessed hash references and a naming convention.

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
    // `return $x if $cond`: a branch written after its consequence.
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
    // The file itself is asked once, for the namespaces its strings
    // name. Asked of the FILE rather than of each string so a namespace
    // name stays a StrLit for the secret, repetition and clone checks.
    // Promoting the string would cost it whatever the table said it
    // was. See `namespaces`.
    ("source_file", Sem::Import),
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
/// and every accessor a class generates is a method, so `->` chains ARE
/// the data links Demeter is about, and there is no fluent-builder
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
        // Future::AsyncAwait's `async sub` is the ecosystem's async and
        // the grammar reads it, so the keyword at the head decides, the
        // same rule every other language here uses.
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
        doc_span,
        docs_inside_body: false,
        file_level_scope: true,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin: |_, _| None,
        swallows_error,
        loses_context,
        // die/croak/confess ARE Perl's exception vocabulary, not a
        // panic beside it: `unwraps` is declared dead here.
        panicky: |_, _| false,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        // `t/` and `xt/` are what a CPAN distribution writes, and `.t`
        // is the file extension for a test script. `tests/` is what a
        // project in ANOTHER language writes when its harness happens to
        // be Perl: curl keeps `runtests.pl` there — `#!/usr/bin/env
        // perl`, documented at docs/runtests.md — with the FTP, HTTP,
        // HTTP/2 and HTTP/3 servers it starts, the per-case
        // `libtest/test*.pl`, and `appveyor.pm`/`azure.pm`/
        // `directories.pm` beside them.
        //
        // Every Perl-family file under a `tests` component in the 22
        // gold corpora: 53 in curl and 18 in dune, and dune's are all
        // `.t`, which the rule above already matched. So this reads
        // curl's harness and nothing else in the corpus.
        //
        // `/test/` singular is NOT here. It would catch vscode's `.pl`
        // colorize fixtures, and those are fixtures: the one-shot rule
        // takes them by name rather than by pretending a
        // syntax-highlighting sample is a Perl test.
        test_path: |p| {
            p.contains("/t/") || p.ends_with(".t") || p.contains("/xt/") || p.contains("/tests/")
        },
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
        // beside it in the argument list: prose rather than an
        // identifier, the way a Zig test label is.
        let list = node.parent().filter(|p| p.kind() == "list_expression")?;
        list.named_child(0).filter(|n| n.kind() == "string_literal")
    })
}

/// A `use` of a pragma — `strict`, `warnings`, `utf8` — turns a
/// compiler switch on and is not a dependency. `parent` and `base` are
/// pragmas too, but their ARGUMENTS are the dependency, so they stay on
/// this list and are read by `superclasses` instead.
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

/// The modules whose `use` ARGUMENTS name classes rather than symbols.
/// `parent` and `base` are the core pragmas that do it; `Mojo::Base` is
/// Mojolicious spelling the same thing, and its non-class arguments
/// (`-strict`, `-base`, `-role`, `-signatures`) are barewords rather
/// than strings, so reading only the strings tells them apart.
///
/// Every other module's import list holds FUNCTION names — `use
/// POSIX qw(strftime)` — and harvesting those would manufacture a
/// dependency on `strftime`. In the gold corpus 63 `use` statements
/// carry a `::`-shaped string argument and 61 of them are Mojo::Base.
const SUPERCLASS_ARGS: &[&str] = &["parent", "base", "Mojo::Base"];

/// Moo/Moose/Role::Tiny composition. `with` consumes roles and
/// `extends` names a superclass; both take class names and nothing
/// else. 25 of the corpus's 26 arguments resolve to a project file and
/// the 26th is Exporter::Tiny, a CPAN dependency, which is the answer
/// an unresolved one should get.
const COMPOSERS: &[&str] = &["with", "extends"];

/// Perl declares a dependency four ways.
///
///   use Foo::Bar;               the `module` field of `use_statement`
///   require Foo::Bar;           `require_expression` carries NO fields
///                               at all, so `child_by_field_name` never
///                               matches; 32 in the corpus
///   use parent qw(Foo::Bar);    67 superclass arguments, 57 of which
///   use Mojo::Base 'Foo::Bar';  name a project file, plus 61 more
///   with 'Foo::Role';           26 role compositions, read as ordinary
///   extends 'Foo::Base';        calls
///
/// Inheritance and composition ARE the Perl dependency graph: Plack's
/// middleware chain and Dancer2's roles are built from nothing else,
/// and 186 of 410 judged modules read as orphaned while they go
/// uncounted.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    match node.kind() {
        "require_expression" => require_target(node, src),
        "use_statement" => use_targets(node, src),
        "source_file" => namespaces(node, src),
        _ => composed(node, src),
    }
}

/// A namespace a STRING names, and the modules underneath it.
///
/// Perl's fifth way of stating a dependency, and the one no `use` line
/// records: a namespace is handed to a loader as data and the leaf is
/// decided at run time. Plack/lib/Plack/Builder.pm:20 and :31 write
/// `Plack::Util::load_class($mw, 'Plack::Middleware')`, Runner.pm:194
/// and :222 the same with `'Plack::Loader'`, and Loader.pm:41 with
/// `'Plack::Handler'`. Util.pm:401 documents the two-argument form as
/// `load_class($class [, $prefix ])`. Test.pm:14 writes
/// `my $subclass = "Plack::Test::$Impl";`, where the literal head IS
/// the namespace and the leaf is a variable. mojo's Plugins.pm:7 says
/// `has namespaces => sub { ['Mojolicious::Plugin'] };` and Dancer2's
/// CLI.pm:10 `subcommand gen => 'Dancer2::CLI::Gen'`. Structurally the
/// same claim kong's `namespace = "kong.db.migrations.core"` makes.
///
/// 25 orphans across Plack, mojo, PPI and Dancer2 are a module under
/// one of the 491 namespaces this finds.
///
/// `use` and `require` subtrees are skipped: they name their module
/// directly and are read at the declaration, and reading the same name
/// again here would double every one of them.
fn namespaces(root: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "use_statement" | "require_expression") {
            continue;
        }
        if node.kind() == "string_content" {
            let fresh = named_namespace(node, src)
                .into_iter()
                .filter(|t| seen.insert(t.clone()));
            out.extend(fresh.map(|target| super::ImportInfo {
                target: target.into(),
                names: Vec::new(),
                reach: super::Reach::Mention,
            }));
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

/// The namespace a string OPENS with, and the glob under it.
///
/// The string must open with the name (one in the middle of a sentence
/// is prose) and carry two or more `::`-joined segments, so a single
/// bareword-shaped word cannot fire.
///
/// The exact name is stated only when the literal IS the whole string.
/// `"Plack::Test::$Impl"` interpolates its leaf, so it names the
/// namespace and no module called `Plack::Test`.
fn named_namespace(node: Node, src: &[u8]) -> Vec<String> {
    let text = node.utf8_text(src).unwrap_or("");
    let namey = |c: &char| c.is_ascii_alphanumeric() || *c == '_' || *c == ':';
    let head: String = text.chars().take_while(namey).collect();
    let segs: Vec<&str> = head.split("::").filter(|s| !s.is_empty()).collect();
    if segs.len() < 2 {
        return Vec::new();
    }
    let name = segs.join("::");
    let whole = head == text && node.parent().is_some_and(|p| p.named_child_count() == 1);
    [format!("{name}::*")]
        .into_iter()
        .chain(whole.then_some(name))
        .collect()
}

/// A module name is the only thing `use` and `require` can reach that a
/// file might answer to; resolution decides whether that file is in
/// this project or on CPAN, so a miss is an ordinary dependency.
fn dependency(target: &str) -> super::ImportInfo {
    super::ImportInfo {
        target: target.into(),
        names: Vec::new(),
        reach: super::Reach::Anywhere,
    }
}

/// `require Foo::Bar`. The node has no fields, so the module is its
/// first child, and it must be a BAREWORD: `require "some/file.pl"`
/// reads a file off `@INC` rather than naming a module, and `require
/// $class` names one only at run time.
fn require_target(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    node.named_child(0)
        .filter(|c| c.kind() == "bareword")
        .and_then(|c| c.utf8_text(src).ok())
        .filter(|t| !t.is_empty() && !PRAGMAS.contains(t))
        .map(dependency)
        .into_iter()
        .collect()
}

fn use_targets(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let Some(module) = node
        .child_by_field_name("module")
        .and_then(|m| m.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if !module.is_empty() && !PRAGMAS.contains(&module) {
        out.push(dependency(module));
    }
    if SUPERCLASS_ARGS.contains(&module) {
        out.extend(superclasses(node, src).iter().map(|c| dependency(c)));
    }
    out
}

/// `with 'Foo::Role'` and `extends 'Foo::Base'`: a call to the grammar,
/// promoted to an import by `refine` the way shell promotes `source`.
fn composed(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if !is_composition(node, src) {
        return Vec::new();
    }
    superclasses(node, src)
        .iter()
        .map(|c| dependency(c))
        .collect()
}

fn is_composition(node: Node, src: &[u8]) -> bool {
    matches!(
        node.kind(),
        "function_call_expression" | "ambiguous_function_call_expression"
    ) && callee_text(node, src).is_some_and(|t| COMPOSERS.contains(&t))
}

/// The class names in one `qw()` word list. A leading `-` marks a flag
/// (`-norequire`, `-signatures`) rather than a class.
fn class_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split_whitespace()
        .filter(|w| !w.starts_with('-'))
        .map(str::to_string)
}

/// The class names a statement quotes. Only STRING content counts: a
/// `qw()` list holds several per node and a bareword option
/// (`-norequire`, `-signatures`) holds none, which is exactly the
/// distinction between an argument that names a class and one that sets
/// a flag.
fn superclasses(node: Node, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "string_content" {
            out.extend(class_words(n.utf8_text(src).unwrap_or("")));
            continue;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    out
}

/// Signatures, where the code has them. A sub that unpacks `@_` by hand
/// declares nothing and correctly reads as taking no parameters. The
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
        // `@rest` and `%opts` take whatever is left, named or not.
        splat: kw_splat,
        boolish,
        ..Default::default()
    };
    match node.kind() {
        "mandatory_parameter" => Some(info(name_of(node)?, false, false)),
        "optional_parameter" => Some(info(name_of(node)?, true, false)),
        // `sub f (:$x)`: named at the call site, so not opaque.
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
/// unary node: the grammar has a `logical_not_expression` kind and does
/// not use it for either spelling, so reading only that kind leaves
/// this language with no negations at all.
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
/// that follow it. Deliberately shallow: two statements is what a
/// reader checks too.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "eval_expression" {
        return false;
    }
    // Only a BARE eval statement. `my $ok = eval { ... }` hands the
    // caller a value to test and `eval { ...; 1 } or do { ... }` handles
    // the failure in the same statement. Both keep the error in the
    // story, and counting them puts this rung-2 gate over its ceiling
    // on the gold corpus at 1.6%.
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
/// evidence lives on the call it was passed to, the shape jest gives
/// TypeScript and busted gives Lua.
fn declaring_test<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let list = node.parent().filter(|p| p.kind() == "list_expression")?;
    let call = list.parent()?;
    matches!(callee_text(call, src), Some("subtest")).then_some(list)
}

fn declares_test(node: Node, src: &[u8]) -> bool {
    declaring_test(node, src).is_some()
}

/// `skip` and `todo_skip` suppress the block they head. `plan` does not
/// suppress anything by itself: `plan tests => 15` DECLARES how many
/// tests will run, which is the opposite. It counts only in the one
/// form that switches a file off wholesale.
fn skips_test(node: Node, src: &[u8]) -> bool {
    match callee_text(node, src) {
        Some("skip" | "todo_skip") => true,
        Some("plan") => node
            .child_by_field_name("arguments")
            .and_then(|a| a.utf8_text(src).ok())
            .is_some_and(|a| a.trim_start().starts_with("skip_all")),
        _ => false,
    }
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
/// so most code offers only the comment run.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment", "pod"], &[], src)
}

/// A hash reference literal with bareword keys is an undeclared shape:
/// nothing catches a typo in one, and `use strict` does not reach hash
/// keys.
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
        // `with 'Foo::Role'` composes a role into this class. The
        // grammar has only calls to offer, so the import has to be
        // recognised as one, the move the shell pack makes for
        // `source`.
        Sem::Call if is_composition(node, src) => Sem::Import,
        Sem::BoolOp if node.kind() == "binary_expression" => {
            match super::field_text_is(node, "operator", src) {
                Some("&&" | "||" | "//") => Sem::BoolOp,
                _ => Sem::None,
            }
        }
        _ => sem,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn targets(src: &str) -> Vec<String> {
        let pack = super::Lang::Perl.pack();
        let mut parser = pack.make_parser();
        let facts = crate::facts::extract(pack, &mut parser, Path::new("lib/App/Core.pm"), src);
        facts.imports.iter().map(|i| i.target.to_string()).collect()
    }

    #[test]
    fn inheritance_and_composition_are_dependencies_too() {
        // 67 `use parent`/`use base` arguments, 61 Mojo::Base parents,
        // 32 `require`s and 26 role compositions in the Perl gold
        // corpus, none of them read when only `use MODULE` counts.
        let got = targets(
            "use parent qw( App::Base );\n\
             use base 'App::Other';\n\
             use Mojo::Base 'App::Mother', -signatures;\n\
             with qw<\n  App::Role::Alpha\n  App::Role::Beta\n>;\n\
             extends 'App::Sub::Deep';\n\
             sub go { require App::Lazy; }\n",
        );
        for want in [
            "App::Base",
            "App::Other",
            "Mojo::Base",
            "App::Mother",
            "App::Role::Alpha",
            "App::Role::Beta",
            "App::Sub::Deep",
            "App::Lazy",
        ] {
            assert!(got.iter().any(|t| t == want), "{want} missing from {got:?}");
        }
    }

    #[test]
    fn a_namespace_a_string_names_reaches_the_modules_under_it() {
        // Plack/lib/Plack/Builder.pm:20 hands 'Plack::Middleware' to a
        // loader and the leaf is decided at run time; Test.pm:14 builds
        // "Plack::Test::$Impl" the same way with the leaf interpolated.
        let got = targets(
            "my $c = Plack::Util::load_class($mw, 'Plack::Middleware');\n\
             my $subclass = \"Plack::Test::$Impl\";\n\
             use parent qw( App::Base );\n\
             warn 'failed to load Plack::Middleware here';\n\
             my $one = 'Bareword';\n",
        );
        // The namespace, and everything under it.
        assert!(got.iter().any(|t| t == "Plack::Middleware"), "{got:?}");
        assert!(got.iter().any(|t| t == "Plack::Middleware::*"), "{got:?}");
        // An interpolated leaf names no module called Plack::Test, so
        // only the glob is stated.
        assert!(got.iter().any(|t| t == "Plack::Test::*"), "{got:?}");
        assert!(!got.iter().any(|t| t == "Plack::Test"), "{got:?}");
        // A `use` names its module at the declaration; reading the
        // string again here would double every one in the file.
        let named = got.iter().filter(|t| *t == "App::Base").count();
        assert_eq!(named, 1, "{got:?}");
        // The string must OPEN with the name and carry two segments.
        assert!(!got.iter().any(|t| t.starts_with("failed")), "{got:?}");
        assert!(!got.iter().any(|t| t.starts_with("Bareword")), "{got:?}");
    }

    #[test]
    fn a_harness_a_c_project_writes_in_perl_is_still_a_test() {
        let is_test = crate::lang::perl::pack().test_path;
        // CPAN's own spellings.
        assert!(is_test("/Plack/t/middleware.t"));
        assert!(is_test("/mojo/xt/author.t"));
        // curl keeps its harness under `tests/`, because the project is
        // a C project and `t/` is not its convention.
        assert!(is_test("/curl/tests/runtests.pl"));
        assert!(is_test("/curl/tests/ftpserver.pl"));
        assert!(is_test("/curl/tests/appveyor.pm"));
        assert!(is_test("/curl/tests/libtest/test1013.pl"));
        // Production Perl is untouched, and `/test/` singular is not
        // this rule: vscode's colorize samples are fixtures.
        assert!(!is_test("/Plack/lib/Plack/Builder.pm"));
        assert!(!is_test("/curl/docs/libcurl/symbols.pl"));
        assert!(!is_test("/vscode/ext/colorize-fixtures/test.pl"));
    }

    #[test]
    fn a_pragma_and_a_flag_argument_are_not_dependencies() {
        // `-norequire` and `-signatures` set options; `strict` switches
        // the compiler on. None of the three names a module.
        let got = targets(
            "use strict;\nuse warnings;\n\
             use parent -norequire, 'App::Base';\n\
             use Mojo::Base -role;\n\
             use POSIX qw(strftime setlocale);\n\
             require \"some/file.pl\";\n",
        );
        assert_eq!(got, ["App::Base", "Mojo::Base", "POSIX"]);
    }
}
