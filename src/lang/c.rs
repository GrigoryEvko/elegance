//! C: the language the classical rules were written for (K&P, McCabe).
//!
//! Honest caveats, by decision:
//! - Macros stay unexpanded. Function-like macros parse as calls; macro
//!   BODIES are invisible, so macro-heavy code under-measures. For `.h`
//!   tricks the numbers are floors rather than truths.
//! - `#if`/`#ifdef`/`#elif` count as If/ElseIf: conditional compilation
//!   is a real branch the reader must follow. Convention does not indent
//!   preprocessor regions, so they add cognitive cost and visual depth
//!   alike, and the depth budget is calibrated with that in.
//! - `goto` is the flat +1 the Sem::Goto variant exists for.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, field_text_is, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("struct_specifier", Sem::TypeDef),
    ("union_specifier", Sem::TypeDef),
    ("enum_specifier", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("else_clause", Sem::Else),
    ("conditional_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    // Both `case` and `default` arms share this kind.
    ("case_statement", Sem::CaseArm),
    ("preproc_if", Sem::If),
    ("preproc_ifdef", Sem::If),
    ("preproc_elif", Sem::ElseIf),
    ("preproc_else", Sem::Else),
    // All binary operators share one kind; refine keeps only `&&`/`||`.
    ("binary_expression", Sem::BoolOp),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("goto_statement", Sem::Goto),
    ("call_expression", Sem::Call),
    ("cast_expression", Sem::Cast),
    ("comment", Sem::Comment),
    ("preproc_include", Sem::Import),
    // `#include` is only a `preproc_include` where a declaration may
    // stand. Inside a struct or enum body the grammar reads it as the
    // generic `preproc_call`, and every other directive shares that
    // kind, so `include_target` tells them apart.
    ("preproc_call", Sem::Import),
    ("identifier", Sem::Ident),
    ("field_identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("number_literal", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("char_literal", Sem::StrLit),
    ("concatenated_string", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
];

// Pure binding side only: a declaration's declarator field includes
// initializer values, init_declarator's does not.
const DEF_SITES: &[(&str, &str)] = &[
    ("init_declarator", "declarator"),
    ("assignment_expression", "left"),
];
/// One kind covers `=` and `+=` alike; the check filters by the
/// spelled operator.
const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("field_expression", "argument");

/// `void *` is C's only escape hatch, and its most load-bearing one.
const LOOSE: &[&str] = &["void"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::C,
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
        name_node,
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
        negation_operand: |node, src| {
            (node.kind() == "unary_expression" && field_text_is(node, "operator", src) == Some("!"))
                .then(|| node.child_by_field_name("argument"))?
        },
        catch_sin: |_, _| None,
        swallows_error: |_, _| false,
        // No exceptions, so no chain to break.
        loses_context: |_, _| false,
        panicky,
        // Manual everywhere, so every fopen would fire and none would mean anything.
        // Designated initializers belong to a declared struct.
        record_keys: |_, _| None,
        // No async in this language; threads are not it.
        is_async: |_, _| false,
        declares_test: |_, _| false,
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/test/") || p.contains("/tests/") || p.ends_with("_test.c"),
        // assert(), plus the assert-wrapper convention (serverAssert,
        // redisassert, ASSERT): case-insensitive assert prefix/suffix.
        asserty: |call, src| {
            call.child_by_field_name("function")
                .filter(|f| f.kind() == "identifier")
                .and_then(|f| f.utf8_text(src).ok())
                .is_some_and(super::assertish)
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        // One return value by construction; the rest leave through
        // out-parameters, which `params` already prices.
        return_arity: |_, _| 0,
        // A vtable is a struct-of-function-pointers idiom, not a
        // declaration the grammar can point at.
        interfaces: |_, _| Vec::new(),
        // No test-declaration form, so nothing to switch off.
        skips_test: |_, _| false,
        magic_exempt: &[
            // Enum values and #defines ARE the named constants.
            "enumerator",
            "preproc_def",
            "case_statement",
            "subscript_expression",
            "array_declarator",
            "sized_type_specifier",
        ],
        assign_kinds: &["init_declarator"],
    }
}

/// Quotes stripped, angle brackets kept: `<stdio.h>` is definitionally
/// external. C binds no names.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    include_target(node, src)
        .map(|t| {
            vec![super::ImportInfo {
                target: t.into(),
                names: Vec::new(),
                reach: super::Reach::Anywhere,
            }]
        })
        .unwrap_or_default()
}

/// The header an include node names, or None when the node is not an
/// include at all.
///
/// `preproc_include` carries a `path` field, and the grammar produces
/// it wherever a declaration may stand. Where one may not (a
/// struct body, an enum body) the same line parses as `preproc_call`,
/// which is also `#pragma`, `#error` and `#line`, so the directive has
/// to be read. ctre's pcre_actions.hpp holds 17 of its 22 includes
/// inside `struct pcre_actions { … }` and every one of those headers
/// read as an orphan; cutlass, musl and fmt spell the same idiom in
/// enum bodies.
///
/// `#include FOO` stays invisible, as the module caveat states: the
/// argument is a macro name and expansion is not run.
pub(super) fn include_target<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    if spliced_include(node, src) {
        // The specifier stands beside the ERROR node as a plain string.
        let beside = node.next_sibling()?;
        return specifier(beside.utf8_text(src).ok()?);
    }
    if let Some(path) = node.child_by_field_name("path") {
        return path.utf8_text(src).ok().map(|t| t.trim_matches('"'));
    }
    let directive = super::field_text_is(node, "directive", src)?;
    if !matches!(directive, "#include" | "#include_next" | "#import") {
        return None;
    }
    // A `preproc_arg` runs to end of line, so it carries any trailing
    // comment with it; the specifier ends at its own closing delimiter.
    specifier(super::field_text_is(node, "argument", src)?)
}

/// The header a specifier names, quotes stripped and angle brackets kept.
fn specifier(arg: &str) -> Option<&str> {
    let arg = arg.trim_start();
    match arg.as_bytes().first()? {
        b'"' => arg[1..].split('"').next(),
        b'<' => arg.split_once('>').map(|(head, _)| &arg[..head.len() + 1]),
        _ => None,
    }
}

/// An `#include` the grammar could not place.
///
/// A directive is a declaration, and no declaration may stand inside an
/// ARRAY INITIALIZER, so tree-sitter emits the `#include` token itself
/// as an ERROR node and leaves the specifier beside it as an ordinary
/// string literal. musl splices every one of its character tables in
/// that way: `iswalpha.c:3` is `static const unsigned char table[] = {`,
/// then `#include "alpha.h"`, then `};`, and `iconv.c` does it nine
/// times over. Thirteen musl headers read as included by nobody while
/// the two files that splice them sat right beside them.
///
/// Promoted from `Sem::None`, and the ENCLOSING error node rather than
/// the string, so the node gives nothing up: the specifier keeps its own
/// `StrLit` classification and the secret, repetition and clone checks
/// go on seeing it. Only a real file is ever named.
pub(super) fn spliced_include(node: Node, src: &[u8]) -> bool {
    node.is_error()
        && node
            .utf8_text(src)
            .is_ok_and(|t| matches!(t.trim(), "#include" | "#include_next" | "#import"))
}

/// abort() is C's panic; exit() is judgment we don't make.
fn panicky(call: Node, src: &[u8]) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(src).ok())
        == Some("abort")
}

/// `&&`/`||` from the shared binary kind; `else if` flattens as in Rust.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::None if spliced_include(node, src) => Sem::Import,
        Sem::If if node.parent().is_some_and(|p| p.kind() == "else_clause") => Sem::ElseIf,
        Sem::Else
            if node
                .named_child(0)
                .is_some_and(|c| c.kind() == "if_statement") =>
        {
            Sem::None
        }
        Sem::BoolOp => match field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        _ => sem,
    }
}

/// The name hides at the bottom of the declarator chain:
/// `static void *name(...)` is function_definition -> pointer_declarator
/// -> function_declarator -> identifier.
fn name_node(node: Node) -> Option<Node> {
    if node.kind() != "function_definition" {
        return None;
    }
    let mut d = node.child_by_field_name("declarator")?;
    while let Some(inner) = d.child_by_field_name("declarator") {
        d = inner;
    }
    (d.kind() == "identifier").then_some(d)
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if node.kind() != "parameter_declaration" {
        return None;
    }
    // Unnamed prototype params (`int f(int, char *)`) keep an empty
    // name. Pointer-ness must be read DURING the drill: checking the
    // final declarator saw only the identifier, so a named `void *p`
    // was never loose and the hatch fired for unnamed prototypes alone.
    let mut d = node.child_by_field_name("declarator");
    let mut pointered = d.is_some_and(|n| n.kind().contains("pointer"));
    while let Some(inner) = d.and_then(|n| n.child_by_field_name("declarator")) {
        pointered = pointered || inner.kind().contains("pointer");
        d = Some(inner);
    }
    let name = d
        .filter(|n| n.kind() == "identifier")
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("");
    Some(ParamInfo {
        name: name.into(),
        boolish: matches!(field_text_is(node, "type", src), Some("bool" | "_Bool")),
        typed: true,
        type_name: field_text_is(node, "type", src).unwrap_or("").into(),
        // Only a POINTER to void is the hatch; a `void` return is not.
        loose: field_text_is(node, "type", src).is_some_and(|t| super::is_loose(t, LOOSE))
            && pointered,
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(src).ok())
        == Some(unit_name)
}

/// Public means external linkage: any function not declared `static`.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    !node
        .named_children(&mut cursor)
        .any(|c| c.kind() == "storage_class_specifier" && c.utf8_text(src) == Ok("static"))
}

/// Comment run directly above the definition (contiguous rows).
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &[], src)
}

/// setjmp/longjmp: control flow the text cannot predict.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "identifier")
            .and_then(|f| f.utf8_text(src).ok())
            .is_some_and(|t| matches!(t, "setjmp" | "longjmp" | "siglongjmp"))
}

#[cfg(test)]
mod tests {
    use crate::facts::extract;
    use crate::lang::Lang;
    use crate::metrics::complexity;
    use std::path::Path;

    #[test]
    fn goto_costs_one_flat_and_preproc_branches_count() {
        let pack = Lang::C.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.c"),
            "int f(int x) {\n#ifdef FAST\n    x += 1;\n#else\n    x -= 1;\n#endif\n    if (x < 0) goto fail;\n    return x;\nfail:\n    return -1;\n}\n",
        );
        let (cog, cyc) = complexity(&f.units[1]);
        // #ifdef +1, #else +1, if +1, goto +1 = 4; decisions ifdef+if = 3.
        assert_eq!((cog, cyc), (4, 3));
    }

    #[test]
    fn an_include_in_an_enum_body_is_still_an_include() {
        // `#include` is a `preproc_include` only where a declaration may
        // stand; inside a struct or enum body the grammar reads it as
        // `preproc_call`, the kind it shares with `#pragma`.
        let pack = Lang::C.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.h"),
            "#pragma once\n#include <stdio.h>\nenum codes {\n#include \"codes.h\"\n};\nstruct s {\n#include \"fields.h\"  /* trailing */\n};\n",
        );
        let targets: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(targets, ["<stdio.h>", "codes.h", "fields.h"]);
    }

    #[test]
    fn an_include_spliced_into_an_initializer_is_still_an_include() {
        // A directive is a declaration and no declaration may stand
        // inside an array initializer, so tree-sitter emits the
        // `#include` token as an ERROR node and leaves the specifier
        // beside it as an ordinary string. musl builds every character
        // table that way, and thirteen of its headers read as included
        // by nobody while the files splicing them sat right beside.
        let pack = Lang::C.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("iswalpha.c"),
            "static const unsigned char table[] = {\n#include \"alpha.h\"\n};\nstatic const unsigned char b[] = {\n#include \"nonspacing.h\"\n};\n",
        );
        let targets: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(targets, ["alpha.h", "nonspacing.h"]);
        // It really is an ERROR node: the grammar could not place the
        // directive, which is why the ENCLOSING node rather than the
        // string is what gets promoted.
        assert!(f.parse_errors > 0, "the grammar could not place it");
    }

    #[test]
    fn declarator_chains_resolve_names_and_static_visibility() {
        let pack = Lang::C.pack();
        let mut parser = pack.make_parser();
        let f = extract(
            pack,
            &mut parser,
            Path::new("t.c"),
            "/* Frees the node. */\nstatic void *pool_take(struct pool *p, size_t n) {\n    return p->slab;\n}\n\nlong api_count(void) {\n    return 0;\n}\n",
        );
        assert_eq!(&*f.units[1].name, "pool_take");
        let names: Vec<&str> = f.units[1].params.iter().map(|p| &*p.name).collect();
        assert_eq!(names, ["p", "n"]);
        assert!(!f.units[1].is_public, "static is file-local");
        assert!(f.units[1].doc_lines > 0);
        assert_eq!(&*f.units[2].name, "api_count");
        assert!(f.units[2].is_public, "external linkage is surface");
    }
}
