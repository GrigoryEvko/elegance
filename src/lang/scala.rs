//! Scala: expressions all the way down, so the branch metrics count
//! things that also produce values.
//!
//! `if` is an expression and so is `match`, which means a branch here
//! can sit inside an argument list. That is idiomatic rather than
//! suspicious, and the budgets absorb it: they are pinned to what cats
//! and zio do, not to what an imperative language would.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("function_declaration", Sem::FnDef),
    ("class_definition", Sem::TypeDef),
    ("object_definition", Sem::TypeDef),
    ("trait_definition", Sem::TypeDef),
    ("enum_definition", Sem::TypeDef),
    ("type_definition", Sem::TypeDef),
    ("given_definition", Sem::TypeDef),
    ("lambda_expression", Sem::Lambda),
    ("if_expression", Sem::If),
    ("match_expression", Sem::Match),
    ("case_clause", Sem::CaseArm),
    ("for_expression", Sem::Loop),
    ("while_expression", Sem::Loop),
    ("do_while_expression", Sem::Loop),
    ("try_expression", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("finally_clause", Sem::With),
    ("infix_expression", Sem::BoolOp),
    ("call_expression", Sem::Call),
    ("instance_expression", Sem::Call),
    ("comment", Sem::Comment),
    ("block_comment", Sem::Comment),
    ("import_declaration", Sem::Import),
    // The file itself is asked for the qualified names its BODY uses.
    // See `qualified_uses`.
    ("compilation_unit", Sem::Import),
    ("identifier", Sem::Ident),
    ("type_identifier", Sem::Ident),
    ("integer_literal", Sem::NumLit),
    ("floating_point_literal", Sem::NumLit),
    ("string", Sem::StrLit),
    ("interpolated_string", Sem::StrLit),
    // The whole `s"..."` expression, interpolator included: the inner
    // node alone is never what an argument list holds.
    ("interpolated_string_expression", Sem::StrLit),
    ("character_literal", Sem::StrLit),
    ("boolean_literal", Sem::BoolLit),
    ("return_expression", Sem::Jump),
    ("throw_expression", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    // `val`/`var` is where a local is born. Without it the live map
    // held no definition row for any local, so the repurposing check
    // had nothing to compare a rewrite against.
    ("val_definition", "pattern"),
    ("var_definition", "pattern"),
    ("function_definition", "name"),
    ("function_declaration", "name"),
    ("class_definition", "name"),
    ("object_definition", "name"),
    ("trait_definition", "name"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("field_expression", "value");

/// `Any` and `AnyRef` sit at the top of the hierarchy and say nothing;
/// `asInstanceOf` is the cast that goes with them.
const LOOSE: &[&str] = &["Any", "AnyRef", "AnyVal"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_scala::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Scala,
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
        return_type_field: "return_type",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: true,
        // A case class declares the shape, and that is the idiom.
        record_keys: |_, _| None,
        // Concurrency is Future, ZIO and cats-effect — library, and the
        // library is the whole point.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["/**"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override,
        spooky,
        negation_operand,
        catch_sin,
        swallows_error: |_, _| false,
        loses_context,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path,
        asserty,
        is_hook: |_, _| false,
        return_arity: |_, _| 0,
        interfaces,
        skips_test,
        magic_exempt: &["enum_definition"],
        assign_kinds: &["val_definition", "var_definition"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
        .or_else(|| test_label(node))
}

/// An import states a PATH and then what it takes from it, and each name
/// it takes is a separate dependency: `import cats.data.{NonEmptyList,
/// Chain}` names two files, not the package they share. Reading only the
/// text before the brace left 1380 selector lists in the gold corpus
/// pointing at a package that no module component vector can match.
///
/// A wildcard — `._`, Scala 3's `.*`, `.given` — names the path itself.
/// Only `._` was trimmed, so 949 Scala 3 wildcards carried a literal
/// `.*` into the resolver.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if node.kind() == "compilation_unit" {
        return qualified_uses(node, src);
    }
    let mut cursor = node.walk();
    let path: Vec<&str> = node
        .children_by_field_name("path", &mut cursor)
        .filter_map(|n| n.utf8_text(src).ok())
        .filter(|t| *t != ".")
        .collect();
    if path.is_empty() {
        return Vec::new();
    }
    let prefix = path.join(".");
    let mut targets = Vec::new();
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        match child.kind() {
            // `import a.b._` / `.*` / `.given` — the path is the target.
            "namespace_wildcard" => targets.push(prefix.clone()),
            "as_renamed_identifier" => selector(&prefix, child, src, &mut targets),
            "namespace_selectors" => {
                let mut inner = child.walk();
                for sel in child.named_children(&mut inner) {
                    selector(&prefix, sel, src, &mut targets);
                }
            }
            _ => {}
        }
    }
    // `import a.b.C` — no selector list, so the path already names it.
    if targets.is_empty() {
        targets.push(prefix);
    }
    targets
        .iter()
        .map(|t| super::ImportInfo {
            target: super::java::type_path(t).into(),
            // A selector list is already expanded one target per name,
            // so the leaf of each names what that import binds. See
            // `java::leaf`.
            names: super::java::leaf(t).map(Into::into).into_iter().collect(),
            reach: super::Reach::Anywhere,
        })
        .collect()
}

/// What a file depends on WITHOUT importing it: Scala resolves a fully
/// qualified name on the spot, so `cats.compat.Seq.zipWith(fa, fb)` at
/// core/src/main/scala/cats/instances/seq.scala:192 is the only
/// statement cats makes about `cats/compat/Seq.scala`, and there is no
/// import line to read. All nine files under `cats/compat` read as
/// depended on by nothing.
///
/// A `Mention`, never an import: the file states no dependency — it
/// spells a name the compiler resolves against the whole classpath — so
/// it supplies an edge and no tally entry, exactly as a C# type
/// reference does.
///
/// The shape is a dotted chain that OPENS lower case and reaches a
/// capitalized segment: a package path ending at a type. `foo.bar` is a
/// field access and names nothing, and `Foo.bar` is a member of a type
/// the ordinary reference machinery already sees.
fn qualified_uses(root: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        if node.kind() != "field_expression" && node.kind() != "stable_identifier" {
            continue;
        }
        let Ok(text) = node.utf8_text(src) else {
            continue;
        };
        let Some(target) = package_qualified(text) else {
            continue;
        };
        if seen.insert(target.to_string()) {
            out.push(super::ImportInfo {
                target: target.into(),
                names: super::java::leaf(target)
                    .map(Into::into)
                    .into_iter()
                    .collect(),
                reach: super::Reach::Mention,
            });
        }
    }
    out
}

/// The type a dotted chain names, when the chain is a PACKAGE path.
/// `cats.compat.Seq.zipWith` -> `cats.compat.Seq`; `x.foo.bar` and
/// `Chunk.empty` yield nothing.
fn package_qualified(text: &str) -> Option<&str> {
    let mut segs = text.split('.');
    let first = segs.next()?;
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
    // `this` and `super` open a chain through the OBJECT, not through a
    // package, so `this.foo.Bar` names a member and no file.
    if !plain(first) || !first.starts_with(|c: char| c.is_lowercase()) {
        return None;
    }
    if matches!(first, "this" | "super") {
        return None;
    }
    if !segs.clone().all(plain) || !segs.any(|s| s.starts_with(char::is_uppercase)) {
        return None;
    }
    Some(super::java::type_path(text))
}

/// One entry of a selector list: a name, a rename whose left side is the
/// name (`X => Y`, `X as Y`), or a wildcard standing for the path.
fn selector(prefix: &str, sel: Node, src: &[u8], out: &mut Vec<String>) {
    let name = match sel.kind() {
        "namespace_wildcard" | "wildcard" => {
            out.push(prefix.to_string());
            return;
        }
        "arrow_renamed_identifier" | "as_renamed_identifier" => sel.child_by_field_name("name"),
        _ => Some(sel),
    };
    let Some(text) = name.and_then(|n| n.utf8_text(src).ok()) else {
        return;
    };
    // A selector may also be a type expression or an operator; only a
    // plain name can name a file.
    if !text.is_empty() && text != "given" && text.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        out.push(format!("{prefix}.{text}"));
    }
}

/// sbt names the production source set `main` and cross-building splits
/// it across `scala`, `scala-2`, `scala-3` and `scala-2.13+` roots, so
/// `zio/test/shared/src/main/scala` is ZIO's published test FRAMEWORK
/// while `zio/core-tests/shared/src/test/scala` is a test.
///
/// Reading the path for `/test/` instead dropped 188 production files —
/// 175 of them because a `zio.test` source path carries the segment —
/// and every edge into them with it.
fn test_path(p: &str) -> bool {
    if scalafix_fixture(p) {
        return true;
    }
    match super::java::source_set(p, |c| c.starts_with("scala") || c == "java") {
        Some(set) => set != "main",
        // No source-set layout: the name is all there is.
        None => p.contains("/test/") || p.ends_with("Spec.scala") || p.ends_with("Suite.scala"),
    }
}

/// scalafix-testkit compiles an `input` tree, applies a rewrite rule to
/// it, and diffs the result against an `output` tree. Both sit under
/// `src/main/scala`, so the source set calls them production.
///
/// The build file says otherwise outright. cats/scalafix/build.sbt
/// writes `scalafixTestkitOutputSourceDirectories :=
/// sourceDirectories.in(v1_0_0_output, Compile).value` and makes the
/// `tests` project compile depending on the `input` one. All 75 such
/// files in the gold corpus read as orphans, and nothing else lives
/// under those directories.
fn scalafix_fixture(p: &str) -> bool {
    let comps: Vec<&str> = p.split('/').collect();
    let Some(at) = comps.iter().rposition(|c| c.starts_with("scalafix")) else {
        return false;
    };
    comps[at + 1..]
        .iter()
        .any(|c| matches!(*c, "input" | "output"))
}

fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if !matches!(node.kind(), "parameter" | "class_parameter") {
        return None;
    }
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    let name = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    Some(ParamInfo {
        name: name.into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text),
        boolish: type_text == "Boolean",
        optional: node.child_by_field_name("default_value").is_some(),
        // `xs: Int*` — a repeated parameter takes as many arguments as
        // the caller writes, and names none of them.
        splat: ty.is_some_and(|t| t.kind() == "repeated_parameter_type"),
        type_name: type_text.into(),
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
    Some(text.rsplit('.').next().unwrap_or(text).trim())
}

/// Runtime reflection and the cast that skips the checker.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some("asInstanceOf" | "getClass" | "getDeclaredMethod" | "reflect")
        )
}

/// `catch { case e: Exception => ... }`. The arms are where the width
/// lives: a root type or a bare `_` reaches everything the runtime can
/// raise. The root name is matched exactly, so `case e: ClassCastException`
/// — which contains the substring the first version tested for — is narrow.
///
/// EMPTINESS IS ASKED OF EVERY ARM, and the exact match is why. Silence
/// is a sin the caught type does not excuse: `case _: SecurityException =>`
/// with nothing after it loses the failure exactly as a wildcard would,
/// and every other pack in the tree judges an empty handler without
/// asking what it caught. This pack asked breadth first, so narrowing
/// breadth would otherwise have taken two real gold findings with it.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "catch_clause" {
        return None;
    }
    let mut broad = false;
    for arm in case_arms(node) {
        let text = |field| {
            arm.child_by_field_name(field)
                .and_then(|n| n.utf8_text(src).ok())
                .unwrap_or("")
                .trim()
        };
        let body = text("body");
        if body.is_empty() || body == "()" {
            return Some(super::CatchSin::Swallowed);
        }
        // `_` is a wildcard rather than a type name, so it is its own
        // clause; everything else is a root type matched exactly.
        let pattern = text("pattern");
        broad |= pattern == "_" || super::catches_every_failure(pattern);
    }
    broad.then_some(super::CatchSin::Broad)
}

/// A catch arm that throws a NEW exception and never names the one it
/// bound: the stack that explains the failure is gone.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    case_arms(node).into_iter().any(|arm| {
        let Some(bound) = arm
            .child_by_field_name("pattern")
            .and_then(|p| p.child_by_field_name("pattern"))
            .and_then(|n| n.utf8_text(src).ok())
        else {
            return false;
        };
        arm.child_by_field_name("body")
            .is_some_and(|body| super::rethrows_without_cause(body, "throw_expression", bound, src))
    })
}

/// The `case` arms of a catch, which the grammar wraps in a case_block.
fn case_arms<'t>(clause: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = clause.walk();
    let Some(block) = clause
        .named_children(&mut cursor)
        .find(|c| c.kind() == "case_block")
    else {
        return Vec::new();
    };
    let mut inner = block.walk();
    block
        .named_children(&mut inner)
        .filter(|c| c.kind() == "case_clause")
        .collect()
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "prefix_expression" && text.starts_with('!')).then(|| node.named_child(0))?
}

/// `sys.error` and a thrown exception both leave by the same door.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("error" | "require" | "assume"))
}

/// ScalaTest and munit both write `test("name") { body }`: a call whose
/// CALLEE is itself a call carrying the name, and whose argument is the
/// block that is the test. `refine` promotes the whole thing to a unit,
/// because the block alone is not a node the ontology opens.
fn declares_test(node: Node, src: &[u8]) -> bool {
    let Some(inner) = node.child_by_field_name("function") else {
        return false;
    };
    if inner.kind() != "call_expression" {
        return false;
    }
    matches!(
        callee_text(inner, src),
        Some("test" | "it" | "property" | "check")
    ) && test_label(node).is_some()
}

/// A declared test's name is the string its first call carries — prose
/// rather than an identifier, exactly as a Zig test label is.
fn test_label(node: Node) -> Option<Node> {
    let args = node
        .child_by_field_name("function")?
        .child_by_field_name("arguments")?;
    args.named_child(0)
        .filter(|n| matches!(n.kind(), "string" | "interpolated_string_expression"))
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(callee_text(node, src), Some("ignore" | "pending"))
}

fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src)
        .is_some_and(|t| super::assertish(t) || matches!(t, "shouldBe" | "shouldEqual" | "expect"))
}

/// A trait carries method bodies, so only its ABSTRACT members are the
/// contract an implementor must satisfy.
fn interfaces(node: Node, src: &[u8]) -> Vec<crate::facts::InterfaceFact> {
    if node.kind() != "trait_definition" {
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
        .filter(|c| matches!(c.kind(), "function_declaration" | "val_declaration"))
        .count();
    vec![crate::facts::InterfaceFact {
        name: name.into(),
        line: node.start_position().row as u32 + 1,
        methods: methods as u16,
    }]
}

/// Public is the DEFAULT here, so the surface is everything without an
/// access modifier saying otherwise.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    !node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "modifiers")
        .filter_map(|m| m.utf8_text(src).ok())
        .any(|m| m.contains("private") || m.contains("protected"))
}

fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["block_comment"], &["/**"], src)
}

/// `infix_expression` is every operator in the language, and in Scala
/// that includes every METHOD called without a dot. Only the two that
/// sequence a condition are boolean.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        // `test("name") { ... }` — the block is the test body, and the
        // grammar gives it no node of its own, so the CALL becomes the
        // unit the test metrics judge.
        Sem::Call if declares_test(node, src) => Sem::FnDef,
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

/// `override`, and a member of a trait, which carries defaults the way
/// an interface with bodies does.
fn is_override(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "modifiers")
        .filter_map(|m| m.utf8_text(src).ok())
        .any(|m| m.contains("override"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_scalafix_rewrite_fixture_is_test_data_wherever_it_is_filed() {
        // scalafix-testkit compiles an `input` tree, applies a rule to
        // it and diffs the result against `output`. Both sit under
        // `src/main/scala`, so the source set calls them production, and
        // all 75 in the gold corpus read as orphans. cats/scalafix's own
        // build.sbt says otherwise outright.
        let fixture = "cats/scalafix/v1_0_0/input/src/main/scala/fix/RemoveCartesian.scala";
        assert!(super::test_path(fixture));
        assert!(super::test_path(&fixture.replace("/input/", "/output/")));
        // The `scalafix` root is half the rule: an ordinary `input`
        // directory elsewhere is somebody's production code.
        assert!(!super::test_path(
            "cats/core/src/main/scala/input/Parser.scala"
        ));
    }

    #[test]
    fn a_fully_qualified_name_is_a_dependency_with_no_import_to_read() {
        // `cats.compat.Seq.zipWith(fa, fb)` at
        // core/src/main/scala/cats/instances/seq.scala:192 is the only
        // statement cats makes about cats/compat/Seq.scala. All nine
        // files under `cats/compat` read as depended on by nothing.
        let cut = |t| super::package_qualified(t);
        assert_eq!(cut("cats.compat.Seq.zipWith"), Some("cats.compat.Seq"));
        assert_eq!(cut("zio.Chunk"), Some("zio.Chunk"));
        // A field access names no package, and a member of a type the
        // ordinary reference machinery already sees is not one either.
        assert_eq!(cut("config.server.port"), None);
        assert_eq!(cut("Chunk.empty"), None);
        assert_eq!(cut("this.foo.Bar"), None);
    }
}
