//! C++: the language the other packs were rehearsing for.
//!
//! Everything C's pack says still holds: macros stay unexpanded, `#if`
//! is a real branch, `goto` is the flat +1.
//!
//! - Templates are measured AS WRITTEN. One template is one unit
//!   however many instantiations the linker emits, which is the same
//!   call the C pack makes about macros: measure what you can read.
//! - A class whose methods are ALL pure virtual is an interface, and
//!   that is what `interface width` counts here. A class with a
//!   concrete method is not one.
//!
//! Names come from the declarator chain as in C, with one addition:
//! `int Store::get(...)` carries its scope in a `qualified_identifier`,
//! and the drill takes that node's `name` child, so an out-of-line
//! definition is named `get` and the class stays in the declarator.
//!
//! Tests arrive as macros. `TEST(Pool, TakesASlot) { ... }` is the only
//! declaration form the grammar can read. Catch2's `TEST_CASE("a pool
//! takes a slot")` puts a STRING where a parameter belongs and parses
//! as an error, so Catch2 files declare no tests here.
//!
//! CUDA rides this pack the way TSX rides TypeScript: a second grammar,
//! the same tables, one extra entry. `__global__` and `__device__` are
//! unnamed tokens the tree never shows, `__shared__` arrives as an
//! ordinary `type_qualifier`, and `add<<<grid, block>>>(x)` is already a
//! `call_expression` with one extra child. CUDA adds exactly two named
//! kinds to C++ and a kernel is an ordinary unit.

use tree_sitter::Node;

use super::{CatchSin, Lang, Pack, ParamInfo, field_text_is, sem_table};
use crate::clangfmt::Kind;
use crate::sem::Sem;

pub enum Dialect {
    Cpp,
    Cuda,
}

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("lambda_expression", Sem::Lambda),
    ("class_specifier", Sem::TypeDef),
    ("struct_specifier", Sem::TypeDef),
    ("union_specifier", Sem::TypeDef),
    ("enum_specifier", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("else_clause", Sem::Else),
    ("conditional_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    // `for (auto& x : xs)`: the range form is its own kind.
    ("for_range_loop", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    ("case_statement", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("preproc_if", Sem::If),
    ("preproc_ifdef", Sem::If),
    ("preproc_elif", Sem::ElseIf),
    ("preproc_else", Sem::Else),
    ("binary_expression", Sem::BoolOp),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("goto_statement", Sem::Goto),
    ("call_expression", Sem::Call),
    ("new_expression", Sem::Call),
    // The C-style `(T)x`. The named casts have no node of their own:
    // `static_cast<T>(x)` parses as a call to a template function, so
    // `refine` reclassifies those.
    ("cast_expression", Sem::Cast),
    ("comment", Sem::Comment),
    ("preproc_include", Sem::Import),
    // Inside a class or enum body `#include` is not a declaration
    // position, and the grammar reads the line as the generic
    // `preproc_call` it shares with `#pragma` and `#error`.
    ("preproc_call", Sem::Import),
    // `using_declaration` is absent by decision: it names a namespace
    // member, and the graph resolves against file paths. See `imports`.
    ("identifier", Sem::Ident),
    ("field_identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("namespace_identifier", Sem::Ident),
    ("number_literal", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("raw_string_literal", Sem::StrLit),
    ("char_literal", Sem::StrLit),
    ("concatenated_string", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("init_declarator", "declarator"),
    ("assignment_expression", "left"),
];
/// One kind covers `=` and `+=` alike; the check filters by the
/// spelled operator, and a declaration's name never starts where the
/// node starts.
const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("field_expression", "argument");

/// `void *` remains the hatch. `auto` is inference rather than evasion:
/// the compiler knows the type.
const LOOSE: &[&str] = &["void"];

/// `add<<<grid, block>>>(x)` parses as a `call_expression` carrying an
/// extra `kernel_call_syntax` child that holds the launch geometry. The
/// call is ALREADY counted by the shared table, so mapping this to
/// `Sem::Call` would price one launch as two calls. It is named here
/// rather than left out so that a grammar bump which renames or drops
/// it fails the resolution test instead of silently changing nothing.
///
/// `launch_bounds` is `__launch_bounds__(256, 4)`: a compiler hint on a
/// declaration, which is configuration rather than control flow.
const CUDA_ONLY: &[(&str, Sem)] = &[
    ("kernel_call_syntax", Sem::None),
    ("launch_bounds", Sem::None),
];

pub fn pack(dialect: Dialect) -> Pack {
    let (lang, ts): (Lang, tree_sitter::Language) = match dialect {
        Dialect::Cpp => (Lang::Cpp, tree_sitter_cpp::LANGUAGE.into()),
        Dialect::Cuda => (Lang::Cuda, tree_sitter_cuda::LANGUAGE.into()),
    };
    let kinds: &[&[(&str, Sem)]] = match lang {
        Lang::Cuda => &[KINDS, CUDA_ONLY],
        _ => &[KINDS],
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
        scope_sep: "::",
        return_type_field: "type",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: true,
        refine,
        name_node,
        composed_name,
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
        negation_operand: |node, src| {
            (node.kind() == "unary_expression" && field_text_is(node, "operator", src) == Some("!"))
                .then(|| node.child_by_field_name("argument"))?
        },
        catch_sin,
        swallows_error: |_, _| false,
        loses_context: |_, _| false,
        panicky,
        // Aggregate initialisation names a declared type.
        record_keys: |_, _| None,
        // RAII: a destructor runs on scope exit, so there is no missing
        // guard to detect. Same reason this is dead in Rust.
        // Coroutines exist in C++20 but `co_await` is not `async fn`:
        // nothing declares a unit async, so nothing can block one.
        is_async: |_, _| false,
        declares_test,
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path: |p| {
            p.contains("/test/")
                || p.contains("/tests/")
                || p.ends_with("_test.cc")
                || p.ends_with("_test.cpp")
                // The pack serves both dialects, and a CUDA test file
                // spells the same convention with its own extension.
                || p.ends_with("_test.cu")
                || p.ends_with("_unittest.cc")
        },
        asserty: |call, src| callee_name(call, src).is_some_and(is_an_assertion),
        is_hook: |_, _| false,
        // One return value; the rest leave through out-parameters or a
        // struct, and `std::tuple` needs a type to resolve.
        return_arity: |_, _| 0,
        interfaces,
        // gtest DISABLED_ prefixes are the skip, and they live in the
        // test NAME rather than in a declaration this can point at.
        skips_test: |_, _| false,
        magic_exempt: &[
            "enumerator",
            "preproc_def",
            "case_statement",
            "subscript_expression",
            "array_declarator",
            "sized_type_specifier",
            "template_argument_list",
        ],
        assign_kinds: &["init_declarator"],
    }
}

/// A class whose methods are ALL pure virtual is an interface in
/// everything but keyword: the C++ spelling of the contract Go and
/// Rust declare outright. A class with one concrete method is a base
/// class, which is a different thing, so it declares nothing here.
fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    if !matches!(node.kind(), "class_specifier" | "struct_specifier") {
        return Vec::new();
    }
    let (Some(name), Some(body)) = (
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok()),
        node.child_by_field_name("body"),
    ) else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let members: Vec<Node> = body.named_children(&mut cursor).collect();
    let methods: Vec<&Node> = members
        .iter()
        .filter(|m| m.kind() == "field_declaration" && declares_a_method(**m))
        .collect();
    // A destructor may be defaulted without making this a base class:
    // `virtual ~I() = default;` is how an interface is written.
    let concrete = members
        .iter()
        .any(|m| m.kind() == "function_definition" && !is_destructor(*m));
    let all_pure = !methods.is_empty() && methods.iter().all(|m| is_pure_virtual(**m));
    match !concrete && all_pure {
        true => vec![crate::facts::InterfaceFact {
            name: name.into(),
            line: node.start_position().row as u32 + 1,
            methods: methods.len() as u16,
        }],
        false => Vec::new(),
    }
}

fn declares_a_method(field: Node) -> bool {
    let mut d = field.child_by_field_name("declarator");
    while let Some(inner) = d {
        if inner.kind() == "function_declarator" {
            return true;
        }
        d = inner.child_by_field_name("declarator");
    }
    false
}

/// `virtual int a() = 0;`: the grammar records the `= 0` as a default
/// value on the field.
fn is_pure_virtual(field: Node) -> bool {
    field.child_by_field_name("default_value").is_some()
}

fn is_destructor(def: Node) -> bool {
    let mut d = def.child_by_field_name("declarator");
    while let Some(inner) = d {
        if inner.kind() == "destructor_name" {
            return true;
        }
        d = inner.child_by_field_name("declarator");
    }
    false
}

/// `&&`/`||` from the shared binary kind; `else if` flattens as in C.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::None if super::c::spliced_include(node, src) => Sem::Import,
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
        // `static_cast<T>(x)` has no node of its own: the grammar reads
        // it as a call to a template function. It is a cast to every
        // reader, and `reinterpret_cast` is the loudest one in the
        // language, so the four named casts are reclassified here.
        Sem::Call if is_a_named_cast(node, src) => Sem::Cast,
        _ => sem,
    }
}

fn is_a_named_cast(call: Node, src: &[u8]) -> bool {
    matches!(
        callee_name(call, src),
        Some("static_cast" | "dynamic_cast" | "reinterpret_cast" | "const_cast")
    )
}

/// The declarator chain, as in C, ending at an identifier or at a
/// `qualified_identifier`, which is how an out-of-line definition
/// carries its class. `int Store::get(...)` is named through that
/// node's `name` child, so the unit is `get` and the class stays in
/// the declarator.
fn name_node(node: Node) -> Option<Node> {
    if node.kind() != "function_definition" {
        return None;
    }
    if let Some(case) = macro_case_name(node) {
        return Some(case);
    }
    let mut d = node.child_by_field_name("declarator")?;
    while let Some(inner) = d.child_by_field_name("declarator") {
        d = inner;
    }
    // `Store::get` and `~Store` both name the unit.
    if matches!(d.kind(), "qualified_identifier" | "destructor_name") {
        return d.child_by_field_name("name").or(Some(d));
    }
    matches!(
        d.kind(),
        "identifier" | "field_identifier" | "operator_name"
    )
    .then_some(d)
}

/// A macro invocation followed by a block — `TEST(Pool, TakesASlot)
/// { ... }` — which the grammar can only read as a function called
/// TEST. A definition with NO return type whose "parameters" are bare
/// type names carrying no declarator: they are not parameters at all.
/// Returns (macro, last argument), which is the name a human reads.
/// Without this every gtest unit reports under the macro's own four
/// letters, and every test-quality metric judges `TEST` instead of the
/// case.
///
/// A constructor also has no return type, but its parameters are
/// named; the only collision is a constructor whose every parameter is
/// unnamed, which could not use them.
fn test_macro(node: Node) -> Option<(Node, Node)> {
    if node.child_by_field_name("type").is_some() {
        return None;
    }
    let declarator = node.child_by_field_name("declarator")?;
    let macro_name = declarator.child_by_field_name("declarator")?;
    if macro_name.kind() != "identifier" {
        return None;
    }
    let params = declarator.child_by_field_name("parameters")?;
    let mut cursor = params.walk();
    let args: Vec<Node> = params.named_children(&mut cursor).collect();
    let names: Vec<Node> = args
        .iter()
        .filter(|a| a.child_by_field_name("declarator").is_none())
        .filter_map(|a| a.child_by_field_name("type"))
        .filter(|t| t.kind() == "type_identifier")
        .collect();
    match names.len() == args.len() && names.len() >= 2 {
        true => Some((macro_name, *names.last()?)),
        false => None,
    }
}

fn macro_case_name(node: Node) -> Option<Node> {
    test_macro(node).map(|(_, case)| case)
}

/// `TEST(args_test, basic)` names one test in two parts, and gtest
/// itself prints them joined: `args_test.basic`. Judging `basic` alone
/// judged half a name, and the C++ gold corpus read 61% lazily named
/// against 16% for a corpus of notorious code. The metric was measuring
/// the pack rather than the tests. Joined, fmt's names say what they
/// test and a genuinely lazy `TEST(foo, bar)` still reads as two words.
fn composed_name(node: Node, src: &[u8]) -> Option<String> {
    // The gtest family only. Some other two-identifier macro still gets
    // its last argument as a name (that is `name_node`'s answer), but
    // nothing here claims to know that its first argument qualifies it.
    if !declares_test(node, src) {
        return None;
    }
    let (_, case) = test_macro(node)?;
    let suite = suite_argument(node)?;
    Some(format!(
        "{}.{}",
        suite.utf8_text(src).ok()?,
        case.utf8_text(src).ok()?
    ))
}

fn suite_argument(node: Node) -> Option<Node> {
    let params = node
        .child_by_field_name("declarator")?
        .child_by_field_name("parameters")?;
    params.named_child(0)?.child_by_field_name("type")
}

/// `#include <x>` keeps its brackets (definitionally external), and an
/// include is the only thing here that names a module.
///
/// `using ns::name` and `using namespace ns` are deliberately NOT import
/// edges. A C++ namespace has no file it corresponds to, so the graph's
/// path resolver can never match one: every such target fell through to
/// the unresolved bucket, which is supposed to mean "this names
/// something that should be HERE and is not". Across the gold corpus
/// 2,480 of the 3,236 imports reported unresolved under cpp and cuda
/// were `using` declarations — cutlass 1,386 of 1,617, transformer-
/// engine 596 of 727, kakoune 57 of 57 — and none of them could ever
/// have resolved.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    super::c::include_target(node, src)
        .map(|t| {
            vec![super::ImportInfo {
                target: t.into(),
                names: Vec::new(),
                reach: super::Reach::Anywhere,
            }]
        })
        .unwrap_or_default()
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if !matches!(
        node.kind(),
        "parameter_declaration" | "optional_parameter_declaration"
    ) {
        return None;
    }
    // Drill to the bound name. `&` and `&&` nest their identifier
    // WITHOUT a declarator field, unlike `*`, so the fall-back to the
    // first named child names `const std::string& k` as k rather than
    // as nothing.
    let mut d = node.child_by_field_name("declarator");
    let mut pointered = d.is_some_and(|n| n.kind().contains("pointer"));
    while let Some(node) = d.filter(|n| !is_a_name(*n)) {
        let Some(inner) = node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))
        else {
            break;
        };
        pointered = pointered || inner.kind().contains("pointer");
        d = Some(inner);
    }
    let name = d
        .filter(|n| is_a_name(*n))
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("");
    let declared = field_text_is(node, "type", src).unwrap_or("");
    Some(ParamInfo {
        name: name.into(),
        boolish: declared == "bool",
        typed: true,
        type_name: declared.into(),
        // A default argument makes the parameter optional at every
        // call site, and the grammar's own kind says so.
        optional: node.kind() == "optional_parameter_declaration",
        loose: super::is_loose(declared, LOOSE) && pointered,
        ..Default::default()
    })
}

fn is_a_name(node: Node) -> bool {
    matches!(node.kind(), "identifier" | "field_identifier")
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    callee_name(call, src) == Some(unit_name)
}

/// The called name, through the member and template spellings.
fn callee_name<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let f = call.child_by_field_name("function")?;
    let named = match f.kind() {
        "field_expression" => f.child_by_field_name("field")?,
        "qualified_identifier" => f.child_by_field_name("name")?,
        "template_function" => f.child_by_field_name("name")?,
        _ => f,
    };
    named.utf8_text(src).ok()
}

/// Inside a class body, `private:`/`protected:` govern until the next
/// specifier. Outside one, a definition is public unless it is static.
fn is_public(node: Node, src: &[u8]) -> bool {
    let inside_class = node
        .parent()
        .is_some_and(|p| p.kind() == "field_declaration_list");
    if !inside_class {
        let mut cursor = node.walk();
        return !node
            .named_children(&mut cursor)
            .any(|c| c.kind() == "storage_class_specifier" && c.utf8_text(src) == Ok("static"));
    }
    // The nearest preceding access specifier wins; a class defaults to
    // private and a struct to public, which the grammar does not say,
    // so the absence of any specifier is read as the struct case.
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        if p.kind() == "access_specifier" {
            return p.utf8_text(src).unwrap_or("").starts_with("public");
        }
        prev = p.prev_named_sibling();
    }
    true
}

/// Comment run directly above the definition (contiguous rows).
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &[], src)
}

/// `catch (...)` catches everything and binds nothing: C++'s spelling
/// of the bare except. An EMPTY handler swallows.
fn catch_sin(node: Node, src: &[u8]) -> Option<CatchSin> {
    let empty = node
        .child_by_field_name("body")
        .is_some_and(|b| b.named_child_count() == 0);
    if empty {
        return Some(CatchSin::Swallowed);
    }
    let params = node.child_by_field_name("parameters")?;
    let broad = params.named_child_count() == 0
        || params
            .utf8_text(src)
            .is_ok_and(|t| t.contains("...") || t.contains("std::exception"));
    broad.then_some(CatchSin::Broad)
}

/// C++ spells one claim four ways. `ASSERT_*` is `assertish` already.
/// `EXPECT_*` is gtest's non-fatal twin and asserts as much. Catch2's
/// `REQUIRE`/`CHECK` and glog's `CHECK` are the same sentence again.
/// Requiring the underscore keeps ordinary names — `checked()`,
/// `expected_value()` — out of it.
fn is_an_assertion(name: &str) -> bool {
    super::assertish(name)
        || name.starts_with("EXPECT_")
        || matches!(name, "REQUIRE" | "CHECK")
        || name.starts_with("REQUIRE_")
        || name.starts_with("CHECK_")
}

/// `abort()` and `std::terminate()` end the process where an error
/// belonged. `throw` is a declared path and is not one.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_name(call, src), Some("abort" | "terminate"))
}

/// The gtest family, the one that parses: every one of these takes two
/// identifiers and a block. Catch2's string-argument macros are absent
/// because the grammar rejects them, and google/benchmark's
/// `BENCHMARK(BM_Foo);` is a statement naming a function defined
/// elsewhere, not a declaration of anything.
fn declares_test(node: Node, src: &[u8]) -> bool {
    let Some(name) = test_macro(node).and_then(|(m, _)| m.utf8_text(src).ok()) else {
        return false;
    };
    matches!(
        name,
        "TEST" | "TEST_F" | "TEST_P" | "TYPED_TEST" | "TYPED_TEST_P"
    )
}

/// The `this` of dynamic dispatch is `dynamic_cast`, and the escape
/// hatches are the same as C's plus `reinterpret_cast`, which the
/// kinds table already prices as a Cast. longjmp is the one
/// unpredictable construct left, as in C.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && callee_name(node, src).is_some_and(|t| matches!(t, "longjmp" | "setjmp" | "siglongjmp"))
}

// ---------------------------------------------------------------------
// Reading a dialect the grammar does not know yet
// ---------------------------------------------------------------------

/// The type keywords that cannot stand alone as an expression. `^^int`
/// reflects on one of these, and the operand has to become a name
/// before the grammar reads the line.
const BUILTIN_TYPES: [&[u8]; 16] = [
    b"void",
    b"bool",
    b"char",
    b"char8_t",
    b"char16_t",
    b"char32_t",
    b"wchar_t",
    b"short",
    b"int",
    b"long",
    b"signed",
    b"unsigned",
    b"float",
    b"double",
    b"auto",
    b"nullptr_t",
];

/// The keywords that take a parenthesized condition. A `)` that closes
/// one of these is the end of a condition, and never the end of the
/// parameter list of a function.
const CONDITIONS: [&[u8]; 6] = [
    b"if",
    b"while",
    b"for",
    b"switch",
    b"catch",
    b"synchronized",
];

/// The words that can stand between the parameter list of a function
/// and a contract clause.
const QUALIFIERS: [&[u8]; 6] = [
    b"const",
    b"volatile",
    b"noexcept",
    b"override",
    b"final",
    b"requires",
];

/// Which bytes are code, and not comment or literal text. Every rewrite
/// reads this first. A `^^` inside a string is a string, and a `pre(`
/// in a doc comment is prose.
///
/// Three lexical rules keep the mask true, and each one of them is a
/// defect if it is missing:
///
/// - A line comment continues past the line break when the line ends
///   with a backslash. Translation phase 2 joins the two lines before
///   the compiler reads a single token.
/// - A quote does not open a literal when no quote closes it on the
///   same line. Neither a string nor a character literal holds a raw
///   line break, so an apostrophe in prose stays prose.
/// - An apostrophe between two digits is a digit separator. `1'000'000`
///   is one number, and the C++14 separator in it opens nothing.
///
/// Without the last two rules, one apostrophe in `#error can't` or in
/// `1'000` marks the rest of the file as text. Every rewrite below it
/// then stops, and the file goes to the parser unchanged.
fn code_mask(src: &[u8]) -> Vec<bool> {
    let mut code = vec![true; src.len()];
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            b'/' if src.get(i + 1) == Some(&b'/') => {
                let end = line_comment_end(src, i);
                code[i..end].fill(false);
                i = end;
            }
            b'/' if src.get(i + 1) == Some(&b'*') => {
                let end = find(src, i + 2, b"*/").map_or(src.len(), |e| e + 2);
                code[i..end].fill(false);
                i = end;
            }
            // A raw string has no escapes, so its own delimiter is the
            // only way out: R"tag( ... )tag". The `L`, `u8`, `u` and `U`
            // prefixes sit before the `R`, and the scan meets the `R`
            // itself, so each prefix needs no rule of its own.
            b'R' if src.get(i + 1) == Some(&b'"') => {
                let open = i + 2;
                let Some(paren) = src[open..]
                    .iter()
                    .position(|&c| c == b'(')
                    .map(|p| open + p)
                else {
                    i += 1;
                    continue;
                };
                let mut close = Vec::with_capacity(paren - open + 2);
                close.push(b')');
                close.extend_from_slice(&src[open..paren]);
                close.push(b'"');
                let end = find(src, paren + 1, &close).map_or(src.len(), |e| e + close.len());
                code[i..end].fill(false);
                i = end;
            }
            // `#error` and `#warning` carry prose, and prose carries
            // apostrophes. The text after them is not a token sequence.
            b'#' if directive_is_prose(src, i) => {
                let end = line_comment_end(src, i);
                code[i..end].fill(false);
                i = end;
            }
            b'\'' if digit_separator(src, i) => i += 1,
            b'"' | b'\'' => match literal_end(src, i) {
                Some(end) => {
                    code[i..end].fill(false);
                    i = end;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    code
}

/// The end of the line comment that opens at `at`, past every line that
/// a backslash joins to it.
fn line_comment_end(src: &[u8], at: usize) -> usize {
    let mut i = at;
    loop {
        while i < src.len() && src[i] != b'\n' {
            i += 1;
        }
        if i >= src.len() || !spliced(src, i) {
            return i;
        }
        i += 1;
    }
}

/// True when the line break at `nl` joins its line to the next one. A
/// line that ends with a backslash is one logical line with the line
/// that follows it.
fn spliced(src: &[u8], nl: usize) -> bool {
    let mut k = nl;
    if k > 0 && src[k - 1] == b'\r' {
        k -= 1;
    }
    k > 0 && src[k - 1] == b'\\'
}

/// True when the `#` at `at` starts a directive whose argument is prose.
fn directive_is_prose(src: &[u8], at: usize) -> bool {
    let Some(word) = (at + 1..src.len()).find(|&k| !matches!(src[k], b' ' | b'\t')) else {
        return false;
    };
    word_at(src, word, b"error") || word_at(src, word, b"warning")
}

/// True when the apostrophe at `at` separates the digits of a number.
/// C++14 writes `1'000'000` and `0xDEAD'BEEF`, and neither apostrophe
/// opens a character literal. The run that ends at the apostrophe tells
/// the two apart: a number starts with a digit, and the `L`, `u8`, `u`
/// and `U` prefixes of a character literal do not.
fn digit_separator(src: &[u8], at: usize) -> bool {
    if at == 0 || !src[at - 1].is_ascii_alphanumeric() {
        return false;
    }
    let mut start = at;
    while start > 0 && (is_word(src[start - 1]) || src[start - 1] == b'\'') {
        start -= 1;
    }
    src[start].is_ascii_digit()
}

/// The byte after the literal that opens at `at`, or `None` when the
/// quote does not close on the same logical line. A string and a
/// character literal both end on the line that starts them, unless a
/// backslash joins that line to the next.
fn literal_end(src: &[u8], at: usize) -> Option<usize> {
    let quote = src[at];
    let mut i = at + 1;
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'\n' if !spliced(src, i) => return None,
            byte if byte == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() || needle.len() > hay.len() - from {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The index of the bracket that closes the `open` bracket at `at`,
/// counted over code bytes only.
fn match_close(src: &[u8], code: &[bool], at: usize, open: u8, shut: u8) -> Option<usize> {
    if !code.get(at).copied().unwrap_or(false) || src[at] != open {
        return None;
    }
    let mut depth = 0u32;
    for (i, &byte) in src.iter().enumerate().skip(at) {
        if !code[i] {
            continue;
        }
        if byte == open {
            depth += 1;
        } else if byte == shut {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn match_paren(src: &[u8], code: &[bool], at: usize) -> Option<usize> {
    match_close(src, code, at, b'(', b')')
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Overwrite a span with `fill`, and leave the line breaks where they
/// are. A `delete("...")` reason or a contract predicate can run across
/// several lines. A rewrite that removes those breaks moves every
/// finding below it onto the wrong line.
fn blank(out: &mut [u8], range: std::ops::Range<usize>, fill: u8) {
    for byte in &mut out[range] {
        if *byte != b'\n' && *byte != b'\r' {
            *byte = fill;
        }
    }
}

/// Write `text` at the start of a span and blank the rest of it.
///
/// False when the span cannot hold the text, or when a line break runs
/// through where the text would go. Nothing is written then, because a
/// rewrite that grows the source or eats a line break moves every
/// finding below it. Each caller has an answer for false that is safe.
fn overwrite(out: &mut [u8], range: std::ops::Range<usize>, text: &[u8]) -> bool {
    let start = range.start;
    if range.len() < text.len() || out[start..start + text.len()].contains(&b'\n') {
        return false;
    }
    blank(out, range, b' ');
    out[start..start + text.len()].copy_from_slice(text);
    true
}

/// The last code byte before `at` that is not whitespace.
fn prev_code(src: &[u8], code: &[bool], at: usize) -> Option<usize> {
    (0..at)
        .rev()
        .find(|&i| code[i] && !src[i].is_ascii_whitespace())
}

/// The first code byte at or after `at` that is not whitespace.
fn next_code(src: &[u8], code: &[bool], at: usize) -> Option<usize> {
    (at..src.len()).find(|&i| code[i] && !src[i].is_ascii_whitespace())
}

/// The word that ends at `end`, when one does.
fn word_before(src: &[u8], end: usize) -> &[u8] {
    let mut start = end;
    while start > 0 && is_word(src[start - 1]) {
        start -= 1;
    }
    &src[start..end]
}

/// The end of the word that starts at `at`.
fn word_end(src: &[u8], at: usize) -> usize {
    let mut end = at;
    while end < src.len() && is_word(src[end]) {
        end += 1;
    }
    end
}

/// True when `at` starts the word `want` and no word byte runs into it.
fn word_at(src: &[u8], at: usize, want: &[u8]) -> bool {
    src[at..].starts_with(want)
        && !src.get(at + want.len()).is_some_and(|&c| is_word(c))
        && (at == 0 || !is_word(src[at - 1]))
}

/// True when `at` sits in a `[...]` that opens in the same statement.
/// A structured binding pack is the one ellipsis that stands in
/// brackets and before a name.
fn inside_brackets(src: &[u8], code: &[bool], at: usize) -> bool {
    let mut depth = 0u32;
    for i in (0..at).rev() {
        if !code[i] {
            continue;
        }
        match src[i] {
            b']' => depth += 1,
            b'[' if depth == 0 => return true,
            b'[' => depth -= 1,
            b';' | b'{' | b'}' => return false,
            _ => {}
        }
    }
    false
}

/// True when the `)` at `at` closes the condition of an `if`, a `while`
/// or another statement that takes one. A contract clause follows the
/// parameter list of a function, and `if (ready) pre(x);` is a call.
fn closes_a_condition(src: &[u8], code: &[bool], at: usize) -> bool {
    let mut depth = 0u32;
    for i in (0..=at).rev() {
        if !code[i] {
            continue;
        }
        match src[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return prev_code(src, code, i)
                        .is_some_and(|p| CONDITIONS.contains(&word_before(src, p + 1)));
                }
            }
            _ => {}
        }
    }
    false
}

/// The keywords that an expression can follow. A declarator holds none
/// of them, so one of them before a `(` makes that `(` the start of an
/// argument list.
const EXPRESSIONS: [&[u8]; 12] = [
    b"return",
    b"co_return",
    b"co_yield",
    b"co_await",
    b"throw",
    b"case",
    b"new",
    b"delete",
    b"sizeof",
    b"alignof",
    b"typeid",
    b"goto",
];

/// The `(` that opens the parameter list that `close` ends.
fn open_paren_of(src: &[u8], code: &[bool], close: usize) -> Option<usize> {
    let mut depth = 0u32;
    for i in (0..=close).rev() {
        if !code[i] {
            continue;
        }
        match src[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// True when the parameter list that ends at `close` belongs to a
/// declaration, and not to a call in an expression.
///
/// The scan steps over the parameter list and reads back to the start
/// of the statement. Four things end it. A `]` directly before the list
/// is the introducer of a lambda, and a lambda takes a contract clause
/// of its own. An unmatched `(` or `[` means an argument list or a
/// subscript holds the call, as in `f(g() & pre(h))`. An `=` or one of
/// the expression keywords before it means an expression, as in
/// `return g() & pre(2)`. A statement boundary means a declaration.
fn declares_a_function(src: &[u8], code: &[bool], close: usize) -> bool {
    let Some(open) = open_paren_of(src, code, close) else {
        return false;
    };
    if prev_code(src, code, open).is_some_and(|p| src[p] == b']') {
        return true;
    }
    let mut depth = 0u32;
    let mut i = open;
    while i > 0 {
        i -= 1;
        if !code[i] {
            continue;
        }
        match src[i] {
            b')' | b']' => depth += 1,
            b'(' | b'[' if depth == 0 => return false,
            b'(' | b'[' => depth -= 1,
            b'=' | b'?' if depth == 0 => return false,
            b';' | b'{' | b'}' if depth == 0 => return true,
            byte if depth == 0 && is_word(byte) => {
                let word = word_before(src, i + 1);
                if EXPRESSIONS.contains(&word) {
                    return false;
                }
                i -= word.len() - 1;
            }
            _ => {}
        }
    }
    true
}

/// The position after a contract clause that starts at `at`, or `None`
/// when `at` starts a call instead.
///
/// P2900 gives `pre` and `post` no keyword of their own, so the two
/// read as ordinary names. Only the position tells a clause from a
/// call, and the tool answers the question from three sides. A wrong
/// answer in one direction removes real work and under-reports
/// complexity, and that failure is silent. A wrong answer in the other
/// direction leaves a parse error, which `--errors` reports.
///
/// A clause needs all three of these:
///
/// 1. The bytes before it belong to a declarator. A clause follows the
///    parameter list, a cv-qualifier or ref-qualifier, `noexcept`, a
///    virt-specifier, or a trailing return type.
/// 2. The `)` it reads back to closes a parameter list. It does not
///    close the condition of an `if`, and the declarator it ends is not
///    a call in an expression.
/// 3. A body or another clause follows it. A call is followed by an
///    operator or an argument.
fn contract_clause(src: &[u8], code: &[bool], at: usize, name_end: usize) -> Option<usize> {
    let open = next_code(src, code, name_end).filter(|&k| src[k] == b'(')?;
    let close = match_paren(src, code, open)?;

    // 3. What follows a clause is a body, a declaration end, another
    //    clause, a trailing return type, or the initializer list of a
    //    constructor. A call is followed by an operator or an argument.
    let follows = next_code(src, code, close + 1)?;
    let tail_ok = matches!(src[follows], b'{' | b';' | b'-' | b'=' | b':')
        || word_at(src, follows, b"pre")
        || word_at(src, follows, b"post")
        || QUALIFIERS.contains(&&src[follows..word_end(src, follows)]);
    if !tail_ok {
        return None;
    }

    // 1. Read back over the tokens a declarator can end with. A word is
    //    a qualifier, or part of a trailing return type, and a trailing
    //    return type has to hold a type name before its `->`.
    let mut saw_word = false;
    let mut i = prev_code(src, code, at)?;
    loop {
        let byte = src[i];
        if byte == b')' {
            // 2. The parameter list of a declaration, not a condition
            //    and not a call.
            return (!closes_a_condition(src, code, i) && declares_a_function(src, code, i))
                .then_some(close + 1);
        }
        if is_word(byte) {
            // A qualifier, or the type in a trailing return type. The
            // arrow below is what tells the second one from a member
            // access, and it needs a type name to have come first.
            let start = i + 1 - word_before(src, i + 1).len();
            saw_word = true;
            i = prev_code(src, code, start)?;
            continue;
        }
        match byte {
            // A ref-qualifier, or a pointer or reference in a trailing
            // return type.
            b'&' => {}
            // `->` opens a trailing return type. Without a type name
            // between the arrow and the clause, the arrow is a member
            // access and `pre` is the member.
            b'>' if prev_code(src, code, i).is_some_and(|p| src[p] == b'-') => {
                if !saw_word {
                    return None;
                }
                i = prev_code(src, code, i)?;
            }
            // The rest of a trailing return type.
            b'*' | b':' | b',' | b'<' | b'>' | b'~' if saw_word => {}
            _ => return None,
        }
        i = prev_code(src, code, i)?;
    }
}

/// The end of the operand of a `^^` that cannot stand as an expression.
/// `^^int` reflects on a type keyword, and `^^::` on the global
/// namespace. Neither is a name, so both become one.
fn reflect_operand_end(src: &[u8], code: &[bool], at: usize) -> Option<usize> {
    let start = next_code(src, code, at)?;
    if src[start..].starts_with(b"::") {
        return Some(start + 2);
    }
    let mut end = None;
    let mut i = start;
    while let Some(word) = next_code(src, code, i) {
        if word > i && end.is_none() {
            break;
        }
        let stop = word_end(src, word);
        if stop == word || !BUILTIN_TYPES.contains(&&src[word..stop]) {
            break;
        }
        end = Some(stop);
        i = stop;
    }
    end
}

/// True when a qualified name follows `at`. `typename` before one is a
/// disambiguator the grammar reads without, and `typename` in a
/// template parameter list is followed by a plain name instead.
fn qualified_after(src: &[u8], code: &[bool], at: usize) -> bool {
    let Some(start) = next_code(src, code, at) else {
        return false;
    };
    if src[start..].starts_with(b"::") {
        return true;
    }
    let name_end = word_end(src, start);
    name_end > start && next_code(src, code, name_end).is_some_and(|k| src[k..].starts_with(b"::"))
}

/// True when the `=` at `at` follows the designator of a braced
/// initializer, as `.pad = {}` does.
///
/// The `,` after such a value opens the next designator, and not the
/// next parameter, so the value is not a default argument. Blanking it
/// leaves `.pad ,` and the list around it stops parsing.
fn designator_before(src: &[u8], code: &[bool], at: usize) -> bool {
    let Some(prev) = prev_code(src, code, at) else {
        return false;
    };
    let word = word_before(src, prev + 1);
    let start = prev + 1 - word.len();
    !word.is_empty() && start > 0 && src[start - 1] == b'.'
}

/// True when `at` sits on a preprocessor directive line.
///
/// The name a directive spells is not a declarator. Blanking
/// `CRUCIBLE_INLINE` in the `#define` that writes it leaves `#define`
/// with nothing to define, and every declaration under it stops
/// parsing. `#undef` and `#ifdef` name it the same way.
fn on_a_directive(src: &[u8], at: usize) -> bool {
    (0..at)
        .rev()
        .take_while(|&k| src[k] != b'\n')
        .filter(|&k| !src[k].is_ascii_whitespace())
        .last()
        .is_some_and(|k| src[k] == b'#')
}

/// True when a string literal ends earlier on the same line as `at`,
/// with whitespace between the two.
///
/// A name in that position can only be a macro that expands to a
/// string: `"%016" PRIx64` is one literal to the compiler, and C++ puts
/// nothing else after a literal. Two details carry the rule.
///
/// The space is one. A user-defined literal writes its suffix against
/// the quote, so `""_km` names an operator and keeps its name.
///
/// The line is the other, and without it the rule is a defect. An
/// include line ends with a quote, and the declaration under it opens
/// with a name: `#include "config.h"` over `namespace crucible {`
/// blanks the namespace and every file that holds one stops parsing.
fn follows_a_string(src: &[u8], code: &[bool], at: usize) -> bool {
    if at == 0 || !src[at - 1].is_ascii_whitespace() {
        return false;
    }
    let Some(quote) = (0..at)
        .rev()
        .take_while(|&k| src[k] != b'\n')
        .find(|&k| !src[k].is_ascii_whitespace())
        .filter(|&k| src[k] == b'"' && !code[k])
    else {
        return false;
    };
    // Two declarations put a name after a string of their own.
    // `extern "C" int f()` states a linkage, and `operator "" _km`
    // names a literal operator. The word before the string tells them
    // from a macro, so the scan reads back over the literal to find it.
    let opens = (0..=quote)
        .rev()
        .take_while(|&k| !code[k])
        .last()
        .unwrap_or(quote);
    !prev_code(src, code, opens)
        .is_some_and(|p| matches!(word_before(src, p + 1), b"extern" | b"operator"))
}

/// C++ syntax the bundled grammar cannot read, rewritten into syntax it
/// can, with every byte offset left where it was.
///
/// A parse error is not local. The node that fails swallows the rest of
/// its scope, so one `= delete("reason")` near the top of a header
/// re-parents every function below it. The loss is silent. Units go
/// missing from the listing, and the units that survive carry the
/// complexity of whatever the error folded into them. That reads as a
/// real measurement, which is worse than no measurement.
///
/// So the source is normalized before the parser reads it. Every
/// rewrite here replaces a span with the SAME NUMBER OF BYTES, because
/// every offset the report prints is an offset into this text. Spaces
/// and underscores keep all of them true. A splice becomes underscores
/// and not spaces because it stands where a name stands: `obj.[:m:]`
/// has to stay a member access, and `obj.` and then blanks is not one.
///
/// | written | parsed as | paper |
/// |---|---|---|
/// | `= delete("reason")` | blanked to the declaration | P2573 |
/// | `^^T` | `  T` | P2996 |
/// | `^^int`, `^^::` | `_____` | P2996 |
/// | `[:e:]` | `_____` | P2996 |
/// | `[[=annotation]]` | blanked | P3394 |
/// | `template for (...)` | `         for (...)` | P1306 |
/// | `f() noexcept pre(p)` | `f() noexcept` | P2900 |
/// | `T...[0]` | `T` | P2662 |
/// | `auto [x, ...rest]` | `auto [x,    rest]` | P1061 |
/// | `f(this Self& self)` | `f(     Self& self)` | P0847 |
/// | `if consteval` | `if (true)   ` | P1938 |
/// | `export module m;` | blanked | P1103 |
///
/// None of this is an opinion about the code. It is the smallest edit
/// that lets the grammar read the shape the compiler reads.
///
/// The grammar moves, and the language moves faster. A construct that
/// no rule here covers stays a parse error, and `--errors` names the
/// file and the line. That is the signal to add a rule. The failure
/// mode is loud on purpose.
pub fn normalize(src: &str, macros: &crate::clangfmt::Macros) -> Option<String> {
    let bytes = src.as_bytes();
    if !worth_reading(bytes) && macros.is_empty() {
        return None;
    }

    let code = code_mask(bytes);
    let mut out = bytes.to_vec();
    let mut touched = false;
    let mut i = 0;
    while i < out.len() {
        if !code[i] {
            i += 1;
            continue;
        }
        // Blank a span, record the edit, and go on from the end of it.
        macro_rules! cut {
            ($range:expr, $fill:expr) => {{
                let range = $range;
                blank(&mut out, range.clone(), $fill);
                touched = true;
                i = range.end;
                continue;
            }};
        }
        // P2996 reflect. The operand is an ordinary expression once the
        // operator is gone, unless the operand is a type keyword or the
        // global namespace, and neither of those is an expression.
        if out[i] == b'^' && out.get(i + 1) == Some(&b'^') {
            match reflect_operand_end(&out, &code, i + 2) {
                Some(end) => cut!(i..end, b'_'),
                None => cut!(i..i + 2, b' '),
            }
        }
        // P2996 splice. Splices nest, so the depth answers where one
        // ends, and the first `:]` does not.
        if out[i] == b'[' && out.get(i + 1) == Some(&b':') {
            let mut depth = 0u32;
            let mut j = i;
            let end = loop {
                if j + 1 >= out.len() {
                    break None;
                }
                if code[j] && out[j] == b'[' && out[j + 1] == b':' {
                    depth += 1;
                    j += 2;
                } else if code[j] && out[j] == b':' && out[j + 1] == b']' {
                    depth -= 1;
                    if depth == 0 {
                        break Some(j + 2);
                    }
                    j += 2;
                } else {
                    j += 1;
                }
            };
            if let Some(end) = end {
                cut!(i..end, b'_');
            }
        }
        // P3394 annotation. An attribute that starts with `=` carries
        // an expression, and an attribute is not a unit of complexity.
        if out[i] == b'['
            && out.get(i + 1) == Some(&b'[')
            && next_code(&out, &code, i + 2).is_some_and(|k| out[k] == b'=')
            && let Some(close) = match_close(&out, &code, i, b'[', b']')
            && close > i + 2
            && out[close - 1] == b']'
        {
            cut!(i..close + 1, b' ');
        }
        // P2662 pack indexing, and P1061 structured binding packs. An
        // index into a pack carries no complexity. An ellipsis before a
        // name in brackets binds the rest of the pack, and the names
        // that remain read as an ordinary binding.
        if out[i] == b'.' && out[i..].starts_with(b"...") {
            let after = next_code(&out, &code, i + 3);
            if let Some(open) = after.filter(|&k| out[k] == b'[')
                && let Some(close) = match_close(&out, &code, open, b'[', b']')
            {
                cut!(i..close + 1, b' ');
            }
            if after.is_some_and(|k| is_word(out[k]) && !out[k].is_ascii_digit())
                && prev_code(&out, &code, i).is_some_and(|p| matches!(out[p], b'[' | b','))
                && inside_brackets(&out, &code, i)
            {
                cut!(i..i + 3, b' ');
            }
        }
        // P1306 expansion statement. Drop `template` and keep the `for`.
        if word_at(&out, i, b"template") {
            let after = i + b"template".len();
            if next_code(&out, &code, after).is_some_and(|k| word_at(&out, k, b"for")) {
                cut!(i..after, b' ');
            }
        }
        // P2573 `= delete("reason")`. The whole initializer goes, and
        // not only the reason. The grammar has no deleted function
        // outside a class body, so `= delete` alone fails where the
        // reason string made the line parse — and it fails wrongly, as
        // a variable with a call for an initializer. What remains is
        // the declaration, which is what a deleted function is.
        if word_at(&out, i, b"delete")
            && let Some(eq) = prev_code(&out, &code, i).filter(|&p| out[p] == b'=')
            && let Some(open) =
                next_code(&out, &code, i + b"delete".len()).filter(|&k| out[k] == b'(')
            && let Some(close) = match_paren(&out, &code, open)
        {
            cut!(eq..close + 1, b' ');
        }
        // P2900 contract clause.
        if word_at(&out, i, b"pre") || word_at(&out, i, b"post") {
            let name_end = word_end(&out, i);
            if let Some(end) = contract_clause(&out, &code, i, name_end) {
                cut!(i..end, b' ');
            }
        }
        // P0847 explicit object parameter. `this` names the parameter,
        // and a type follows it. A `this` that a call passes is followed
        // by `)` or `,` and stays.
        if word_at(&out, i, b"this")
            && prev_code(&out, &code, i).is_some_and(|p| out[p] == b'(')
            && next_code(&out, &code, i + b"this".len()).is_some_and(|k| is_word(out[k]))
        {
            cut!(i..i + b"this".len(), b' ');
        }
        // P1938 `if consteval`. The branch is an ordinary branch, and
        // the condition it takes is a constant.
        if word_at(&out, i, b"consteval") {
            let end = i + b"consteval".len();
            let bang = prev_code(&out, &code, i).filter(|&p| out[p] == b'!');
            let start = bang.unwrap_or(i);
            let before = prev_code(&out, &code, start);
            if before.is_some_and(|p| word_before(&out, p + 1) == b"if")
                && overwrite(&mut out, start..end, b"(true)")
            {
                touched = true;
                i = end;
                continue;
            }
        }
        // P1103 modules. The grammar reads no module declaration, and
        // `export` before an ordinary declaration is the whole of what
        // it adds to one.
        if word_at(&out, i, b"export") || word_at(&out, i, b"module") || word_at(&out, i, b"import")
        {
            if let Some(end) = module_declaration(&out, &code, i) {
                cut!(i..end, b' ');
            }
            if word_at(&out, i, b"export") {
                cut!(i..i + b"export".len(), b' ');
            }
        }
        // A braced default argument. The grammar reads `= Cfg{}` and
        // `= 0`, and no `= {}`, which is ordinary C++11. What follows
        // the closing brace is what says the value is one: a default
        // argument ends at the next parameter or at the parameter list.
        //
        // An empty pair states value-initialization and carries no
        // complexity, so it goes. A pair with something in it becomes
        // parentheses instead of blanks, so that a call written inside
        // a default is still a call that this tool counts.
        if out[i] == b'='
            && !designator_before(&out, &code, i)
            && let Some(open) = next_code(&out, &code, i + 1).filter(|&k| out[k] == b'{')
            && let Some(close) = match_close(&out, &code, open, b'{', b'}')
            && next_code(&out, &code, close + 1).is_some_and(|k| matches!(out[k], b',' | b')'))
        {
            if next_code(&out, &code, open + 1) == Some(close) {
                cut!(i..close + 1, b' ');
            }
            out[open] = b'(';
            out[close] = b')';
            touched = true;
            i = close + 1;
            continue;
        }
        // A qualified name after `typename`. The keyword disambiguates
        // for the compiler and the grammar has no rule for it here.
        if word_at(&out, i, b"typename") && qualified_after(&out, &code, i + b"typename".len()) {
            cut!(i..i + b"typename".len(), b' ');
        }
        // A name that follows a string literal expands to one.
        if is_word(out[i]) && (i == 0 || !is_word(out[i - 1])) && follows_a_string(&out, &code, i) {
            cut!(i..word_end(&out, i), b' ');
        }
        // A macro the project declared to clang-format. The grammar
        // has no rule for a bare token in a declarator, and the name is
        // the only thing that says one is there.
        if !macros.is_empty()
            && is_word(out[i])
            && (i == 0 || !is_word(out[i - 1]))
            && !on_a_directive(&out, i)
            && let Some(kind) = macros.kind(&out[i..word_end(&out, i)])
        {
            let name_end = word_end(&out, i);
            // The argument list, when the macro takes one. It goes with
            // the name, because what remains has to stand on its own
            // and `(a, b);` is not a declaration at namespace scope.
            let args = next_code(&out, &code, name_end)
                .filter(|&k| out[k] == b'(')
                .and_then(|open| match_paren(&out, &code, open).map(|close| (open, close)));
            let call_end = args.map_or(name_end, |(_, close)| close + 1);
            match kind {
                // It expands to an attribute, which is not complexity.
                Kind::Attribute => cut!(i..name_end, b' '),
                Kind::Statement => cut!(i..call_end, b' '),
                // The body stays a loop, because the macro writes one.
                // A name too short to hold `for (;;)` leaves a plain
                // block, which parses and counts one loop less.
                Kind::ForEach => {
                    if overwrite(&mut out, i..call_end, b"for (;;)") {
                        touched = true;
                        i = call_end;
                        continue;
                    }
                    cut!(i..call_end, b' ')
                }
                // The branch stays a branch, and it keeps its condition.
                Kind::Branch => {
                    if overwrite(&mut out, i..name_end, b"if") {
                        touched = true;
                        i = name_end;
                        continue;
                    }
                    cut!(i..call_end, b' ')
                }
                // The argument IS the type, so only the name and the
                // two parentheses around it go, and the type stays
                // where the declaration put it.
                Kind::Typename => {
                    if let Some((open, close)) = args {
                        blank(&mut out, i..open + 1, b' ');
                        blank(&mut out, close..close + 1, b' ');
                        touched = true;
                        i = close + 1;
                        continue;
                    }
                    cut!(i..name_end, b' ')
                }
                Kind::Namespace => {
                    if overwrite(&mut out, i..call_end, b"namespace") {
                        touched = true;
                        i = call_end;
                        continue;
                    }
                    cut!(i..call_end, b' ')
                }
            }
        }
        i += 1;
    }
    // A span is bounded by ASCII tokens, so a rewrite covers every byte
    // of any character inside it. If some file proves otherwise, the
    // original text is still measurable, and a panic would not be.
    touched.then(|| String::from_utf8(out).ok()).flatten()
}

/// The end of a module declaration that starts at `at`, or `None` when
/// `at` starts something else. `module` and `import` are context
/// keywords, so a declaration has to start a statement, and a call such
/// as `import(path)` keeps its parentheses and stays.
fn module_declaration(src: &[u8], code: &[bool], at: usize) -> Option<usize> {
    let starts_statement =
        prev_code(src, code, at).is_none_or(|p| matches!(src[p], b';' | b'{' | b'}'));
    if !starts_statement {
        return None;
    }
    let mut i = at;
    if word_at(src, i, b"export") {
        let after = next_code(src, code, i + b"export".len())?;
        if !word_at(src, after, b"module") && !word_at(src, after, b"import") {
            return None;
        }
        i = after;
    }
    if !word_at(src, i, b"module") && !word_at(src, i, b"import") {
        return None;
    }
    // `import(x)` is a call, and `module = 1` is an assignment. Neither
    // word is reserved, so both stay where a declaration does not follow.
    let after = next_code(src, code, word_end(src, i));
    if after.is_none_or(|k| matches!(src[k], b'(' | b'=')) {
        return None;
    }
    let end = (i..src.len()).find(|&k| code[k] && src[k] == b';')?;
    Some(end + 1)
}

/// True when the source holds a byte that one of the rewrites needs.
/// The scan below costs two allocations of the length of the file, and
/// a file of plain C++ pays for neither.
fn worth_reading(src: &[u8]) -> bool {
    src.windows(2)
        .any(|w| w == b"^^" || w == b"[:" || w == b"..")
        // A brace that opens a value, which is the shape of a braced
        // default argument. And a quote, a space, and the start of a
        // name, which is a macro that expands to a string.
        || src.windows(2).any(|w| w == b"={")
        || src.windows(3).any(|w| w == b"= {")
        || src.windows(3).any(|w| {
            w[0] == b'"' && w[1].is_ascii_whitespace() && (w[2].is_ascii_alphabetic() || w[2] == b'_')
        })
        || [
            &b"delete"[..],
            b"template",
            b"pre",
            b"post",
            b"this",
            b"consteval",
            b"typename",
            b"export",
            b"module",
            b"import",
            b"[[",
        ]
        .iter()
        .any(|needle| find(src, 0, needle).is_some())
}

#[cfg(test)]
mod tests {
    use super::normalize as rewrite;
    use crate::clangfmt::Macros;

    /// A project that declares nothing, which is what every test about
    /// the standard syntax measures against.
    fn normalize(src: &str) -> Option<String> {
        rewrite(src, &Macros::default())
    }

    /// The same rewrite, against a project that declared these names.
    fn with_macros(src: &str, yaml: &str) -> Option<String> {
        rewrite(src, &Macros::read(yaml))
    }
    use crate::facts::extract;
    use crate::lang::Lang;
    use crate::sem::Sem;
    use std::path::Path;

    fn facts(src: &str) -> crate::facts::FileFacts {
        let pack = Lang::Cpp.pack();
        extract(pack, &mut pack.make_parser(), Path::new("t.cpp"), src)
    }

    fn cuda(src: &str) -> crate::facts::FileFacts {
        let pack = Lang::Cuda.pack();
        extract(pack, &mut pack.make_parser(), Path::new("k.cu"), src)
    }

    #[test]
    fn a_kernel_launch_is_one_call_and_a_kernel_is_an_ordinary_unit() {
        // `f<<<g, b>>>(x)` is a call_expression that carries an extra
        // `kernel_call_syntax` child. Pricing that child as a call too
        // would read every launch in a CUDA codebase as two.
        let f = cuda(
            "__global__ void add(float* a, int n) {\n\
             \x20 int i = blockIdx.x * blockDim.x + threadIdx.x;\n\
             \x20 if (i < n) a[i] += 1.0f;\n\
             }\n\n\
             void host(float* d_a, int n) {\n\
             \x20 add<<<grid, block>>>(d_a, n);\n\
             }\n",
        );
        assert!(!f.low_confidence(), "the CUDA grammar must read CUDA");
        let names: Vec<&str> = f.units[1..].iter().map(|u| &*u.name).collect();
        assert_eq!(names, ["add", "host"], "a kernel is an ordinary unit");
        let add = f.units.iter().find(|u| &*u.name == "add").expect("add");
        assert_eq!(add.params.len(), 2, "__global__ hides no parameters");
        assert_eq!(
            add.ctrl.iter().filter(|c| c.sem == Sem::If).count(),
            1,
            "a kernel's guard is an ordinary branch"
        );
        // And the same source is unreadable to the plain C++ grammar,
        // which is why the dialect exists.
        assert!(
            facts("void host(float* a, int n) {\n  add<<<grid, block>>>(a, n);\n}\n")
                .low_confidence(),
            "if C++ could read a launch there would be nothing to add"
        );
    }

    #[test]
    fn an_out_of_line_method_keeps_its_class() {
        // `int Store::get(...)` carries its class in a qualified
        // declarator, and the unit takes that node's `name` child.
        let f =
            facts("int Store::get(const std::string& k) const {\n    return items_.at(0);\n}\n");
        let unit = &f.units[1];
        assert_eq!(&*unit.name, "get");
        assert_eq!(&*unit.qualname, "get", "the scope rides in the declarator");
        assert_eq!(unit.params.len(), 1);
        assert_eq!(&*unit.params[0].name, "k");
    }

    #[test]
    fn a_class_of_pure_virtuals_is_an_interface_and_a_base_class_is_not() {
        let iface = facts(
            "struct Reader {\n  virtual ~Reader() = default;\n  virtual int read(char* b) = 0;\n  virtual int size() = 0;\n};\n",
        );
        assert_eq!(iface.interfaces.len(), 1);
        assert_eq!(&*iface.interfaces[0].name, "Reader");
        assert_eq!(iface.interfaces[0].methods, 2, "the destructor is not one");
        // One concrete method makes it a base class, which is a
        // different thing and declares no contract.
        let base = facts(
            "struct Partial {\n  virtual int read(char* b) = 0;\n  int size() { return 1; }\n};\n",
        );
        assert!(base.interfaces.is_empty());
    }

    #[test]
    fn access_specifiers_decide_the_surface() {
        let f = facts(
            "class Store {\npublic:\n  int get() { return 1; }\nprivate:\n  int helper() { return 2; }\n};\n",
        );
        let get = f.units.iter().find(|u| &*u.name == "get").expect("get");
        let helper = f
            .units
            .iter()
            .find(|u| &*u.name == "helper")
            .expect("helper");
        assert!(get.is_public, "public: is the surface");
        assert!(!helper.is_public, "private: is not");
    }

    #[test]
    fn a_gtest_macro_declares_a_test_named_after_its_case() {
        // The grammar can only read `TEST(Pool, Takes) { ... }` as a
        // function called TEST. Left there, every gtest unit in every
        // C++ repository reports under the macro's four letters.
        let f = facts(
            "TEST(PoolTest, TakesAKnownSlot) {\n  EXPECT_EQ(take(\"a\"), 1);\n}\n\nint Pool::take(const std::string& k) { return 1; }\n",
        );
        let named: Vec<(&str, bool, u16)> = f.units[1..]
            .iter()
            .map(|u| (&*u.name, u.named_test, u.assert_calls))
            .collect();
        assert_eq!(
            named,
            [("PoolTest.TakesAKnownSlot", true, 1), ("take", false, 0)],
            "gtest prints suite.case, and EXPECT_ asserts as much as ASSERT_"
        );
        // A constructor has no return type either; its parameters tell
        // the two apart.
        let ctor = facts("struct S {\n  S(int a, int b) { x = a + b; }\n};\n");
        assert_eq!(&*ctor.units[1].name, "S");
        assert!(!ctor.units[1].named_test);
    }

    #[test]
    fn a_using_declaration_is_not_a_module_and_an_include_in_a_class_body_is() {
        // `using` names a namespace member, and no file corresponds to a
        // C++ namespace, so treating one as an import edge could only
        // ever fill the unresolved bucket: 2,480 of the 3,236 imports
        // the gold corpus reported unresolved under cpp and cuda were
        // these.
        let f = facts(
            "#include <vector>\n#include \"local.hpp\"\nusing std::vector;\nusing namespace detail;\n",
        );
        let targets: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(targets, ["<vector>", "local.hpp"]);
        // Inside a class body `#include` is not a declaration position,
        // so the grammar yields `preproc_call`, the kind it also gives
        // `#pragma`. ctre's pcre_actions.hpp keeps 17 of its 22 includes
        // there and every one of those headers read as an orphan.
        let g = facts(
            "#pragma once\nstruct actions {\n#include \"a.inc.hpp\"\n#include <b.hpp>  // trailing\n};\n",
        );
        let inner: Vec<&str> = g.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(inner, ["a.inc.hpp", "<b.hpp>"], "#pragma names nothing");
    }

    #[test]
    fn catch_all_is_broad() {
        let f = facts("void f() {\n  try { risky(); } catch (...) { log(); }\n}\n");
        assert_eq!(f.units[1].broad_catch, 1, "catch (...) binds nothing");
    }

    fn normalized(src: &str) -> crate::facts::FileFacts {
        let pack = Lang::Cpp.pack();
        let text = normalize(src).unwrap_or_else(|| src.to_string());
        extract(pack, &mut pack.make_parser(), Path::new("t.cpp"), &text)
    }

    /// The two invariants every rewrite owes the report. A finding
    /// prints a byte offset and a line number, and both of them are
    /// offsets into the text the parser read.
    fn same_length(src: &str) {
        if let Some(out) = normalize(src) {
            assert_eq!(out.len(), src.len(), "a rewrite moved a byte offset");
            assert_eq!(
                out.bytes().filter(|&b| b == b'\n').count(),
                src.bytes().filter(|&b| b == b'\n').count(),
                "a rewrite moved a line"
            );
        }
    }

    /// Source that the grammar cannot read as written. Each one of
    /// these re-parents every declaration below it while it fails.
    const DIALECT: &[(&str, &str)] = &[
        (
            "delete with a reason",
            "struct S { S(const S&) = delete(\"no copy\"); };",
        ),
        ("reflect", "constexpr auto r = ^^Widget;"),
        ("reflect a type keyword", "constexpr auto r = ^^int;"),
        (
            "reflect unsigned long",
            "constexpr auto r = ^^unsigned long;",
        ),
        ("reflect the global namespace", "constexpr auto r = ^^::;"),
        ("splice a type", "using T = [:r:];"),
        ("splice a member", "int y = obj.[:m:];"),
        ("splice a nested splice", "auto q = [:members[:i:]:];"),
        ("annotation", "struct [[=1]] S { int f() { return 0; } };"),
        (
            "expansion statement",
            "void f() { template for (constexpr auto m : ms) { use(m); } }",
        ),
        (
            "contract on a free function",
            "int f(int n) pre(n > 0) { return n; }",
        ),
        (
            "contract with a result binding",
            "int f(int n) post(r: r > 0) { return n; }",
        ),
        (
            "contract after noexcept",
            "int f(int n) noexcept pre(n > 0) { return n; }",
        ),
        (
            "contract after const",
            "struct S { int f() const pre(ok()) { return 1; } };",
        ),
        (
            "contract after a trailing return",
            "auto f(int n) -> int pre(n > 0) { return n; }",
        ),
        (
            "contract after a ref qualifier",
            "struct S { int f() & pre(x) { return 1; } };",
        ),
        (
            "contract after an rvalue ref",
            "struct S { int f() && pre(x) { return 1; } };",
        ),
        (
            "contract after const volatile",
            "struct S { int f() const volatile pre(x) { return 1; } };",
        ),
        (
            "contract on a lambda",
            "auto g = [](int n) pre(n > 0) { return n; };",
        ),
        (
            "pack indexing",
            "template <class... T> using First = T...[0];",
        ),
        (
            "structured binding pack",
            "void f() { auto [x, ...rest] = t; }",
        ),
        (
            "explicit object parameter",
            "struct S { int f(this S& self) { return 1; } };",
        ),
        (
            "if consteval",
            "int f() { if consteval { return 1; } else { return 2; } }",
        ),
        (
            "module declaration",
            "export module widget;\nexport int f() { return 1; }\n",
        ),
    ];

    /// Ordinary C++ that happens to spell one of the words a rewrite
    /// looks for. None of it may change. A rewrite here would delete
    /// real work, and the complexity of that work would go unreported
    /// without any sign that it had.
    const ORDINARY: &[(&str, &str)] = &[
        (
            "pre and post as names",
            "int f(int x) { int y = pre(x); return post(y); }",
        ),
        (
            "a call on a member",
            "int f(A a) { return a.pre(1) + a.post(2); }",
        ),
        (
            "a call through a pointer",
            "int f(A* a) { return a->pre(1) + a->post(2); }",
        ),
        (
            "a call after a condition",
            "int f() { if (ready()) pre(1); return 0; }",
        ),
        (
            "a call after a loop head",
            "int f() { while (go()) post(1); return 0; }",
        ),
        (
            "a call behind an operator",
            "int f() { return g() * pre(2); }",
        ),
        (
            "a call behind a bitwise and",
            "int f() { return g() & pre(2); }",
        ),
        (
            "a call inside an argument",
            "int f() { return h(g() & pre(2)); }",
        ),
        (
            "a qualified call",
            "int f() { return N::pre(1) + N::post(2); }",
        ),
        (
            "a call in a ternary",
            "int f(int h) { return h ? pre(1) : post(2); }",
        ),
        ("a function named pre", "int pre(int x) { return x; }"),
        (
            "a method named pre",
            "struct S { int pre(int x) { return x; } };",
        ),
        (
            "this as an argument",
            "struct S { void f() { g(this); h(this, 1); } };",
        ),
        (
            "this through an arrow",
            "struct S { int x; int f() { return this->x; } };",
        ),
        ("a returned this", "struct S { S* f() { return this; } };"),
        (
            "an ordinary delete",
            "void f(int* p) { delete p; delete[] p; }",
        ),
        ("varargs", "void f(int, ...);"),
        (
            "a pack expansion",
            "template <class... T> void f(T... a) { g(a...); }",
        ),
        (
            "a catch-all",
            "void f() { try { g(); } catch (...) { h(); } }",
        ),
        (
            "a pack of base classes",
            "template <class... T> struct S : T... { };",
        ),
        (
            "module as an identifier",
            "int f() { int module = 1; return module; }",
        ),
        ("import as a call", "void f() { import(3); }"),
        ("exclusive or", "int f(int a, int b) { return a ^ b; }"),
        ("an attribute", "[[nodiscard]] int f() { return 1; }"),
        (
            "a capture by value",
            "void f() { auto l = [=]() { return 1; }; l(); }",
        ),
        ("a subscript", "int f(A a) { return a[1] + a[2]; }"),
        (
            "consteval as a specifier",
            "consteval int f() { return 1; }",
        ),
        (
            "a template member call",
            "void f() { obj.template get<int>(); }",
        ),
    ];

    #[test]
    fn the_dialect_the_grammar_cannot_read_parses_after_a_rewrite() {
        for (what, src) in DIALECT {
            same_length(src);
            assert!(
                normalize(src).is_some(),
                "{what}: nothing was rewritten, so the grammar still cannot read it"
            );
            assert!(
                !normalized(src).low_confidence(),
                "{what}: still fails to parse"
            );
        }
    }

    #[test]
    fn ordinary_code_is_left_exactly_as_it_was() {
        for (what, src) in ORDINARY {
            assert_eq!(
                normalize(src),
                None,
                "{what}: a rewrite fired on code the grammar already reads"
            );
        }
    }

    #[test]
    fn ordinary_code_survives_a_file_that_needs_a_rewrite() {
        // A header carries both. The rewrite fires for the dialect, and
        // every byte of the ordinary code below it has to come through
        // unchanged. A test on the ordinary code alone cannot see this,
        // because there the rewrite never starts.
        for (dialect, head) in DIALECT {
            for (plain, tail) in ORDINARY {
                let src = format!("{head}\n{tail}\n");
                let Some(out) = normalize(&src) else {
                    panic!("{dialect}: the rewrite stopped firing");
                };
                assert_eq!(
                    &out[src.len() - tail.len() - 1..],
                    &src[src.len() - tail.len() - 1..],
                    "{dialect} + {plain}: the rewrite reached the ordinary code"
                );
            }
        }
    }

    #[test]
    fn a_deleted_copy_with_a_reason_still_parses() {
        // The whole point: without this the class closes early and every
        // member below it is re-parented.
        let src = "struct Arena {\n\
                   \x20 Arena(const Arena&) = delete(\"interior pointers would dangle\");\n\
                   \x20 int take(int n) { return n + 1; }\n\
                   };\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "delete(\"reason\") must parse");
        assert!(
            f.units.iter().any(|u| &*u.name == "take"),
            "the member after the deleted copy survives"
        );
    }

    #[test]
    fn a_reason_containing_parens_and_quotes_does_not_end_the_span_early() {
        let src = "struct S { S(S&&) = delete(\"use move(x) \\\"not\\\" copy\"); int f() { return 1; } };\n";
        same_length(src);
        assert!(!normalized(src).low_confidence());
    }

    #[test]
    fn reflection_and_splices_leave_a_readable_body() {
        let src = "template <typename T>\n\
                   consteval unsigned long hash(const T& obj) {\n\
                   \x20 unsigned long h = 0;\n\
                   \x20 h ^= mix(obj.[:member_of<T, 0>():]);\n\
                   \x20 return h + sizeof(^^T);\n\
                   }\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "reflect and splice must parse");
        assert!(f.units.iter().any(|u| &*u.name == "hash"));
    }

    #[test]
    fn an_expansion_statement_is_an_ordinary_loop() {
        let src = "void walk() {\n\
                   \x20 template for (constexpr auto m : members) {\n\
                   \x20   use(m);\n\
                   \x20 }\n\
                   }\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "template for must parse");
        assert_eq!(
            f.units.iter().filter(|u| &*u.name == "walk").count(),
            1,
            "the loop stays inside its function"
        );
    }

    #[test]
    fn contract_clauses_vanish_and_the_body_remains() {
        let src = "int take(int n) noexcept pre(n > 0) post(r: r > n) {\n\
                   \x20 return n + 1;\n\
                   }\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "contract clauses must parse");
        let take = f.units.iter().find(|u| &*u.name == "take").expect("take");
        assert_eq!(take.params.len(), 1, "a clause is not a parameter");
    }

    #[test]
    fn a_call_to_a_function_named_pre_is_left_alone() {
        // `pre` is only a clause where a clause can stand. Blanking a
        // call would delete real work and under-report complexity.
        let src = "int f(int x) {\n  int y = pre(x);\n  return post(y);\n}\n";
        assert_eq!(normalize(src), None, "no clause here, so no rewrite");
    }

    #[test]
    fn constructs_inside_comments_and_strings_are_not_code() {
        let src = "// x = delete(\"not real\") and ^^T and [:s:]\n\
                   const char* s = \"= delete(\\\"still not\\\") ^^T\";\n\
                   int f() { return 0; }\n";
        assert_eq!(normalize(src), None, "prose is not syntax");
    }

    #[test]
    fn a_raw_string_holding_the_syntax_is_left_alone() {
        let src = "const char* t = R\"sql(= delete(\"x\") ^^T [:y:])sql\";\n";
        assert_eq!(normalize(src), None, "a raw string is one literal");
    }

    #[test]
    fn a_file_without_any_of_it_is_not_copied() {
        assert_eq!(normalize("int main() { return 0; }\n"), None);
    }
    /// What a project declares, and what each declaration has to do to
    /// the one line under it.
    const DECLARED: &str = "AttributeMacros: [KEEP_ALIVE]\n\
                            StatementMacros: [LAYOUT_INVARIANT]\n\
                            ForEachMacros: [for_each_slot]\n\
                            IfMacros: [IF_SOME]\n\
                            TypenameMacros: [STACK_OF]\n\
                            NamespaceMacros: [TESTSUITE]\n";

    #[test]
    fn a_declared_macro_stops_being_a_parse_error() {
        let cases: &[(&str, &str)] = &[
            (
                "an attribute in a declarator",
                "int* keep(int& a KEEP_ALIVE) KEEP_ALIVE { return &a; }\n",
            ),
            (
                "a whole declaration at namespace scope",
                "LAYOUT_INVARIANT(Alias, int);\nint f() { return 1; }\n",
            ),
            (
                "a loop over a range",
                "void f(Table t) { for_each_slot(s, t) { use(s); } }\n",
            ),
            (
                "a branch",
                "void f(Maybe m) { IF_SOME(x, m) { use(x); } }\n",
            ),
            ("a type", "STACK_OF(Frame)* frames() { return 0; }\n"),
            ("a namespace", "TESTSUITE(Pool) { int f() { return 1; } }\n"),
        ];
        for (what, src) in cases {
            let out = with_macros(src, DECLARED);
            let text = out.clone().unwrap_or_else(|| src.to_string());
            assert_eq!(text.len(), src.len(), "{what}: a rewrite moved an offset");
            assert!(out.is_some(), "{what}: the declaration was not read");
            let pack = Lang::Cpp.pack();
            let f = extract(pack, &mut pack.make_parser(), Path::new("t.cpp"), &text);
            assert!(!f.low_confidence(), "{what}: still fails to parse");
        }
    }

    #[test]
    fn a_loop_macro_stays_a_loop_and_a_branch_macro_stays_a_branch() {
        // Blanking either one would leave a bare block, which parses
        // and reports one control structure less than the code has.
        let loops = with_macros("void f(T t) { for_each_slot(s, t) { g(s); } }\n", DECLARED);
        assert!(loops.is_some_and(|t| t.contains("for (;;)")), "not a loop");
        let branch = with_macros("void f(T m) { IF_SOME(x, m) { g(x); } }\n", DECLARED).unwrap();
        let head = branch.split_once('{').unwrap().1.trim_start();
        assert!(head.starts_with("if "), "not a branch: {branch}");
        assert!(branch.contains("(x, m)"), "the condition went: {branch}");
    }

    #[test]
    fn a_type_macro_keeps_the_type_it_wraps() {
        // The argument IS the type. Blanking it with the name would
        // leave a declaration with nothing to declare.
        let out = with_macros("STACK_OF(Frame)* frames() { return 0; }\n", DECLARED).unwrap();
        assert!(out.contains("Frame"), "the type went with the macro: {out}");
        assert!(out.trim_start().starts_with("Frame"), "{out}");
    }

    #[test]
    fn a_macro_the_project_did_not_declare_is_left_alone() {
        // The name is the whole of what identifies one. A tool that
        // guessed would blank real work and report less than is there.
        assert_eq!(
            with_macros("int f(int a UNDECLARED) { return a; }\n", DECLARED),
            None
        );
    }

    #[test]
    fn a_declaration_does_not_reach_a_word_that_merely_contains_it() {
        // `KEEP_ALIVE` is declared; `KEEP_ALIVE_2` is another name.
        let src = "int KEEP_ALIVE_2 = 1;\nint f() { return KEEP_ALIVE_2; }\n";
        assert_eq!(with_macros(src, DECLARED), None);
    }
    #[test]
    fn typename_before_a_qualified_name_goes_and_a_template_parameter_keeps_it() {
        let dependent = "void f() { g(typename sriov::VfIndex::Trusted{}); }\n";
        same_length(dependent);
        assert!(normalize(dependent).is_some(), "typename must go here");
        assert!(!normalized(dependent).low_confidence());
        // A template parameter list spells a plain name after the word,
        // and blanking it there would turn the parameter into a value.
        for kept in [
            "template <typename T> void f(T x) { }\n",
            "template <typename... Ts> void f(Ts... a) { }\n",
            "template <template <typename> class C> void f() { }\n",
        ] {
            assert_eq!(normalize(kept), None, "a template parameter kept its word");
        }
    }

    #[test]
    fn a_name_after_a_string_literal_is_a_macro_that_expands_to_one() {
        let src = "void f(long v) { printf(\"%016\" PRIx64 \"\\n\", v); }\n";
        same_length(src);
        let out = normalize(src).expect("the macro must go");
        assert!(!out.contains("PRIx64"), "{out}");
        assert!(!normalized(src).low_confidence());
    }

    #[test]
    fn an_include_does_not_reach_the_declaration_under_it() {
        // The rule above reads back to a quote. Without a line to stop
        // it, `#include "config.h"` blanks the `namespace` below it and
        // every file that holds one stops parsing.
        let src = "#include \"config.h\"\nnamespace crucible {\nint f() { return 1; }\n}\n";
        assert_eq!(normalize(src), None, "the include reached past its line");
        assert!(!normalized(src).low_confidence());
    }

    #[test]
    fn a_user_defined_literal_keeps_its_suffix() {
        // The suffix is written against the quote, and it names the
        // operator. Only a space makes the name a separate token.
        let src = "constexpr int operator\"\"_km(unsigned long long v) { return (int)v; }\n";
        assert_eq!(normalize(src), None, "the suffix went with the literal");
    }
    #[test]
    fn a_declaration_that_states_a_string_of_its_own_keeps_its_name() {
        // A name after a string is a macro, EXCEPT in the two
        // declarations that put a string of their own in front of one.
        // Blanking either leaves a declaration with no declarator.
        for kept in [
            "extern \"C\" int fuzz_one(const char* data, int size);\n",
            "extern \"C++\" void g();\n",
            "extern \"C\" {\nint h(int a);\n}\n",
            "constexpr int operator \"\" _km(unsigned long long v) { return (int)v; }\n",
        ] {
            assert_eq!(normalize(kept), None, "a rewrite ate a declarator: {kept}");
            assert!(!normalized(kept).low_confidence(), "{kept}");
        }
    }

    #[test]
    fn a_macro_is_not_rewritten_in_the_directive_that_defines_it() {
        // `#define KEEP_ALIVE [[gnu::always_inline]]` names the macro on
        // a line that is not a declarator. Blanking it there leaves
        // `#define` with nothing to define, and the file stops parsing.
        let src = "#define KEEP_ALIVE [[gnu::always_inline]]\n\
                   #ifdef KEEP_ALIVE\n\
                   int f(int a KEEP_ALIVE) { return a; }\n\
                   #endif\n";
        let out = with_macros(src, DECLARED).expect("the declarator use is rewritten");
        assert!(
            out.contains("#define KEEP_ALIVE"),
            "the definition went: {out}"
        );
        assert!(out.contains("#ifdef KEEP_ALIVE"), "the guard went: {out}");
        assert!(!out.contains("int a KEEP_ALIVE"), "the use stayed: {out}");
    }
    #[test]
    fn a_braced_default_argument_parses_and_keeps_the_calls_in_it() {
        for src in [
            "void f(int x = {}) { }\n",
            "void f(Cfg c = {}) { }\n",
            "void f(Cfg = {}) { }\n",
            "void f(std::type_identity<Row> = {}) { }\n",
            "struct S { S(Cfg c = {}) noexcept : c_{c} {} Cfg c_; };\n",
            "void f(Cfg c = {1, 2}) { }\n",
        ] {
            same_length(src);
            assert!(normalize(src).is_some(), "not rewritten: {src}");
            assert!(!normalized(src).low_confidence(), "still fails: {src}");
        }
        // A value becomes parentheses and not blanks, so a call written
        // in a default is a call this tool still counts.
        let out = normalize("void f(Cfg c = {compute(), 2}) { }\n").unwrap();
        assert!(
            out.contains("compute()"),
            "the call went with the braces: {out}"
        );
        // A braced initializer that is not a default argument already
        // parses, and nothing here may touch it.
        for kept in ["int x = {};\n", "struct S { int x = {}; };\n"] {
            assert_eq!(normalize(kept), None, "rewrote an initializer: {kept}");
        }
    }
    #[test]
    fn a_designated_initializer_is_not_a_default_argument() {
        // `.pad = {},` sits in a braced list, where the comma opens the
        // next designator. Blanking the value leaves `.pad ,` and the
        // list stops parsing, which is worse than what it replaced.
        let src = "void f() {\n\
                   \x20 Meta m = {.layout = Strided,\n\
                   \x20            .pad = {},\n\
                   \x20            .slot = SlotId{SL_X},\n\
                   \x20            .pad2 = {}};\n\
                   \x20 use(m);\n\
                   }\n";
        assert_eq!(normalize(src), None, "a designator was read as a default");
        assert!(!normalized(src).low_confidence());
    }
}
