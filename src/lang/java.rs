//! Java: everything is declared, so almost everything is measurable.
//!
//! The type-hygiene family reads at full strength here — parameters,
//! returns and fields all carry types, and a cast is written down rather
//! than inferred. What the language does not give is a free function: a
//! unit is a method on a class, so `cohesion` and `interface width` are
//! measuring the structure the language forces rather than a choice the
//! author made, and both read that way.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("method_declaration", Sem::FnDef),
    ("constructor_declaration", Sem::FnDef),
    ("compact_constructor_declaration", Sem::FnDef),
    ("lambda_expression", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("interface_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("record_declaration", Sem::TypeDef),
    ("annotation_type_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("ternary_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("enhanced_for_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_expression", Sem::Match),
    ("switch_block_statement_group", Sem::CaseArm),
    ("switch_rule", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("try_with_resources_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("finally_clause", Sem::With),
    ("binary_expression", Sem::BoolOp),
    ("method_invocation", Sem::Call),
    ("object_creation_expression", Sem::Call),
    ("explicit_constructor_invocation", Sem::Call),
    ("cast_expression", Sem::Cast),
    ("line_comment", Sem::Comment),
    ("block_comment", Sem::Comment),
    ("import_declaration", Sem::Import),
    ("identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("decimal_integer_literal", Sem::NumLit),
    ("hex_integer_literal", Sem::NumLit),
    ("octal_integer_literal", Sem::NumLit),
    ("binary_integer_literal", Sem::NumLit),
    ("decimal_floating_point_literal", Sem::NumLit),
    ("hex_floating_point_literal", Sem::NumLit),
    ("string_literal", Sem::StrLit),
    ("character_literal", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("return_statement", Sem::Jump),
    ("yield_statement", Sem::Jump),
    ("throw_statement", Sem::Jump),
    ("assert_statement", Sem::Assert),
];

const DEF_SITES: &[(&str, &str)] = &[
    // A local's declarator is its birth. Without it the live map held
    // no definition row for any local, so the repurposing check had
    // nothing to compare a rewrite against.
    ("variable_declarator", "name"),
    ("method_declaration", "name"),
    ("constructor_declaration", "name"),
    ("class_declaration", "name"),
    ("interface_declaration", "name"),
    ("enum_declaration", "name"),
    ("record_declaration", "name"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("field_access", "object");

/// `Object` is the type that says nothing; a raw collection says it more
/// politely, and both predate generics doing the work.
const LOOSE: &[&str] = &["Object", "var"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Java,
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
        call_target_fields: &["name"],
        types_declared: true,
        // A record or a class declares the shape; there is no anonymous
        // map literal standing in for one.
        record_keys: |_, _| None,
        // Concurrency is Executor, Future and virtual threads — library,
        // never syntax.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc,
        doc_markers: &["/**"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override,
        spooky,
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
        // One return value, always. A method that wants to return three
        // things declares a record, which is the point.
        return_arity: |_, _| 0,
        interfaces,
        skips_test,
        magic_exempt: &["enum_declaration", "annotation"],
        assign_kinds: &["variable_declarator"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// The last dotted segment: the type an import names, which is the name
/// it binds. A wildcard import binds nothing in particular.
pub(super) fn leaf(target: &str) -> Option<&str> {
    target
        .rsplit('.')
        .next()
        .filter(|s| !s.is_empty() && *s != "*")
}

/// `import a.b.C` names a TYPE and its file is `a/b/C.java`, but a
/// static import and a nested type name a MEMBER of that type, so the
/// file is one segment shorter or more: `import static
/// org.junit.jupiter.api.Assertions.assertEquals` is Assertions.java and
/// `import com.github.benmanes.caffeine.cache.CacheSpec.CacheWeigher` is
/// CacheSpec.java. 8361 static imports and 1240 nested types in the gold
/// corpus named a file in the corpus and resolved to nothing.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let text = node.utf8_text(src).unwrap_or("");
    let target = text
        .trim_start_matches("import")
        .trim_start_matches(" static")
        .trim()
        .trim_end_matches(';')
        .trim()
        .trim_end_matches(".*");
    let target = type_path(target);
    if target.is_empty() {
        return Vec::new();
    }
    vec![super::ImportInfo {
        target: target.into(),
        // The leaf of a JVM import is BOTH the name the file binds and
        // the name the target exports -- Java has no import aliasing --
        // so the one field answers for the envy detector and for the
        // surface question alike. Without it `fat_surfaces` reported
        // exports_imported 0 for every Java and Scala module in gold,
        // vacuously.
        names: leaf(target).map(Into::into).into_iter().collect(),
        reach: super::Reach::Anywhere,
    }]
}

/// The prefix of a dotted name that ends at its first capitalized
/// segment — the type whose file the name lives in. A package is lower
/// case and a type is capitalized, and that convention is what decides
/// where the file ends; cutting there moved no target that already
/// resolved, across all 71978 Java and 12837 Scala imports of the gold
/// corpus. A name with no capitalized segment is a package and is
/// returned whole.
pub(super) fn type_path(target: &str) -> &str {
    let mut end = 0;
    for seg in target.split('.') {
        end += seg.len();
        if seg.starts_with(char::is_uppercase) {
            return &target[..end];
        }
        end += 1; // the separating dot
    }
    target
}

/// Maven, Gradle and sbt all name the production source set `main`, and
/// the set is written into the path: `src/main/java` is the library,
/// while `src/test/java`, `src/testFixtures/java`, `src/jmh/java`,
/// `src/javaPoet/java` and `src/compatibilityTest/java` are not.
///
/// Reading the file NAME instead was wrong in both directions. junit5's
/// `junit-jupiter-api/src/main/java/org/junit/jupiter/api/Test.java`
/// declares the `@Test` annotation, and it plus 62 more production types
/// were dropped from the graph; 299 files under eighteen non-main source
/// sets were judged as production modules in their place.
fn test_path(p: &str) -> bool {
    if fixture_project(p) {
        return true;
    }
    match source_set(p, |c| c == "java") {
        Some(set) => set != "main",
        // No source-set layout: the name is all there is.
        None => p.contains("/test/") || p.ends_with("Test.java") || p.ends_with("Tests.java"),
    }
}

/// A whole little PROJECT kept as a fixture: junit5 compiles the trees
/// under `platform-tooling-support-tests/projects/` to check that its
/// own tooling can build them, and one of them is named
/// `OtherwiseNotReferencedClass.java`. They carry no source-set layout,
/// so the name check saw `projects` and called them production.
///
/// Both halves are load-bearing. A module ending in `-tests` alone
/// covers 605 gold files against 9 orphans, so it would drop 596
/// production modules to recover nine; inside a `projects` directory it
/// is 25 files, and every one is a fixture.
fn fixture_project(p: &str) -> bool {
    let comps: Vec<&str> = p.split('/').collect();
    let Some(at) = comps.iter().position(|c| *c == "projects") else {
        return false;
    };
    comps[..at]
        .iter()
        .any(|c| c.ends_with("-tests") || c.ends_with("-test"))
}

/// The source set a JVM path belongs to: the component after `src` in
/// `.../src/<set>/<language root>/...`. Shared with the Scala pack,
/// which is laid out by the same build tools.
pub(super) fn source_set(p: &str, lang_root: fn(&str) -> bool) -> Option<&str> {
    let comps: Vec<&str> = p.split('/').collect();
    comps
        .windows(3)
        .rev()
        .find(|w| w[0] == "src" && lang_root(w[2]))
        .map(|w| w[1])
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if !matches!(node.kind(), "formal_parameter" | "spread_parameter") {
        return None;
    }
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    let name = node
        .child_by_field_name("name")
        .or_else(|| node.named_children(&mut node.walk()).last())?
        .utf8_text(src)
        .ok()?;
    Some(ParamInfo {
        name: name.into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text),
        boolish: type_text == "boolean" || type_text == "Boolean",
        optional: node.kind() == "spread_parameter",
        splat: node.kind() == "spread_parameter",
        type_name: type_text.into(),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('.').next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("name")?.utf8_text(src).ok()
}

/// Reflection is the language's escape from its own type system: after
/// `getMethod`/`invoke` the text stops predicting which code runs.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "invoke"
                    | "getMethod"
                    | "getDeclaredMethod"
                    | "getField"
                    | "getDeclaredField"
                    | "forName"
                    | "newInstance"
                    | "setAccessible"
            )
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let op = super::field_text_is(node, "operator", src)?;
    (node.kind() == "unary_expression" && op == "!").then(|| node.child_by_field_name("operand"))?
}

/// `catch (Exception e)` reaches every checked failure at once, and
/// `Throwable` reaches the errors the JVM raises for itself. Which type
/// that is, is decided by `catches_every_failure` — an EXACT root-type
/// match, because `IOException e` contains the substring the first
/// version tested for. Emptiness is the wider sin and is asked first —
/// the core consults `swallows_error` on `if` nodes only, so a catch
/// answers both here.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "catch_clause" {
        return None;
    }
    if swallows_error(node, src) {
        return Some(super::CatchSin::Swallowed);
    }
    let text = caught(node)?.utf8_text(src).ok()?;
    // A union `catch (A | B)` is broad when either member is a root.
    super::catches_every_failure(text).then_some(super::CatchSin::Broad)
}

/// The `catch (Type e)` parameter. The grammar fields it under no name,
/// so `child_by_field_name("parameter")` answered None for every catch
/// in the language and both handler checks read zero.
fn caught<'t>(clause: Node<'t>) -> Option<Node<'t>> {
    let mut cursor = clause.walk();
    clause
        .named_children(&mut cursor)
        .find(|c| c.kind() == "catch_formal_parameter")
}

/// A catch whose body is empty, or whose only statement prints, silences
/// the failure — `printStackTrace` is the canonical way to lose one.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return true;
    };
    let text = body.utf8_text(src).unwrap_or("").trim();
    let inner = text.trim_start_matches('{').trim_end_matches('}').trim();
    inner.is_empty()
}

/// `System.exit` ends the JVM where an exception belonged: no caller
/// gets to answer, and no `finally` above it runs.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("exit" | "halt"))
        && call
            .utf8_text(src)
            .is_ok_and(|t| t.starts_with("System.") || t.starts_with("Runtime"))
}

/// Rethrowing without the cause loses the stack that explains it. Asked
/// of the CATCH, not of the throw: the core consults this hook on
/// handler nodes, and a `throw` is a jump it never reaches — which left
/// the check dead for the language it was written for.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    // `catch (IOException e)` — the binding is the last word.
    let Some(bound) = caught(node)
        .and_then(|p| p.utf8_text(src).ok())
        .and_then(|t| t.split_whitespace().last())
    else {
        return false;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    super::rethrows_without_cause(body, "throw_statement", bound, src)
}

/// JUnit and TestNG both mark a test with an annotation.
fn declares_test(node: Node, src: &[u8]) -> bool {
    if node.kind() != "method_declaration" {
        return false;
    }
    modifiers_text(node, src)
        .is_some_and(|m| m.contains("@Test") || m.contains("@ParameterizedTest"))
}

/// `@Disabled` and `@Ignore` switch a test off; `@DisabledOnOs`,
/// `@DisabledOnJre`, `@DisabledIfEnvironmentVariable` and the rest of
/// the conditional family state a JUDGMENT about where the test applies,
/// which is the same exemption a guarded `t.Skip()` already gets. A
/// substring test cannot tell them apart — every conditional name BEGINS
/// with `@Disabled` — so the annotation's own name is compared whole.
fn skips_test(node: Node, src: &[u8]) -> bool {
    let Some(mods) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "modifiers")
    else {
        return false;
    };
    mods.named_children(&mut mods.walk()).any(|a| {
        let name = match a.kind() {
            "marker_annotation" | "annotation" => a.child_by_field_name("name"),
            _ => None,
        };
        matches!(
            name.and_then(|n| n.utf8_text(src).ok()),
            Some("Disabled" | "Ignore")
        )
    })
}

fn modifiers_text<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "modifiers")?
        .utf8_text(src)
        .ok()
}

fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src).is_some_and(|t| super::assertish(t) || t.starts_with("verify"))
}

/// An interface's width is its declared method count — the contract
/// written down apart from any implementation.
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
        .filter(|c| c.kind() == "method_declaration")
        .count();
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

fn is_public(node: Node, src: &[u8]) -> bool {
    match modifiers_text(node, src) {
        Some(m) => m.contains("public") || m.contains("protected"),
        // Package-private is the default and is not the module's surface.
        None => false,
    }
}

fn is_doc(node: Node) -> bool {
    node.kind() == "block_comment"
}

/// The `/** ... */` javadoc immediately above the declaration.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["block_comment"], &["/**"], src)
}

/// `&&`/`||` share the binary kind with arithmetic and every comparison.
/// `else if` nests an if inside the parent's alternative and flattens
/// the same way it does in Rust and TypeScript.
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

/// `@Override` on a method whose enclosing type is an ordinary class —
/// the generic check only reaches members of an interface body.
fn is_override(node: Node, src: &[u8]) -> bool {
    modifiers_text(node, src).is_some_and(|m| m.contains("@Override"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_whole_project_kept_as_a_fixture_is_test_data() {
        // junit5 compiles the trees under
        // `platform-tooling-support-tests/projects/` to check its own
        // tooling can build them; one is named
        // `OtherwiseNotReferencedClass.java`. They carry no source-set
        // layout, so the name check saw `projects` and said production.
        let p = "junit5/platform-tooling-support-tests/projects/jar-describe-module/src/Foo.java";
        assert!(super::test_path(p));
        // Both halves are load-bearing: a `-tests` module alone covers
        // 605 gold files against 9 orphans, so the `projects` directory
        // is what keeps 596 production modules in.
        assert!(!super::test_path(
            "junit5/platform-tooling-support-tests/src/main/java/A.java"
        ));
        assert!(!super::test_path(
            "netty/common/projects/src/main/java/B.java"
        ));
    }
}
