//! Zig: the TigerStyle language. No exceptions and no hidden control
//! flow: errors are values, `catch` is an expression, and assertion
//! culture is the point (std.debug.assert feeds the asserts metric).

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, field_text_is, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    // `test "label" { .. }` blocks are first-class units.
    ("test_declaration", Sem::FnDef),
    ("struct_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("union_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("if_expression", Sem::If),
    ("else_clause", Sem::Else),
    ("while_statement", Sem::Loop),
    ("while_expression", Sem::Loop),
    ("for_statement", Sem::Loop),
    ("for_expression", Sem::Loop),
    ("switch_expression", Sem::Match),
    ("switch_case", Sem::CaseArm),
    // Error handling is an expression; a catch still branches.
    ("catch_expression", Sem::Catch),
    ("defer_statement", Sem::With),
    ("errdefer_statement", Sem::With),
    ("comptime_statement", Sem::With),
    ("comptime_declaration", Sem::With),
    // `and`/`or` keywords arrive via binary_expression; refine filters.
    ("binary_expression", Sem::BoolOp),
    ("break_expression", Sem::Jump),
    ("continue_expression", Sem::Jump),
    ("call_expression", Sem::Call),
    // @builtin(...) calls; refine turns @import into Sem::Import.
    ("builtin_function", Sem::Call),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("builtin_identifier", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("multiline_string", Sem::StrLit),
    ("character", Sem::StrLit),
    ("boolean", Sem::BoolLit),
];

// The grammar labels nothing on `variable_declaration` — `var x: u32 =
// 1` is an unfielded identifier followed by a type and a value — so
// these bind POSITIONALLY, which the empty field name selects. The
// loop variable lives one level down, inside `|it|`.
const DEF_SITES: &[(&str, &str)] = &[("variable_declaration", ""), ("payload", "")];
// The SAME kind, because this grammar spells a fresh binding and a
// reassignment identically: `x = 3` also parses as a
// variable_declaration. Nothing in the node distinguishes them, and
// nothing has to: the repurposing check requires a STRICTLY EARLIER
// definition of the name, which a fresh binding never has.
const REASSIGNS: &[(&str, &str)] = &[("variable_declaration", "")];
const ATTR: (&str, &str) = ("field_expression", "object");

/// `anytype` defers the type to the call site: comptime-checked, but
/// the signature states nothing.
const LOOSE: &[&str] = &["anytype"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_zig::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Zig,
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
        return_type_field: "type",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: true,
        refine,
        // `test "label" { .. }`: the prose label names the unit.
        name_node: test_label,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
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
        swallows_error: |_, _| false,
        // No exceptions, so no chain to break.
        loses_context: |_, _| false,
        panicky,
        // Same as Go: the guard is a `defer` elsewhere in the block.
        // Anonymous struct literals are inferred against a declared type.
        record_keys: |_, _| None,
        // No async in this language; threads are not it.
        is_async: |_, _| false,
        declares_test: |node, _| node.kind() == "test_declaration",
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path,
        // std.debug.assert, wrapper assert_* fns, std.testing.expect and
        // its expectXxx family. Not `expected_value()`-style lookalikes.
        asserty: |call, src| {
            call.child_by_field_name("function")
                .and_then(|f| f.utf8_text(src).ok())
                .map(|t| t.rsplit('.').next().unwrap_or(t))
                .is_some_and(|seg| {
                    seg == "assert" || seg.starts_with("assert_") || super::expectish(seg)
                })
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        // Multiple values come back as a named struct: the remedy the
        // metric would recommend, already applied by the language.
        return_arity: |_, _| 0,
        // An interface is a comptime convention (a struct of function
        // pointers, an anytype constraint) with no declared form.
        interfaces: |_, _| Vec::new(),
        // `test` blocks are compiled in or out by the build; the
        // language has no per-test off switch.
        skips_test: |_, _| false,
        magic_exempt: &[
            "variable_declaration",
            "switch_case",
            "index_expression",
            "array_type",
        ],
        // `const password = "..."`: the secrets anchor. The grammar
        // fields nothing here; bound_name and assigns_to fall back to
        // the identifier child and the last named child.
        assign_kinds: &["variable_declaration"],
    }
}

/// The build DSL names a source file with a PATH, not an `@import`:
/// `b.path("src/main_bench.zig")` is how an executable, a test or a
/// module states its root. Nothing else reaches those files, so
/// reading only `@import` left ghostty's five `main_*.zig`,
/// tigerbeetle's seven client-binding generators and river's two
/// `common/` modules with no dependent.
///
/// A `.zig` argument is a module reference and a C HEADER is one too:
/// `Index::zig` hands the second to C's own resolver, keyed on the
/// target's extension so that this and `@cInclude` share one path. Not a
/// `.c`: a translation unit is linked rather than included. Not an asset
/// or a directory. 113 `b.path("*.zig")` sites are spelled across the
/// corpus and every one names a file in the tree.
fn build_path<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    if node.kind() != "call_expression" {
        return None;
    }
    let callee = node.child_by_field_name("function")?;
    let field = |name| callee.child_by_field_name(name)?.utf8_text(src).ok();
    if callee.kind() != "field_expression" || field("member") != Some("path") {
        return None;
    }
    // `*std.Build` is `b` by universal convention, and requiring the
    // name keeps `self.path(..)` and `dir.path(..)` out.
    if field("object") != Some("b") {
        return None;
    }
    // Arguments are direct children of a call_expression; only
    // builtin_function wraps them in an `arguments` node.
    let mut cursor = node.walk();
    let arg = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "string")?;
    let target = arg.utf8_text(src).ok()?.trim_matches('"');
    // A build file names a file in this repository whatever language it
    // is written in, and a C header is the other one Zig reaches for:
    // river/build.zig:159 writes `.c_source_file = b.path("river/c.h")`,
    // and ghostty installs `pnglibconf.h`, `libintl.h`,
    // `freetype-zig.h` and `include/ghostty.h` the same way.
    //
    // `Index::zig` already hands a C-header target to C's own resolver:
    // the routing keys on the extension rather than on the node, so that
    // this and `@cInclude` share one path. Without a `.h` emitted here
    // that arm would be reachable from `@cInclude` alone.
    (target.ends_with(".zig") || crate::lang::c_header(target)).then_some(target)
}

/// `const std = @import("std");`: refine reclassifies the call, and the
/// binding is the declaration's leading identifier.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if let Some(target) = build_path(node, src) {
        return vec![super::ImportInfo {
            target: target.into(),
            names: Vec::new(),
            // A build path names a file in THIS repository, so a miss
            // is a miss rather than a package that lives elsewhere.
            reach: super::Reach::Project,
        }];
    }
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let mut cursor = node.walk();
    let args = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "arguments");
    let target = args.and_then(|a| {
        let mut cursor = a.walk();
        a.named_children(&mut cursor).find(|c| c.kind() == "string")
    });
    let Some(target) = target else {
        return Vec::new();
    };
    let name = node
        .parent()
        .filter(|p| p.kind() == "variable_declaration")
        .and_then(|p| {
            let mut cursor = p.walk();
            p.named_children(&mut cursor)
                .find(|c| c.kind() == "identifier")
        });
    vec![super::ImportInfo {
        target: text(target).trim_matches('"').into(),
        names: name.map(|n| text(n).into()).into_iter().collect(),
        reach: super::Reach::Anywhere,
    }]
}

fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        // else-if chains flatten, as in Rust/TS/Go.
        Sem::If if node.parent().is_some_and(|p| p.kind() == "else_clause") => Sem::ElseIf,
        Sem::Else
            if node
                .named_child(0)
                .is_some_and(|c| c.kind().starts_with("if_")) =>
        {
            Sem::None
        }
        Sem::BoolOp => match field_text_is(node, "operator", src) {
            Some("and" | "or" | "orelse") => Sem::BoolOp,
            _ => Sem::None,
        },
        // defer/errdefer/comptime indent only in block form; expression
        // forms (`defer x.deinit();`, `comptime T == u8`) add no depth.
        Sem::With if !has_block_child(node) => Sem::None,
        // @import is the module system, not a call. `@cInclude` is the
        // same statement about a C header, and it is how a Zig binding
        // module names the header it wraps: ghostty's `pkg/freetype/
        // c.zig:2` says `@cInclude("freetype-zig.h")` of the file beside
        // it, and `src/stb/main.zig:2` the same of two more.
        Sem::Call if matches!(builtin_name(node, src), Some("@import" | "@cInclude")) => {
            Sem::Import
        }
        // And so is `b.path("x.zig")`: the core asks about imports at
        // Sem::Import nodes only.
        Sem::Call if build_path(node, src).is_some() => Sem::Import,
        // The @xCast family: every one overrules the type checker.
        Sem::Call if builtin_name(node, src).is_some_and(is_cast_builtin) => Sem::Cast,
        _ => sem,
    }
}

/// The `@name` of a builtin_function node (its identifier is unfielded).
fn builtin_name<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    if node.kind() != "builtin_function" {
        return None;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "builtin_identifier")?
        .utf8_text(src)
        .ok()
}

/// `@intCast`, `@ptrCast`, `@bitCast`, `@enumFromInt`, and the rest.
/// Zig spells each conversion out, which is why they are countable.
fn is_cast_builtin(name: &str) -> bool {
    name.ends_with("Cast") || name.contains("FromInt") || name.contains("FromPtr")
}

fn has_block_child(node: Node) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|c| c.kind() == "block")
}

/// `test "decodes a header" { ... }` names itself with a string, and
/// `test decodeHeader { ... }` with an identifier. Nothing else in the
/// grammar carries its name outside a `name` field.
fn test_label(node: Node) -> Option<Node> {
    if node.kind() != "test_declaration" {
        return None;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| matches!(c.kind(), "string" | "identifier" | "builtin_identifier"))
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    (node.kind() == "parameter").then(|| ParamInfo {
        name: field_text_is(node, "name", src).unwrap_or("").into(),
        boolish: field_text_is(node, "type", src) == Some("bool"),
        typed: true,
        type_name: field_text_is(node, "type", src).unwrap_or("").into(),
        loose: field_text_is(node, "type", src).is_some_and(|t| super::is_loose(t, LOOSE)),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(src).ok())
        == Some(unit_name)
}

/// Zig: `pub fn`, a leading `pub` keyword on the declaration.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| t.starts_with("pub ") || t.starts_with("pub\n"))
}

/// Doc comments: `///` runs directly above (comment kind is unified).
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &["///"], src)
}

/// `!x`, the only negation operator.
fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let bang =
        node.kind() == "unary_expression" && field_text_is(node, "operator", src) == Some("!");
    bang.then(|| node.child_by_field_name("argument"))?
}

/// The suffix and the directory, in both spellings of the plural. zls
/// files its whole suite under `tests/` — 58 .zig files, rooted at
/// `tests/tests.zig`, which build.zig:235 hands to `b.addTest` — and
/// tigerbeetle names its four test roots `unit_tests.zig`,
/// `integration_tests.zig`, `fuzz_tests.zig`,
/// `state_machine_tests.zig`.
///
/// Both rules capture only test code: of the 63 files they add, the
/// ones a non-captured non-test file imports are zls/build.zig reaching
/// its two case-adders and a fuzzer reaching state_machine_tests.zig.
/// `_fuzz.zig`, `_benchmark.zig`, `/testing/` and a bare `test.zig`
/// were measured the same way and are NOT here: ewah.zig imports
/// ewah_fuzz.zig, stdx.zig imports radix_benchmark.zig, 94 sites import
/// src/testing/, and six ghostty packages import their own test.zig.
fn test_path(p: &str) -> bool {
    p.ends_with("_test.zig")
        || p.ends_with("_tests.zig")
        || p.contains("/test/")
        || p.contains("/tests/")
}

/// Inline assembly, and nothing else. It is the one place a Zig file
/// stops being Zig: the operands are a string the compiler hands
/// through, so no reader and no tool can follow what runs.
///
/// Everything else the language calls unsafe is already judged
/// elsewhere or is a convention rather than a gap. `@ptrCast` and the
/// `FromInt`/`FromPtr` family are casts, and `refine` says so.
/// `@field(x, name)` looks like the computed attribute access this
/// metric was written for, but it is how Zig iterates a struct at
/// comptime: 444 of the 454 uses in the gold corpus take a computed
/// name, across 123 of 1,138 files. Counting those would measure the
/// idiom, not a defect.
fn spooky(node: Node, _sem: Sem, _src: &[u8]) -> bool {
    node.kind() == "asm_expression"
}

/// `@panic`, and the convention of naming a function that ends in one.
fn panicky(call: Node, src: &[u8]) -> bool {
    if builtin_name(call, src) == Some("@panic") {
        return true;
    }
    let Some(f) = call.child_by_field_name("function") else {
        return false;
    };
    f.utf8_text(src).is_ok_and(|t| t.ends_with("panic"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tests_directory_is_test_code_like_a_test_directory() {
        let is_test = super::pack().test_path;
        for p in [
            "zls/tests/tests.zig",
            "zls/tests/analysis/basic.zig",
            "tigerbeetle/src/unit_tests.zig",
            "tigerbeetle/src/clients/java/src/jni_tests.zig",
            "ghostty/src/terminal/main_test.zig",
            "tigerbeetle/src/clients/c/test/main.zig",
        ] {
            assert!(is_test(p), "{p}");
        }
        // Fuzzers and benchmarks stay production: ewah.zig imports
        // ewah_fuzz.zig and stdx.zig imports radix_benchmark.zig.
        for p in [
            "tigerbeetle/src/ewah_fuzz.zig",
            "tigerbeetle/src/stdx/radix_benchmark.zig",
            "tigerbeetle/src/testing/fuzz.zig",
            "ghostty/pkg/freetype/test.zig",
        ] {
            assert!(!is_test(p), "{p}");
        }
    }

    /// The build DSL's path form, and the three shapes that look like
    /// it: a non-.zig asset, another receiver, another method.
    #[test]
    fn a_build_path_names_a_module_and_a_receiver_that_is_not_the_builder_does_not() {
        let src = r#"pub fn build(b: *std.Build) void {
    const a = b.path("src/main_bench.zig");
    const c = b.path("src/vendor/lib.c");
    const d = self.path("src/other.zig");
    const e = b.addPath("src/third.zig");
    const f = b.path("river/c.h");
    const g = b.path("include/ghostty.h");
    const h = b.path("README.md");
}
"#;
        let pack = super::pack();
        let mut parser = pack.make_parser();
        let tree = parser.parse(src, None).expect("zig parses");
        let mut found = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if let Some(target) = super::build_path(node, src.as_bytes()) {
                found.push(target);
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        // A `.zig` module and a C header, which `Index::zig` hands to
        // C's own resolver. Not a `.c` (a translation unit is linked
        // rather than included) and not a document. The walk is a
        // stack, so the later statements come out first.
        assert_eq!(
            found,
            ["include/ghostty.h", "river/c.h", "src/main_bench.zig"]
        );
    }
}
