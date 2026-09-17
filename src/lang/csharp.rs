//! C#: Java's declarations plus the escape hatches Java refused.
//!
//! `async` and `await` are syntax rather than library, so `blocking
//! async` and `dropped tasks` are live here and dead in Java: a
//! `.Result` on a Task is the deadlock this codebase names. A
//! preprocessor survives too, so `#if` is real control flow the reader
//! must follow, as in C.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("method_declaration", Sem::FnDef),
    ("constructor_declaration", Sem::FnDef),
    ("destructor_declaration", Sem::FnDef),
    ("operator_declaration", Sem::FnDef),
    ("local_function_statement", Sem::FnDef),
    ("accessor_declaration", Sem::FnDef),
    ("lambda_expression", Sem::Lambda),
    ("anonymous_method_expression", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("interface_declaration", Sem::TypeDef),
    ("struct_declaration", Sem::TypeDef),
    ("record_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("delegate_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("conditional_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("foreach_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    ("switch_expression", Sem::Match),
    ("switch_section", Sem::CaseArm),
    ("switch_expression_arm", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("finally_clause", Sem::With),
    ("using_statement", Sem::With),
    ("lock_statement", Sem::With),
    ("binary_expression", Sem::BoolOp),
    ("invocation_expression", Sem::Call),
    ("object_creation_expression", Sem::Call),
    ("implicit_object_creation_expression", Sem::Call),
    ("cast_expression", Sem::Cast),
    ("as_expression", Sem::Cast),
    ("await_expression", Sem::Await),
    ("comment", Sem::Comment),
    ("using_directive", Sem::Import),
    // The file itself is asked for its imports, because a C# file's
    // dependencies are not written as directives. See `type_references`.
    ("compilation_unit", Sem::Import),
    ("identifier", Sem::Ident),
    ("integer_literal", Sem::NumLit),
    ("real_literal", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("verbatim_string_literal", Sem::StrLit),
    ("raw_string_literal", Sem::StrLit),
    ("interpolated_string_expression", Sem::StrLit),
    ("character_literal", Sem::StrLit),
    ("boolean_literal", Sem::BoolLit),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("return_statement", Sem::Jump),
    ("throw_statement", Sem::Jump),
    ("yield_statement", Sem::Jump),
    ("goto_statement", Sem::Goto),
    // Conditional compilation is control flow the reader must follow,
    // for the same reason it counts in C.
    ("preproc_if", Sem::If),
    ("preproc_elif", Sem::ElseIf),
    ("preproc_else", Sem::Else),
];

const DEF_SITES: &[(&str, &str)] = &[
    // A local's declarator is its birth. Without it the live map holds
    // no definition row for any local, so the repurposing check has
    // nothing to compare a rewrite against.
    ("variable_declarator", "name"),
    ("method_declaration", "name"),
    ("constructor_declaration", "name"),
    ("local_function_statement", "name"),
    ("class_declaration", "name"),
    ("interface_declaration", "name"),
    ("struct_declaration", "name"),
    ("record_declaration", "name"),
    ("enum_declaration", "name"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("member_access_expression", "expression");

/// `object` and `dynamic` both abandon the checker; `dynamic` does it
/// loudly enough to deserve the same name.
const LOOSE: &[&str] = &["object", "dynamic", "var"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::CSharp,
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
        return_type_field: "returns",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: true,
        record_keys: |_, _| None,
        is_async,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        // `///` is the XML documentation comment.
        doc_markers: &["///"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override,
        spooky,
        unparsed_ctrl: |_, _| Vec::new(),
        negation_operand,
        catch_sin,
        swallows_error,
        loses_context,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path,
        asserty,
        is_hook: |_, _| false,
        // One return value; a tuple return is declared as a type and
        // read from it rather than counted here.
        return_arity: |_, _| 0,
        interfaces,
        skips_test,
        magic_exempt: &["enum_declaration", "attribute"],
        assign_kinds: &["variable_declarator"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// The words a test directory or file is built from: `.` separates the
/// parts of a project name and case separates the words inside one, so
/// `Newtonsoft.Json.Tests`, `UnitTests` and `FuzzTests` all yield
/// `tests` while `Latest` yields only itself.
const TEST_WORDS: &[&str] = &[
    "test",
    "tests",
    "testing",
    "spec",
    "specs",
    "bench",
    "benches",
    "benchmark",
    "benchmarks",
];

/// A C# test lives in a project of its own, and the project DIRECTORY
/// carries the word: `src/UnitTests`, `Src/Newtonsoft.Json.Tests`,
/// `test/Polly.Specs`, `benchmarks/Dapper.Tests.Performance`. Matching
/// `/test` and a `Tests.cs` suffix catches 595 of the gold corpus's 1668
/// test-tree files, so the C# module population reads 2032 where the
/// production code is 965: over half of it test code, and nearly all of
/// that orphaned, since nothing imports a test.
fn test_path(path: &str) -> bool {
    path.split('/').any(|segment| {
        segment
            .split('.')
            .flat_map(camel_words)
            .any(|word| TEST_WORDS.contains(&word.to_ascii_lowercase().as_str()))
    })
}

/// `UnitTests` -> `Unit`, `Tests`. An uppercase run stays whole so `DI`
/// and `IOStream` split as written.
fn camel_words(name: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = name.char_indices().collect();
    for w in chars.windows(2) {
        let (i, this) = w[0];
        let (j, next) = w[1];
        let boundary = (this.is_lowercase() || this.is_numeric()) && next.is_uppercase();
        if boundary {
            out.push(&name[start..j]);
            start = j;
        } else if this.is_uppercase() && next.is_lowercase() && i > start {
            out.push(&name[start..i]);
            start = i;
        }
    }
    out.push(&name[start..]);
    out
}

fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if node.kind() == "compilation_unit" {
        return type_references(node, src);
    }
    let text = node.utf8_text(src).unwrap_or("");
    let target = text
        .trim_start_matches("global ")
        .trim_start_matches("using")
        .trim_start_matches(" static")
        .trim()
        .trim_end_matches(';')
        .trim();
    // `using X = Y;` is an alias, and the dependency is the right side.
    let target = target.rsplit('=').next().unwrap_or(target).trim();
    if target.is_empty() {
        return Vec::new();
    }
    vec![super::ImportInfo {
        target: target.into(),
        names: Vec::new(),
        reach: super::Reach::Anywhere,
    }]
}

/// Node kinds whose every child is a type: a base list, a type-argument
/// list, and a generic constraint clause each hold nothing else.
const TYPE_LISTS: &[&str] = &[
    "base_list",
    "type_argument_list",
    "type_parameter_constraints_clause",
];

/// What a C# file depends on, read from the types it NAMES.
///
/// A `using` opens a namespace and binds no file: it makes short names
/// visible and nothing more, and a type in the file's own namespace
/// needs no `using` at all. Across the gold corpus the 7032 using
/// directives resolved 23 internal edges, and 2025 of 2032 modules read
/// as orphans with 99.8% of the corpus deletable.
///
/// The type names another file, and a C# file carries the name of the
/// type it declares: 907 of the gold corpus's 962 production files do.
/// So a type reference resolves by file stem, the way a Rust
/// `use crate::Symbol` resolves by defining module.
///
/// It is a `Mention`, not an import. The file states no dependency on
/// anything: the compiler finds `Policy` across the whole assembly and
/// nothing in the source says where it came from. The reference
/// supplies an EDGE and never a tally entry. Counting these as imports
/// would put `imports_external` at 23815 against 7009 using directives,
/// where every other language reports modules from outside rather than
/// type names the compiler resolved elsewhere.
///
/// A reference is a type when the grammar puts it in a type position:
/// the `type` field of any of the 37 kinds that have one, the `returns`
/// field of a signature, an entry of a type list, or an attribute name.
/// Outside a type position the qualifier of a member access counts too:
/// `ReflectionHelper.GetMap(x)` reaches another file's static member
/// without naming a type anywhere else.
fn type_references(root: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let mut uses = Uses::default();
    let mut stack = vec![(root, false)];
    while let Some((node, in_type)) = stack.pop() {
        if let Some(name) = names_a_type(node, in_type) {
            uses.take(name, src, super::Reach::Mention);
        }
        if let Some(member) = called_member(node, src) {
            uses.take(member, src, super::Reach::Member);
        }
        // `[DynamicDependency(...)]` names the type
        // `DynamicDependencyAttribute`: the compiler appends the suffix
        // when the short form does not resolve, and 13 of the C#
        // corpus's remaining orphans are the file that declares one.
        if let Some(long) = attribute_type(node, src) {
            uses.name(long, super::Reach::Mention);
        }
        if sealed(node.kind(), in_type) {
            continue;
        }
        let typed = type_positions(node);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push((child, in_type || typed.contains(&child.id())));
        }
    }
    uses.out
}

/// The names one file uses, each recorded once.
#[derive(Default)]
struct Uses {
    seen: std::collections::HashSet<Box<str>>,
    out: Vec<super::ImportInfo>,
}

impl Uses {
    /// One type reference, if the name can be a type at all. A lowercase
    /// name is a local or a member; `T`, `TKey`, `TResult` are the
    /// generic parameters the .NET naming guidelines spell that way,
    /// and a declared type never does.
    fn take(&mut self, node: Node, src: &[u8], reach: super::Reach) {
        let Ok(text) = node.utf8_text(src) else {
            return;
        };
        let generic_param = text
            .strip_prefix('T')
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_uppercase()));
        if text.starts_with(char::is_uppercase) && !generic_param {
            self.name(text.into(), reach);
        }
    }

    fn name(&mut self, target: Box<str>, reach: super::Reach) {
        if self.seen.insert(target.clone()) {
            self.out.push(super::ImportInfo {
                target,
                names: Vec::new(),
                reach,
            });
        }
    }
}

/// The node naming a type, where this one names one.
fn names_a_type<'t>(node: Node<'t>, in_type: bool) -> Option<Node<'t>> {
    match (node.kind(), in_type) {
        // `System.Collections.Generic.List` names one type; the
        // qualifier is the namespace it lives in.
        ("qualified_name", true) => node.child_by_field_name("name"),
        ("identifier", true) => Some(node),
        ("member_access_expression", false) => qualifying_type(node),
        _ => None,
    }
}

/// The TYPE a member access is qualified by, where it is one.
///
/// `CollectionPropertyRule<T, TElement>.Create(...)` is a static call on
/// a generic type, and the grammar spells the qualifier `generic_name`
/// rather than `identifier`. Requiring a bare `identifier` there names
/// nothing for the only two references FluentValidation makes to
/// `CollectionPropertyRule.cs` and `IncludeRule.cs`, both on
/// AbstractValidator.cs.
fn qualifying_type<'t>(access: Node<'t>) -> Option<Node<'t>> {
    bare_name(access.child_by_field_name("expression")?)
}

/// The identifier a node NAMES, with any generic wrapper taken off.
///
/// The text of a `generic_name` carries the type arguments too, and no
/// file answers to `Rule<T, TElement>` or to `CastResult<DbDataReader,
/// IDataReader>`. Written kind-agnostically because the grammar does not
/// field `CollectionPropertyRule<T, TElement>.Create` as a `generic_name`
/// consistently: matching on that kind scored 0 and the first-identifier-
/// child reading scored 5.
fn bare_name(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    node.child_by_field_name("name")
        .or_else(|| node.named_child(0))
        .filter(|n| n.kind() == "identifier")
}

/// The full type name an attribute usage means, where it is written in
/// the short form. `[Ignore]` and `[IgnoreAttribute]` are the same
/// attribute, and only the second spelling names the file.
fn attribute_type(node: Node, src: &[u8]) -> Option<Box<str>> {
    if node.kind() != "attribute" {
        return None;
    }
    let text = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    let short = text.rsplit('.').next()?;
    (short.starts_with(char::is_uppercase) && !short.ends_with("Attribute"))
        .then(|| format!("{short}Attribute").into())
}

/// The member an invocation calls on a VALUE.
///
/// `policyBuilder.CircuitBreaker(n, t)` names no type at all: an
/// extension method is invoked on its receiver and the declaring class
/// is spelled nowhere, so the member is the only name the call writes.
///
/// A receiver that is a bare capitalized IDENTIFIER is a type, and the
/// qualifier arm has already taken it: `Constants.OptionsValidation`
/// states a dependency on Constants and not on a namesake of the field.
/// Anything else is a value, INCLUDING a fluent chain that started at a
/// type: `Policy.Handle<T>().FallbackAsync(...)` is a call on the
/// builder the first call returned, and reading the whole chain's text
/// for a leading capital loses every extension method Polly's tests
/// reach that way.
///
/// The member is unwrapped the way `qualifying_type` unwraps a generic
/// qualifier; otherwise a GENERIC extension call names nothing. Dapper
/// writes `....CastResult<DbDataReader, IDataReader>()` at
/// SqlMapper.Async.cs:1098, :1124 and :1146 against `Dapper/
/// Extensions.cs`, and Polly's specs write
/// `nonGenericPolicy.AsPolicy<ResultClass>()` against
/// `ISyncPolicyExtensions.cs`, a file that reads as dead without the
/// unwrapping.
fn called_member<'t>(call: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let access = call.child_by_field_name("function")?;
    if access.kind() != "member_access_expression" {
        return None;
    }
    let receiver = access.child_by_field_name("expression")?;
    let names_a_type = receiver.kind() == "identifier"
        && receiver
            .utf8_text(src)
            .is_ok_and(|t| t.starts_with(char::is_uppercase));
    (!names_a_type).then_some(())?;
    bare_name(access.child_by_field_name("name")?)
}

/// Nodes holding no further type reference: a using directive names a
/// namespace and is recorded as one, a type PARAMETER is a declaration
/// rather than a reference, and a qualified name has already given up
/// the one type it holds.
fn sealed(kind: &str, in_type: bool) -> bool {
    matches!(kind, "using_directive" | "type_parameter_list")
        || (in_type && kind == "qualified_name")
}

/// The children this node puts in a type position: a declaration's
/// `type`, a signature's `returns`, an attribute's name, and the right
/// of `as`/`is`, which the grammar fields as `right` rather than
/// `type`, so `x as Policy` would otherwise name nothing. Every child
/// of a type list is one.
fn type_positions(node: Node) -> Vec<usize> {
    let mut cursor = node.walk();
    if TYPE_LISTS.contains(&node.kind()) {
        return node.named_children(&mut cursor).map(|c| c.id()).collect();
    }
    let kind = node.kind();
    let field = |name: &str| node.child_by_field_name(name).map(|c| c.id());
    let tested = matches!(kind, "as_expression" | "is_expression");
    [
        field("type"),
        field("returns"),
        (kind == "attribute").then(|| field("name")).flatten(),
        tested.then(|| field("right")).flatten(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    // `params string[] names` gets NO parameter node of its own: the
    // grammar inlines the type and the name into the parameter list as
    // siblings of the other parameters, with the keyword itself
    // anonymous. So a bare identifier THERE is a variadic parameter.
    // Without this it is missing from every C# signature that has one,
    // and a doc naming it reads as naming something undeclared.
    if node.kind() == "identifier" && node.parent().is_some_and(|p| p.kind() == "parameter_list") {
        return Some(ParamInfo {
            name: node.utf8_text(src).unwrap_or("").into(),
            typed: true,
            optional: true,
            splat: true,
            ..Default::default()
        });
    }
    if node.kind() != "parameter" {
        return None;
    }
    // `this PolicyBuilder policyBuilder` is the RECEIVER of an
    // extension method, not an argument: `builder.CircuitBreaker(n, t)`
    // passes two. Counting it makes every extension method read one
    // parameter wider than it is, and the receiver is what says the
    // method is reached through a value rather than through its
    // declaring class.
    let receiver = node
        .utf8_text(src)
        .unwrap_or("")
        .trim_start()
        .starts_with("this ");
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    let name = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    Some(ParamInfo {
        name: name.into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text.trim_end_matches('?')),
        boolish: type_text.starts_with("bool"),
        optional: node.child_by_field_name("default_value").is_some(),
        type_name: type_text.into(),
        selfish: receiver,
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('.').next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let f = call.child_by_field_name("function")?;
    let text = f.utf8_text(src).ok()?;
    Some(text.rsplit('.').next().unwrap_or(text))
}

fn is_async(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| t.trim_start().starts_with("async ") || t.contains(" async "))
}

/// Reflection and `dynamic` dispatch: after either, the text stops
/// predicting which member is reached.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "GetMethod"
                    | "GetProperty"
                    | "GetField"
                    | "GetType"
                    | "Invoke"
                    | "CreateInstance"
                    | "GetCustomAttributes"
            )
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "prefix_unary_expression" && text.starts_with('!'))
        .then(|| node.named_child(0))?
}

/// `catch (Exception)` reaches everything the runtime raises, and a
/// bare `catch` writes the same net with less written down. The type is
/// matched exactly, as a root name: a substring test would read
/// `catch (ArgumentException)` as broad.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "catch_clause" {
        return None;
    }
    // Emptiness is the wider sin and is asked first: the core consults
    // `swallows_error` on `if` nodes only, so a catch answers both here.
    if swallows_error(node, src) {
        return Some(super::CatchSin::Swallowed);
    }
    let Some(decl) = node.child_by_field_name("type").or_else(|| {
        let mut c = node.walk();
        node.named_children(&mut c)
            .find(|n| n.kind() == "catch_declaration")
    }) else {
        return Some(super::CatchSin::Broad);
    };
    let text = decl.utf8_text(src).unwrap_or("");
    super::catches_every_failure(text).then_some(super::CatchSin::Broad)
}

fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return true;
    };
    let text = body.utf8_text(src).unwrap_or("").trim();
    text.trim_start_matches('{')
        .trim_end_matches('}')
        .trim()
        .is_empty()
}

/// `Environment.Exit` stops the process where an exception belonged: no
/// caller answers, and no `finally` above it runs.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("Exit" | "FailFast"))
}

/// `throw new X(...)` inside a catch, with the caught exception never
/// passed on, drops the stack that explains the failure. `throw;` alone
/// preserves it and is the correct form.
///
/// Asked of the CATCH, not of the throw: the core consults this hook on
/// handler nodes, and a `throw` is a jump it never reaches, so asking
/// of the throw would leave the check dead for the language it was
/// written for.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    let Some(bound) = node
        .child_by_field_name("catch_declaration")
        .or_else(|| {
            let mut c = node.walk();
            node.named_children(&mut c)
                .find(|n| n.kind() == "catch_declaration")
        })
        .and_then(|d| d.child_by_field_name("name"))
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return false;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    super::rethrows_without_cause(body, "throw_statement", bound, src)
}

/// xUnit, NUnit and MSTest all mark a test with an attribute.
fn declares_test(node: Node, src: &[u8]) -> bool {
    if node.kind() != "method_declaration" {
        return false;
    }
    attrs(node, src).is_some_and(|a| {
        a.contains("[Fact")
            || a.contains("[Theory")
            || a.contains("[Test")
            || a.contains("[TestMethod]")
    })
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    attrs(node, src).is_some_and(|a| a.contains("Skip =") || a.contains("[Ignore"))
}

fn attrs<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "attribute_list")?
        .utf8_text(src)
        .ok()
}

/// `Assert.Equal`, `Assert.True`, `value.Should().Be(...)`: every
/// assertion library here names the assertion on the RECEIVER, so
/// reading the trailing member alone (`Equal`, `True`) finds none of
/// them and the assertion family reads zero.
fn asserty(call: Node, src: &[u8]) -> bool {
    let Some(f) = call.child_by_field_name("function") else {
        return false;
    };
    f.utf8_text(src).is_ok_and(|text| {
        text.split('.')
            .any(|seg| super::assertish(seg) || seg.starts_with("Should"))
    })
}

fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    if node.kind() != "interface_declaration" {
        return Vec::new();
    }
    let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut cursor = body.walk();
    let methods = body
        .named_children(&mut cursor)
        .filter(|c| matches!(c.kind(), "method_declaration" | "property_declaration"))
        .count();
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    let modifiers: String = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "modifier")
        .filter_map(|m| m.utf8_text(src).ok())
        .collect::<Vec<_>>()
        .join(" ");
    modifiers.contains("public") || modifiers.contains("protected")
}

/// The `///` run immediately above the declaration.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &["///"], src)
}

/// `&&`/`||` share the binary kind with arithmetic; `??` is a default
/// rather than a branch on truth and is deliberately excluded.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        Sem::If
            if node
                .parent()
                .is_some_and(|p| p.child_by_field_name("alternative") == Some(node)) =>
        {
            Sem::ElseIf
        }
        _ => sem,
    }
}

/// `override`, `virtual` and `abstract` all mark a member of the
/// inheritance contract: `virtual` supplies a default, `override`
/// replaces it, `abstract` demands one.
fn is_override(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "modifier")
        .filter_map(|m| m.utf8_text(src).ok())
        .any(|m| m == "override" || m == "virtual" || m == "abstract")
}

#[cfg(test)]
mod tests {
    use crate::lang::{Lang, Reach};

    fn facts(src: &str) -> crate::facts::FileFacts {
        let pack = Lang::CSharp.pack();
        let mut parser = pack.make_parser();
        crate::facts::extract(pack, &mut parser, std::path::Path::new("A.cs"), src)
    }

    /// The names one source reaches for, by the reach that took them.
    fn named(src: &str, want: Reach) -> Vec<String> {
        let mut out: Vec<String> = facts(src)
            .imports
            .iter()
            .filter(|i| i.reach == want)
            .map(|i| i.target.to_string())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn a_generic_name_is_the_type_it_is_built_on_at_both_ends_of_the_dot() {
        // FluentValidation/src/FluentValidation/AbstractValidator.cs:226
        // writes `CollectionPropertyRule<T, TElement>.Create(...)` and
        // :347 `IncludeRule<T>.Create(...)`. Those are the ONLY
        // references either file receives, and a qualifier arm
        // requiring a bare `identifier` names nothing for them: the
        // wrapper's own text is `CollectionPropertyRule<T, TElement>`,
        // which no file answers to.
        //
        // The member side needs the same unwrapping. Dapper writes
        // `....CastResult<DbDataReader, IDataReader>()` at
        // SqlMapper.Async.cs:1098 against `Dapper/Extensions.cs`, and
        // Polly's specs `nonGenericPolicy.AsPolicy<ResultClass>()`
        // against `ISyncPolicyExtensions.cs`, a file that reads as dead
        // without it although its own suite calls it twice.
        let src = "class C { void M(System.Data.IDataReader r) {\n\
                     CollectionPropertyRule<T, TElement>.Create(x);\n\
                     r.CastResult<DbDataReader, IDataReader>();\n\
                   } }\n";
        assert!(named(src, Reach::Mention).contains(&"CollectionPropertyRule".to_string()));
        assert!(named(src, Reach::Member).contains(&"CastResult".to_string()));
        // `T`, `TKey`, `TResult` are the generic PARAMETERS the .NET
        // naming guidelines spell that way, and no declared type does.
        for name in named(src, Reach::Mention) {
            assert_ne!(name, "TElement");
        }
    }

    #[test]
    fn a_fluent_chain_is_a_value_and_a_bare_capital_is_a_type() {
        // Polly's tests write
        // `Policy.Handle<DivideByZeroException>().FallbackAsync(...)`
        // forty times over against `Fallback/AsyncFallbackSyntax.cs`,
        // which declares `FallbackAsync` as an extension on
        // PolicyBuilder. Reading the whole chain's TEXT for a leading
        // capital sees `Policy...` and refuses it, losing every
        // extension method reached through a builder.
        let src = "class C { void M() {\n\
                     Policy.Handle<E>().FallbackAsync(a);\n\
                     Constants.Check(b);\n\
                   } }\n";
        assert!(named(src, Reach::Member).contains(&"FallbackAsync".to_string()));
        // A bare capitalized receiver is a TYPE and the qualifier arm has
        // already taken it: `Constants.Check(b)` states a dependency on
        // Constants, not on a namesake of the member.
        assert!(named(src, Reach::Mention).contains(&"Constants".to_string()));
        assert!(!named(src, Reach::Member).contains(&"Check".to_string()));
    }

    #[test]
    fn an_attribute_names_the_type_with_the_suffix_the_compiler_appends() {
        // Polly/src/Polly.Core/Registry/ResiliencePipelineRegistry.cs:64
        // writes `[NotNullWhen(true)]` and Polly/src/LegacySupport/
        // NullableAttributes.cs:71 declares `internal sealed class
        // NotNullWhenAttribute`. Counted across production files:
        // DynamicallyAccessedMembers 22 uses, NotNullWhen 20,
        // UnconditionalSuppressMessage 19, every one against a file that
        // reads as an orphan without the suffix.
        let src = "class C { [NotNullWhen(true)] [SerializableAttribute] void M() {} }\n";
        let seen = named(src, Reach::Mention);
        assert!(seen.contains(&"NotNullWhenAttribute".to_string()));
        // Only the SHORT form is completed; the long one already names
        // the file, and doubling the suffix would name nothing.
        assert!(!seen.contains(&"SerializableAttributeAttribute".to_string()));
    }

    #[test]
    fn a_this_parameter_is_the_receiver_and_not_an_argument() {
        // CircuitBreakerSyntax.cs:26 declares `CircuitBreaker(this
        // PolicyBuilder policyBuilder, int, TimeSpan)` and
        // CircuitBreakerTResultSyntax.cs:28 calls
        // `policyBuilder.CircuitBreaker(...)`: the declaring class is
        // spelled NOWHERE in the call, so the member name is the only
        // handle on the file. 40 of gold C#'s 86 orphans declare
        // nothing but extension methods.
        let f = facts(
            "static class S {\n\
               public static int CircuitBreaker(this PolicyBuilder b, int n) => n;\n\
               public static int Plain(PolicyBuilder b) => 1;\n\
             }\n",
        );
        let got: Vec<&str> = f.receiver_units.iter().map(|u| &**u).collect();
        assert_eq!(got, ["CircuitBreaker"]);
    }
}
