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
//! Tests arrive as macros. The grammar reads `TEST(Pool, TakesASlot) {
//! ... }` as a `macro_invocation` with a body, which is a function
//! definition after expansion, and the unit is named `Pool.TakesASlot`.
//! Catch2's `TEST_CASE("a pool takes a slot") { ... }` has the same
//! shape, and its unit keeps the name of the macro: nothing here reads
//! the string as the name of a test.
//!
//! CUDA rides this pack the way TSX rides TypeScript: a second grammar,
//! the same tables, one extra entry. `__global__` and `__device__` are
//! unnamed tokens the tree never shows, `__shared__` arrives as an
//! ordinary `type_qualifier`, and `add<<<grid, block>>>(x)` is already a
//! `call_expression` with one extra child. CUDA adds exactly two named
//! kinds to C++ and a kernel is an ordinary unit.

use tree_sitter::Node;

use super::{CatchSin, Lang, Pack, ParamInfo, TextCtrl, field_text_is, sem_table};
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
    // `preproc_call` it shares with `#pragma` and `#error`. The fork also
    // reads each line of a conditional group that it cannot keep as one
    // node as a `preproc_call`, and `refine` reads those by their name.
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

/// The kinds that only the C++ grammar of the GrigoryEvko fork spells.
/// The CUDA dialect reads with the stock CUDA grammar, which has none of
/// them, so they sit in a table of their own.
///
/// `macro_invocation` is a macro that the grammar did not read as a call
/// expression: a line with no `;`, a name with a block after it, an item
/// at file scope. Most of them stand where a statement stands, so the
/// table says `Call`, and `refine` reads the position. See `macro_sem`.
///
/// Qt's `foreach (x, xs)` and `Q_FOREACH` expand to a `for` statement,
/// and `forever` to `for (;;)`. Each is a loop.
const FORK_ONLY: &[(&str, Sem)] = &[
    ("macro_invocation", Sem::Call),
    ("qt_foreach_statement", Sem::Loop),
    ("qt_forever_statement", Sem::Loop),
];

pub fn pack(dialect: Dialect) -> Pack {
    let (lang, ts): (Lang, tree_sitter::Language) = match dialect {
        Dialect::Cpp => (Lang::Cpp, tree_sitter_cpp::LANGUAGE.into()),
        Dialect::Cuda => (Lang::Cuda, tree_sitter_cuda::LANGUAGE.into()),
    };
    let kinds: &[&[(&str, Sem)]] = match lang {
        Lang::Cuda => &[KINDS, CUDA_ONLY],
        _ => &[KINDS, FORK_ONLY],
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
        // A call expression fields its target as `function`, and a
        // macro invocation fields its name as `name`.
        call_target_fields: &["function", "name"],
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
        unparsed_ctrl,
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

/// `&&`/`||` from the shared binary kind; `else if` flattens as in C, and
/// so does an `else` before a macro that reads as an `if`.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    if sem == Sem::Import
        && node.kind() == "preproc_call"
        && let Some(event) = field_text_is(node, "directive", src)
            .and_then(directive_name)
            .and_then(conditional_event)
    {
        return event;
    }
    let sem = match sem == Sem::Call && node.kind() == "macro_invocation" {
        true => macro_sem(node),
        false => sem,
    };
    match sem {
        Sem::None if super::c::spliced_include(node, src) => Sem::Import,
        Sem::If if node.parent().is_some_and(|p| p.kind() == "else_clause") => Sem::ElseIf,
        Sem::Else if node.named_child(0).is_some_and(is_an_if) => Sem::None,
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

/// An `if` statement, or a macro invocation with an `else`, which is one
/// after expansion. See `macro_sem`.
fn is_an_if(node: Node) -> bool {
    match node.kind() {
        "if_statement" => true,
        "macro_invocation" => node.child_by_field_name("alternative").is_some(),
        _ => false,
    }
}

fn is_a_named_cast(call: Node, src: &[u8]) -> bool {
    matches!(
        callee_name(call, src),
        Some("static_cast" | "dynamic_cast" | "reinterpret_cast" | "const_cast")
    )
}

/// Where a macro invocation stands, which is what decides what it
/// expands to when no macro table is at hand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MacroScope {
    /// A translation unit, a namespace body or a class body. Only
    /// declarations stand here, and nothing runs.
    Declaration,
    /// A block, a case body, a label or a substatement.
    Statement,
    /// An item of an enumerator list, an initializer list or a template
    /// parameter list: a part of that list after expansion.
    List,
}

/// What a `macro_invocation` is after expansion.
///
/// - A macro with a body at declaration scope is a function definition:
///   gtest's `TEST(Suite, Case) { ... }` expands to the definition of
///   `Suite_Case_Test::TestBody`, and GCC and Clang accept no statement
///   there.
/// - A macro with a body and an `else` after it is an `if` statement:
///   only an `if` takes an `else` (GCC `cp_parser_selection_statement`),
///   and Boost defines `BOOST_IF_CONSTEXPR` as `if constexpr` or as `if`.
/// - A macro with arguments at statement scope is a call, as the C pack
///   reads every function-like macro. The grammar itself reads
///   `ASSERT(x);` as a call expression, and the same line with no `;`,
///   `ASSERT(x)`, is this node. It counts wrongly where the macro
///   expands to no call: `Q_UNUSED(x)` is a cast to `void`. With a body,
///   as in `FOREACH(item, items) { ... }`, the macro opens a loop, a
///   branch or a scope that nothing here can name, and the body counts
///   as plain statements of the unit.
/// - A name with no arguments is an object-like macro, and nothing here
///   can tell a call inside its expansion: bde's `T_` prints a tab.
/// - At declaration scope a macro with no body expands to declarations,
///   and nothing runs there. In a list it expands to items of the list.
fn macro_sem(node: Node) -> Sem {
    let has = |field: &str| node.child_by_field_name(field).is_some();
    match macro_scope(node) {
        MacroScope::Declaration if has("body") => Sem::FnDef,
        MacroScope::Statement if has("alternative") => Sem::If,
        MacroScope::Statement if has("arguments") => Sem::Call,
        _ => Sem::None,
    }
}

/// The scope of a macro invocation, read from its nearest ancestor that
/// is neither a conditional group nor an ERROR node. A group holds the
/// items of the scope around it, and an ERROR node holds what the parser
/// could not place. Cost: O(depth) calls to `Node::parent`, and each one
/// walks down from the root.
fn macro_scope(node: Node) -> MacroScope {
    let mut up = node.parent();
    while let Some(p) = up {
        match p.kind() {
            _ if p.is_error() => up = p.parent(),
            "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_elifdef"
            | "preproc_else" => up = p.parent(),
            "translation_unit"
            | "declaration_list"
            | "field_declaration_list"
            | "export_declaration" => return MacroScope::Declaration,
            "enumerator_list" | "initializer_list" | "template_parameter_list" => {
                return MacroScope::List;
            }
            _ => return MacroScope::Statement,
        }
    }
    MacroScope::Declaration
}

/// The name of the directive that a line starts, without its `#` or
/// `%:`: `if` for `#  if A`. None for a line that starts no directive.
fn directive_name(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches([' ', '\t']);
    let rest = rest
        .strip_prefix('#')
        .or_else(|| rest.strip_prefix("%:"))?
        .trim_start_matches([' ', '\t']);
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The control event of a conditional directive, as a structured group
/// counts it: `#if`, `#ifdef` and `#ifndef` open a branch, the `#elif`
/// family and `#else` continue the chain, and `#endif` closes the group
/// with no event, which is `Sem::None`. None for every other directive.
///
/// The fork keeps a group as `preproc_if` or `preproc_ifdef` only where
/// each branch holds whole declarations or statements. A group inside an
/// expression or a function head is lines: each directive line of the
/// branch that the parser reads is a `preproc_call`, and the text of the
/// other branches is one `preproc_skipped` token.
fn conditional_event(name: &str) -> Option<Sem> {
    match name {
        "if" | "ifdef" | "ifndef" => Some(Sem::If),
        "elif" | "elifdef" | "elifndef" => Some(Sem::ElseIf),
        "else" => Some(Sem::Else),
        "endif" => Some(Sem::None),
        _ => None,
    }
}

/// The conditional directive lines in the text of a branch that the
/// parser skipped, as a structured group counts them.
///
/// The text runs from the first skipped line to the line before the
/// `#endif` of the group. It holds the `#elif` and `#else` lines of the
/// group, and the whole of each group nested in it, so a nested `#if`
/// takes the nesting of the groups that the text opens before it. The
/// text has no tree: a call, a statement or a C++ `if` in it counts
/// nothing, and that is a loss against a structured group.
///
/// A line that starts in a block comment is not a directive. A raw
/// string literal that holds a line starting with `#if` is read as a
/// directive. Cost: O(length of the text).
fn unparsed_ctrl(node: Node, src: &[u8]) -> Vec<TextCtrl> {
    if node.kind() != "preproc_skipped" {
        return Vec::new();
    }
    let Ok(text) = node.utf8_text(src) else {
        return Vec::new();
    };
    let first = node.start_position().row as u32 + 1;
    let mut events = Vec::new();
    let mut depth = 0u8;
    let mut in_comment = false;
    for (offset, line) in text.lines().enumerate() {
        let starts_in_comment = in_comment;
        in_comment = comment_open_after(line, in_comment);
        let event = match starts_in_comment {
            true => None,
            false => directive_name(line).and_then(conditional_event),
        };
        let nesting = depth;
        match event {
            None => continue,
            Some(Sem::None) => depth = depth.saturating_sub(1),
            Some(sem) => {
                if sem == Sem::If {
                    depth = depth.saturating_add(1);
                }
                events.push(TextCtrl {
                    sem,
                    line: first + offset as u32,
                    nesting,
                });
            }
        }
    }
    events
}

/// Is a block comment open at the end of `line`, given that one was open
/// at its start or not? A `//` ends the line, and a quoted literal hides
/// the comment markers in it. Cost: O(length of the line).
fn comment_open_after(line: &str, mut open: bool) -> bool {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let pair = (bytes[i], bytes.get(i + 1).copied());
        match (open, quote, pair) {
            (true, _, (b'*', Some(b'/'))) => {
                open = false;
                i += 1;
            }
            (false, Some(_), (b'\\', _)) => i += 1,
            (false, Some(q), (b, _)) if b == q => quote = None,
            (false, None, (b'/', Some(b'/'))) => return false,
            (false, None, (b'/', Some(b'*'))) => {
                open = true;
                i += 1;
            }
            (false, None, (b @ (b'"' | b'\''), _)) => quote = Some(b),
            _ => {}
        }
        i += 1;
    }
    open
}

/// The declarator chain, as in C, ending at an identifier or at a
/// `qualified_identifier`, which is how an out-of-line definition
/// carries its class. `int Store::get(...)` is named through that
/// node's `name` child, so the unit is `get` and the class stays in
/// the declarator.
fn name_node(node: Node) -> Option<Node> {
    if let Some(case) = macro_case_name(node) {
        return Some(case);
    }
    if node.kind() != "function_definition" {
        return None;
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

/// A macro followed by a block whose arguments are two or more bare
/// names: `TEST(Pool, TakesASlot) { ... }`. Returns (macro, first
/// argument, last argument). The last argument is the name a human
/// reads. Without this every gtest unit reports under the macro's own
/// four letters, and every test-quality metric judges `TEST` instead of
/// the case.
///
/// The grammar spells the block in two shapes, and both are read:
///
/// - A `macro_invocation` with a `body`. The fork reads an uppercase
///   name, its argument tokens and a block that way.
/// - A `function_definition` with no return type whose "parameters" are
///   bare type names with no declarator, which are not parameters at
///   all. The fork keeps this shape for a lowercase macro, and for a
///   block that is a function try block. A constructor also has no
///   return type, but its parameters are named. The only collision is a
///   constructor whose every parameter is unnamed, which could not use
///   them.
fn test_macro(node: Node) -> Option<(Node, Node, Node)> {
    let (macro_name, names) = match node.kind() {
        "macro_invocation" => invoked_names(node)?,
        "function_definition" => declared_names(node)?,
        _ => return None,
    };
    match names[..] {
        [first, .., last] => Some((macro_name, first, last)),
        _ => None,
    }
}

/// The macro and the argument names of a `macro_invocation` with a body,
/// when every argument is one bare name. `TEST(a::B, C)` gives three
/// identifiers and a `::` token, and names no test.
fn invoked_names(node: Node) -> Option<(Node, Vec<Node>)> {
    node.child_by_field_name("body")?;
    let macro_name = node.child_by_field_name("name")?;
    let args = node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let tokens: Vec<Node> = args.children(&mut cursor).collect();
    let bare = tokens.iter().all(|t| match t.is_named() {
        true => t.kind() == "identifier",
        false => matches!(t.kind(), "(" | ")" | ","),
    });
    let names: Vec<Node> = tokens.into_iter().filter(|t| t.is_named()).collect();
    bare.then_some((macro_name, names))
}

/// The macro and the "parameter" names of a `function_definition` that a
/// macro spells, when every parameter is a bare type name.
fn declared_names(node: Node) -> Option<(Node, Vec<Node>)> {
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
    (names.len() == args.len()).then_some((macro_name, names))
}

fn macro_case_name(node: Node) -> Option<Node> {
    test_macro(node).map(|(_, _, case)| case)
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
    let (_, suite, case) = test_macro(node)?;
    Some(format!(
        "{}.{}",
        suite.utf8_text(src).ok()?,
        case.utf8_text(src).ok()?
    ))
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

/// The called name, through the member and template spellings. A macro
/// invocation names its macro in `name`.
fn callee_name<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let f = call
        .child_by_field_name("function")
        .or_else(|| call.child_by_field_name("name"))?;
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

/// The gtest family: every one of these takes two identifiers and a
/// block. Catch2's string-argument macros are absent because no name
/// here is read from a string, and google/benchmark's
/// `BENCHMARK(BM_Foo);` is a statement naming a function defined
/// elsewhere, not a declaration of anything.
fn declares_test(node: Node, src: &[u8]) -> bool {
    let Some(name) = test_macro(node).and_then(|(m, _, _)| m.utf8_text(src).ok()) else {
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

mod dialect;

pub use dialect::{clang_gaps, lower_for_clang, normalize, rewrites};

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

    /// Each node of `kind` in the C++ tree of `src`, as (1-based line,
    /// the pack's verdict). The kind pins the shape of the fork's tree,
    /// so a grammar that stops spelling it fails here and not in a count.
    fn sems_of(src: &str, kind: &str) -> Vec<(usize, Sem)> {
        let pack = Lang::Cpp.pack();
        let tree = pack
            .make_parser()
            .parse(src, None)
            .expect("the parse finishes");
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == kind {
                out.push((
                    node.start_position().row + 1,
                    pack.sem_of(node, src.as_bytes()),
                ));
            }
            // Reversed, so that the stack gives the nodes in source order.
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        out
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
    fn a_macro_with_a_body_at_declaration_scope_is_a_definition() {
        // The text of firefox xpcom/tests/gtest: TestThreadUtils.cpp
        // writes the brace on the next line, and
        // TestAvailableMemoryWatcherWin.cpp writes TEST_F. The fork reads
        // each one as a `macro_invocation` with a body. After expansion
        // it defines `Suite_Case_Test::TestBody`, which is one unit.
        let src = "namespace mozilla {\n\
                   TEST(ThreadUtils, NewRunnableFunction)\n\
                   {\n\
                   \x20 EXPECT_TRUE(ran);\n\
                   }\n\
                   }  // namespace mozilla\n\
                   #ifdef XP_WIN\n\
                   TEST_F(AvailableMemoryWatcherFixture, AlwaysActive) {\n\
                   \x20 StartUserInteraction();\n\
                   }\n\
                   #endif\n\
                   void Helper() {\n\
                   \x20 FOREACH(item, items) { use(item); }\n\
                   }\n";
        let sems = sems_of(src, "macro_invocation");
        assert_eq!(
            sems,
            [(2, Sem::FnDef), (8, Sem::FnDef), (13, Sem::Call)],
            "a block at declaration scope defines a function, and a block in a function does not"
        );
        let f = facts(src);
        assert!(!f.low_confidence());
        let units: Vec<(&str, &str, bool, u16)> = f.units[1..]
            .iter()
            .map(|u| (&*u.name, &*u.qualname, u.named_test, u.params.len() as u16))
            .collect();
        assert_eq!(
            units,
            [
                (
                    "ThreadUtils.NewRunnableFunction",
                    "ThreadUtils.NewRunnableFunction",
                    true,
                    0
                ),
                (
                    "AvailableMemoryWatcherFixture.AlwaysActive",
                    "AvailableMemoryWatcherFixture.AlwaysActive",
                    true,
                    0
                ),
                ("Helper", "Helper", false, 0),
            ],
            "the suite and the case name the test, and a test body takes no parameters"
        );
        assert_eq!(f.units[1].assert_calls, 1, "the body belongs to the test");
        // A name that is not one bare identifier names no test, and the
        // unit keeps the name of its macro.
        let qualified = facts("TEST(mozilla::Suite, Case) {\n  f();\n}\n");
        assert_eq!(&*qualified.units[1].name, "TEST");
        assert!(!qualified.units[1].named_test);
    }

    #[test]
    fn a_macro_with_arguments_in_a_block_is_a_call() {
        // The text of bde ball_userfieldvalue.t.cpp and its header. The
        // grammar reads `ASSERT(x);` as a call expression, and the same
        // line with no `;` as a macro invocation, which is the same call.
        let src = "BSLMF_ASSERT(sizeof SUFFICIENTLY_LONG_STRING > sizeof(bsl::string));\n\
                   class UserFieldValue {\n\
                   \x20 public:\n\
                   \x20   BSLMF_NESTED_TRAIT_DECLARATION(UserFieldValue,\n\
                   \x20                                  bslma::UsesBslmaAllocator);\n\
                   };\n\
                   int main(int argc, char *argv[])\n\
                   {\n\
                   \x20   ball::UserFieldValue valueB(5);\n\
                   \x20   ASSERT(ball::UserFieldType::e_INT64 == valueB.type());\n\
                   \x20   ASSERT(5                            == valueB.theInt64())\n\
                   //\n\
                   \x20   ASSERT(valueA != valueB);\n\
                   \x20   if (veryVerbose) { T_ T_ P_(CONFIG) P(X) }\n\
                   \x20   return testStatus;\n\
                   }\n";
        assert_eq!(
            sems_of(src, "macro_invocation"),
            [
                (1, Sem::None),
                (4, Sem::None),
                (11, Sem::Call),
                (14, Sem::None),
                (14, Sem::None),
                (14, Sem::Call),
                (14, Sem::Call),
            ],
            "a declaration scope runs nothing, and a name with no arguments is no call site"
        );
        let f = facts(src);
        assert!(!f.low_confidence());
        let main = f.units.iter().find(|u| &*u.name == "main").expect("main");
        assert_eq!(main.assert_calls, 3, "an assertion with no `;` asserts too");
        assert_eq!(f.units[0].assert_calls, 0, "a static assertion is no call");
    }

    #[test]
    fn a_macro_with_an_else_is_an_if() {
        // The chain of boost uuid/detail/to_chars_x86.hpp, with the
        // statements of each branch cut to one call. Only an `if` takes
        // an `else`, so the chain costs what the same chain of `if`
        // statements costs.
        let chain = |head: &str| {
            format!(
                "template< typename Char >\n\
                 inline void to_chars_simd(Char* out)\n\
                 {{\n\
                 \x20   {head} (sizeof(Char) == 1u)\n\
                 \x20   {{\n\
                 \x20       store(out);\n\
                 \x20   }}\n\
                 \x20   else {head} (sizeof(Char) == 2u)\n\
                 \x20   {{\n\
                 \x20       widen(out);\n\
                 \x20   }}\n\
                 \x20   else\n\
                 \x20   {{\n\
                 \x20       widen4(out);\n\
                 \x20   }}\n\
                 }}\n"
            )
        };
        let macro_chain = chain("BOOST_IF_CONSTEXPR");
        assert_eq!(
            sems_of(&macro_chain, "macro_invocation"),
            [(4, Sem::If), (8, Sem::ElseIf)]
        );
        assert_eq!(
            sems_of(&macro_chain, "else_clause"),
            [(8, Sem::None), (12, Sem::Else)],
            "an `else` before an `if` flattens into the chain"
        );
        let f = facts(&macro_chain);
        let g = facts(&chain("if"));
        assert!(!f.low_confidence() && !g.low_confidence());
        let events = |facts: &crate::facts::FileFacts| -> Vec<(Sem, u32, u8)> {
            let unit = &facts.units[1];
            unit.ctrl
                .iter()
                .map(|c| (c.sem, c.line, c.cog_depth))
                .collect()
        };
        assert_eq!(events(&f), events(&g));
        assert_eq!(
            crate::metrics::complexity(&f.units[1]),
            crate::metrics::complexity(&g.units[1])
        );
    }

    #[test]
    fn a_qt_loop_is_a_loop() {
        // The text of qt-creator src/libs/utils/commandline.cpp:131-141
        // and of the qtbase containers snippet, which spell the loops of
        // Qt. After expansion each one is the `for` statement below it.
        let qt = "QStringList ProcessArgs::splitArgsWin(const QString &args)\n\
                  {\n\
                  \x20   forever {\n\
                  \x20       forever {\n\
                  \x20           if (p == length)\n\
                  \x20               return ret;\n\
                  \x20           if (!isWhiteSpaceWin(args.unicode()[p].unicode()))\n\
                  \x20               break;\n\
                  \x20           ++p;\n\
                  \x20       }\n\
                  \x20       foreach (const QString &str, values)\n\
                  \x20           qDebug() << str;\n\
                  \x20   }\n\
                  }\n";
        let plain = qt.replacen("forever", "for (;;)", 2).replace(
            "foreach (const QString &str, values)",
            "for (const QString &str : values)",
        );
        let (f, g) = (facts(qt), facts(&plain));
        assert!(!f.low_confidence() && !g.low_confidence());
        assert_eq!(
            sems_of(qt, "qt_forever_statement"),
            [(3, Sem::Loop), (4, Sem::Loop)]
        );
        assert_eq!(sems_of(qt, "qt_foreach_statement"), [(11, Sem::Loop)]);
        assert_eq!(ctrl_of(&f), ctrl_of(&g));
        assert_eq!(f.units[1].max_loop_depth, 2);
        assert_eq!(
            crate::metrics::complexity(&f.units[1]),
            crate::metrics::complexity(&g.units[1])
        );
    }

    /// The (event, line) pairs of each unit after the module, in order.
    fn ctrl_of(f: &crate::facts::FileFacts) -> Vec<Vec<(Sem, u32)>> {
        f.units[1..]
            .iter()
            .map(|u| u.ctrl.iter().map(|c| (c.sem, c.line)).collect())
            .collect()
    }

    #[test]
    fn a_conditional_group_of_lines_counts_each_directive() {
        // The text of bde balb_pipecontrolchannel.cpp:355-383, and of the
        // tables of baljsn_encodeimplutil.t.cpp:177-188 and
        // bdlsb_memoutstreambuf.t.cpp:1290-1309. The fork keeps no group
        // node where a branch holds a part of a statement. Each directive
        // line that it reads is a `preproc_call`, and the other branches
        // are one `preproc_skipped` token of text.
        let src = "void PipeControlChannel::backgroundProcessor()\n\
                   {\n\
                   \x20           if (0 == bytesRead) {\n\
                   \x20               continue;\n\
                   \x20           }\n\
                   \x20           else if (0 > bytesRead) {\n\
                   #if EAGAIN != EWOULDBLOCK\n\
                   \x20               if (EAGAIN == savedErrno || EWOULDBLOCK == savedErrno) {\n\
                   #else\n\
                   \x20               if (EAGAIN == savedErrno) {\n\
                   #endif\n\
                   \x20                   continue;\n\
                   \x20               } else {\n\
                   \x20                   bail();\n\
                   \x20               }\n\
                   \x20           }\n\
                   }\n\
                   void testEncode()\n\
                   {\n\
                   \x20   static const struct {\n\
                   \x20       int         d_line;\n\
                   \x20       Int64       d_value;\n\
                   \x20       const char *d_result;\n\
                   \x20   } DATA[] = {\n\
                   \x20       { L_,   UINT_MAX,           \"4294967295\" },\n\
                   #if   defined(BSLS_PLATFORM_CPU_32_BIT)                                       \\\n\
                   \x20 || (defined(BSLS_PLATFORM_CPU_64_BIT) && defined(BSLS_PLATFORM_OS_WINDOWS))\n\
                   \x20       { L_,   LONG_MAX,           \"2147483647\" },\n\
                   #elif defined(BSLS_PLATFORM_CPU_64_BIT)\n\
                   \x20       { L_,   LONG_MAX,  \"9223372036854775807\" },\n\
                   #else\n\
                   # error \"baljsn_encoder.t.cpp does not support the platform's bitness.\"\n\
                   #endif\n\
                   \x20       { L_,  LLONG_MAX,  \"9223372036854775807\" },\n\
                   \x20   };\n\
                   }\n\
                   void testReserve()\n\
                   {\n\
                   \x20   static const struct { int d_line; int d_size; } DATA[] = {\n\
                   \x20              { L_,   INIT_BUFSIZE, IBPO,           TWICE_INIT_BUFSIZE },\n\
                   #if   defined(BSLS_PLATFORM_OS_WINDOWS)\n\
                   \x20 #if   defined(BSLS_PLATFORM_CPU_32_BIT)\n\
                   \x20              { L_,   0,            SZ_INT_MAX/8,   SZ_INT_MAX/8 +1    },\n\
                   \x20 #elif defined(BSLS_PLATFORM_CPU_64_BIT)\n\
                   \x20              { L_,   0,            SZ_INT_MAX/2,   SZ_INT_MAX/2 +1    },\n\
                   \x20 #else\n\
                   \x20       #error \"Unknown CPU\"\n\
                   \x20 #endif\n\
                   #elif defined(BSLS_PLATFORM_OS_UNIX)\n\
                   \x20 /* #if defined(BSLS_PLATFORM_CPU_16_BIT)\n\
                   #if a comment line starts no group\n\
                   \x20 */\n\
                   \x20 #if   defined(BSLS_PLATFORM_CPU_32_BIT)\n\
                   \x20              { L_,   0,            SZ_INT_MAX/4,   SZ_INT_MAX/4 +1    },\n\
                   \x20 #elif defined(BSLS_PLATFORM_CPU_64_BIT)\n\
                   \x20              { L_,   0,            SZ_INT_MAX,    (SZ_INT_MAX+1)*1    },\n\
                   \x20 #else\n\
                   \x20       #error \"Unknown CPU\"\n\
                   \x20 #endif\n\
                   #else\n\
                   \x20   #error \"Unknown OS\"\n\
                   #endif\n\
                   \x20           };\n\
                   }\n";
        let f = facts(src);
        assert!(!f.low_confidence(), "the fork reads each group as lines");
        assert_eq!(
            sems_of(src, "preproc_call")
                .into_iter()
                .map(|(line, sem)| (sem, line))
                .collect::<Vec<_>>(),
            [
                (Sem::If, 7),
                (Sem::None, 11),
                (Sem::If, 26),
                (Sem::None, 33),
                (Sem::If, 41),
                (Sem::If, 42),
                (Sem::None, 48),
                (Sem::None, 62),
            ],
            "a directive line counts by its name, and `#endif` closes with no event"
        );
        use Sem::{Else, ElseIf, If, Jump};
        assert_eq!(
            ctrl_of(&f),
            [
                vec![
                    (If, 3),
                    (Jump, 4),
                    (ElseIf, 6),
                    (If, 7),
                    (If, 8),
                    (Sem::BoolOp, 8),
                    (Else, 9),
                    (Jump, 12),
                    (Else, 13),
                ],
                vec![(If, 26), (ElseIf, 29), (Else, 31)],
                vec![
                    (If, 41),
                    (If, 42),
                    (ElseIf, 44),
                    (Else, 46),
                    (ElseIf, 49),
                    (If, 53),
                    (ElseIf, 55),
                    (Else, 57),
                    (Else, 60),
                ],
            ],
            "the skipped text counts its `#elif`, `#else` and nested `#if` lines, and a comment counts none"
        );
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
}
