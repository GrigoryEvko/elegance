//! C++: the language the other packs were rehearsing for.
//!
//! Everything C's pack says still holds — macros stay unexpanded,
//! `#if` is a real branch, `goto` is the flat +1 — and C++ adds three
//! things worth stating rather than discovering.
//!
//! - Templates are measured AS WRITTEN. One template is one unit
//!   however many instantiations the linker emits, which is the same
//!   call the C pack makes about macros: measure what you can read.
//! - A class is not an interface, but a class whose methods are ALL
//!   pure virtual is exactly one, and that is what `interface width`
//!   counts here.
//!
//! Names come from the declarator chain as in C, with one addition:
//! `int Store::get(...)` carries its scope in a `qualified_identifier`,
//! so a method defined out of line still reports as `Store::get`
//! rather than as a free function called `get`.
//!
//! Tests arrive as macros. `TEST(Pool, TakesASlot) { ... }` is the only
//! declaration form the grammar can read — Catch2's
//! `TEST_CASE("a pool takes a slot")` puts a STRING where a parameter
//! belongs and parses as an error, so Catch2 files declare no tests
//! here at all. That is a stated limit, not a silent one.
//!
//! CUDA rides this pack the way TSX rides TypeScript: a second grammar,
//! the same tables, one extra entry. That is not a shortcut, it is the
//! measurement — `__global__` and `__device__` are unnamed tokens the
//! tree never shows, `__shared__` arrives as an ordinary
//! `type_qualifier`, and `add<<<grid, block>>>(x)` is already a
//! `call_expression` with one extra child. CUDA adds exactly two named
//! kinds to C++ and a kernel is an ordinary unit.

use tree_sitter::Node;

use super::{CatchSin, Lang, Pack, ParamInfo, field_text_is, sem_table};
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
    // `for (auto& x : xs)` — the range form is its own kind.
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
    // The C-style `(T)x`. The named casts have no node of their own —
    // `static_cast<T>(x)` parses as a call to a template function —
    // so `refine` reclassifies those.
    ("cast_expression", Sem::Cast),
    ("comment", Sem::Comment),
    ("preproc_include", Sem::Import),
    ("using_declaration", Sem::Import),
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

/// `void *` remains the hatch; `auto` is inference, not evasion, and
/// the compiler knows the type exactly.
const LOOSE: &[&str] = &["void"];

/// `add<<<grid, block>>>(x)` parses as a `call_expression` carrying an
/// extra `kernel_call_syntax` child that holds the launch geometry. The
/// call is ALREADY counted by the shared table, so mapping this to
/// `Sem::Call` would price one launch as two calls. It is named here
/// rather than left out so that a grammar bump which renames or drops
/// it fails the resolution test instead of silently changing nothing.
///
/// `launch_bounds` is `__launch_bounds__(256, 4)` — a compiler hint on
/// a declaration, which is configuration and not control flow.
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
/// everything but keyword — the C++ spelling of the contract Go and
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

/// `virtual int a() = 0;` — the grammar records the `= 0` as a default
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

/// The declarator chain, as in C, ending at an identifier — or at a
/// `qualified_identifier`, which is how an out-of-line definition
/// carries its class: `int Store::get(...)` must report as a method of
/// Store rather than as a free function named get.
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
/// TEST. The shape is unmistakable: a definition with NO return type
/// whose "parameters" are bare type names carrying no declarator,
/// because they are not parameters at all. Returns (macro, last
/// argument), which is the name a human reads. Without this every
/// gtest unit reports under the macro's own four letters, and every
/// test-quality metric judges `TEST` instead of the case.
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
/// against 16% for a corpus of notorious code — the metric was
/// measuring the pack, not the tests. Joined, fmt's names say what they
/// test and a genuinely lazy `TEST(foo, bar)` still reads as two words.
fn composed_name(node: Node, src: &[u8]) -> Option<String> {
    // The gtest family only. Some other two-identifier macro still gets
    // its last argument as a name — that is `name_node`'s answer — but
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

/// `#include <x>` keeps its brackets (definitionally external);
/// `using namespace ns` and `using ns::name` are import edges too.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = |n: Node| n.utf8_text(src).unwrap_or("");
    let target = match node.kind() {
        "preproc_include" => node.child_by_field_name("path").map(text),
        _ => node.named_child(0).map(text),
    };
    target
        .map(|t| {
            vec![super::ImportInfo {
                target: t.trim_matches('"').into(),
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
    // first named child is what makes `const std::string& k` a
    // parameter named k rather than a parameter named nothing.
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
        // call site, which is what the grammar's own kind says.
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

/// `catch (...)` catches everything and binds nothing — C++'s spelling
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

/// C++ spells one claim four ways. `ASSERT_*` is `assertish` already;
/// `EXPECT_*` is gtest's non-fatal twin and asserts exactly as much;
/// Catch2's `REQUIRE`/`CHECK` and glog's `CHECK` are the same sentence
/// again. Requiring the underscore keeps ordinary names — `checked()`,
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

/// The gtest family, which is the family that parses: every one of
/// these takes two identifiers and a block. Catch2's string-argument
/// macros are absent because the grammar rejects them, and
/// google/benchmark's `BENCHMARK(BM_Foo);` is a statement naming a
/// function defined elsewhere, not a declaration of anything.
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
/// kinds table already prices as a Cast. What remains genuinely
/// unpredictable is longjmp, as in C.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && callee_name(node, src).is_some_and(|t| matches!(t, "longjmp" | "setjmp" | "siglongjmp"))
}

#[cfg(test)]
mod tests {
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
        // which is the whole reason the dialect exists.
        assert!(
            facts("void host(float* a, int n) {\n  add<<<grid, block>>>(a, n);\n}\n")
                .low_confidence(),
            "if C++ could read a launch there would be nothing to add"
        );
    }

    #[test]
    fn an_out_of_line_method_keeps_its_class() {
        // `int Store::get(...)` is a method of Store, not a free
        // function called get — the qualified declarator says so.
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
        // A constructor has no return type either; its parameters are
        // what tell the two apart.
        let ctor = facts("struct S {\n  S(int a, int b) { x = a + b; }\n};\n");
        assert_eq!(&*ctor.units[1].name, "S");
        assert!(!ctor.units[1].named_test);
    }

    #[test]
    fn catch_all_is_broad() {
        let f = facts("void f() {\n  try { risky(); } catch (...) { log(); }\n}\n");
        assert_eq!(f.units[1].broad_catch, 1, "catch (...) binds nothing");
    }
}
