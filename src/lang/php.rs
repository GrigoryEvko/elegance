//! PHP: the language that grew types from the outside in.
//!
//! Almost alone here it can be read at two ages at once. The same
//! codebase may hold an untyped array-shaped function from 2009 and a
//! `final readonly class` with union types and `match` from last year,
//! and both are ordinary PHP. That gives the type-hygiene family a
//! second reading: `untyped params` measures a migration rather than a
//! habit, and it is why the modern corpus here is deliberately code that
//! made the move: Composer, PHPStan, PHP-Parser.
//!
//! A `.php` file may hold HTML with islands of code, and the grammar
//! reads both. Text outside `<?php` is `text` and contributes no units,
//! so a template-heavy file measures as small rather than as wrong.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("method_declaration", Sem::FnDef),
    ("anonymous_function", Sem::Lambda),
    ("arrow_function", Sem::Lambda),
    ("class_declaration", Sem::TypeDef),
    ("interface_declaration", Sem::TypeDef),
    ("trait_declaration", Sem::TypeDef),
    ("enum_declaration", Sem::TypeDef),
    ("if_statement", Sem::If),
    ("else_if_clause", Sem::ElseIf),
    ("else_clause", Sem::Else),
    ("conditional_expression", Sem::Ternary),
    ("for_statement", Sem::Loop),
    ("foreach_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("do_statement", Sem::Loop),
    ("switch_statement", Sem::Match),
    ("match_expression", Sem::Match),
    ("case_statement", Sem::CaseArm),
    // The catch-all arm is its own kind here, and leaving it unmapped
    // leaves every `switch` looking exhaustive to the wildcard check.
    ("default_statement", Sem::CaseArm),
    ("match_conditional_expression", Sem::CaseArm),
    ("match_default_expression", Sem::CaseArm),
    ("try_statement", Sem::Try),
    ("catch_clause", Sem::Catch),
    ("finally_clause", Sem::With),
    ("binary_expression", Sem::BoolOp),
    ("function_call_expression", Sem::Call),
    ("member_call_expression", Sem::Call),
    ("scoped_call_expression", Sem::Call),
    ("object_creation_expression", Sem::Call),
    ("cast_expression", Sem::Cast),
    ("comment", Sem::Comment),
    ("namespace_use_declaration", Sem::Import),
    // PHP's OTHER module system. `use` names a class for the autoloader;
    // `include`/`require` name a FILE. Without all four spellings here
    // the pack reads none of the corpus's 66 include statements. See
    // `included_file`.
    ("include_expression", Sem::Import),
    ("include_once_expression", Sem::Import),
    ("require_expression", Sem::Import),
    ("require_once_expression", Sem::Import),
    // The file itself is asked for its imports, because most of a PHP
    // file's dependencies are not `use` statements. See
    // `class_references`.
    ("program", Sem::Import),
    ("name", Sem::Ident),
    ("variable_name", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("encapsed_string", Sem::StrLit),
    ("boolean", Sem::BoolLit),
    ("break_statement", Sem::Jump),
    ("continue_statement", Sem::Jump),
    ("return_statement", Sem::Jump),
    ("goto_statement", Sem::Goto),
    ("throw_expression", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("function_definition", "name"),
    ("method_declaration", "name"),
    ("class_declaration", "name"),
    ("interface_declaration", "name"),
    ("trait_declaration", "name"),
    // A variable is BORN at its first assignment: there is no `var`.
    // Without this the live map holds no definition row for any local,
    // so the repurposing check has nothing to compare a rewrite
    // against and every span reads zero.
    ("assignment_expression", "left"),
];

const REASSIGNS: &[(&str, &str)] = &[("assignment_expression", "left")];
const ATTR: (&str, &str) = ("member_access_expression", "object");

/// `mixed` is the type that says "I gave up"; `array` without a shape
/// says it more politely, and is the pre-generics idiom this language
/// spent a decade escaping.
const LOOSE: &[&str] = &["mixed", "array", "object", "iterable"];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Php,
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
        return_type_field: "return_type",
        bool_op_field: "operator",
        call_target_fields: &["function", "name"],
        types_declared: true,
        record_keys,
        // A resource closes when the last reference drops; there is no
        // scope-guard statement whose absence would be the finding.
        // Fibers exist but concurrency is a library concern here.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc,
        // The docblock is the language's documentation culture, and
        // static analysers read its annotations as types.
        doc_markers: &["/**"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
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
        // A suite here is named freely — flysystem writes `test_files/`
        // and PHP-Parser `test_old/` — so the directory is matched by
        // what its name CONTAINS. See `lang::test_dir`.
        test_path: |p| super::test_dir(p) || p.ends_with("Test.php"),
        asserty: |call, src| callee_text(call, src).is_some_and(super::assertish),
        is_hook: |_, _| false,
        return_arity,
        interfaces,
        skips_test,
        magic_exempt: &["array_creation_expression", "enum_declaration"],
        assign_kinds: &[
            "assignment_expression",
            "augmented_assignment_expression",
            // `const API_KEY = '...'` and `private string $token = '...'`
            // are the two places a class keeps a value under a name, and
            // both are where a pasted credential lands.
            "const_element",
            "property_element",
        ],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// `use Foo\Bar;` when asked of the declaration, and every other way a
/// PHP file names a class when asked of the file.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    match node.kind() {
        "namespace_use_declaration" => namespace_uses(node, src),
        "include_expression"
        | "include_once_expression"
        | "require_expression"
        | "require_once_expression" => included_file(node, src).into_iter().collect(),
        _ => class_references(node, src),
    }
}

/// `require __DIR__ . '/compatibility_tokens.php'`: the other half of
/// PHP's module story, and the half the autoloader never sees.
///
/// PHP-Parser's Lexer.php:5 is that line, and unread it leaves
/// compatibility_tokens.php depended on by nothing; flysystem's
/// phpunit.php:4-5 pulls in AdapterTestUtilities/test-functions.php and
/// mocked-functions.php the same way, and phpunit.xml.dist:2 names
/// phpunit.php as the suite bootstrap. The gold corpus holds 66
/// include/require statements.
///
/// `__DIR__` is REQUIRED, and it makes the rest readable. Without
/// it the specifier is relative to the process's working directory or is
/// built from a variable, and neither can be known without running the
/// program. With it the base is the including file's own directory, so
/// the string pieces of the concatenation — read in document order —
/// spell a path this tool can check against the tree.
fn included_file(node: Node, src: &[u8]) -> Option<super::ImportInfo> {
    let mut here = false;
    let mut path = String::new();
    let mut stack = vec![node];
    let mut pieces: Vec<(usize, Box<str>)> = Vec::new();
    while let Some(n) = stack.pop() {
        match n.kind() {
            "name" if n.utf8_text(src) == Ok("__DIR__") => here = true,
            "string" | "encapsed_string" => {
                if let Ok(text) = n.utf8_text(src) {
                    pieces.push((n.start_byte(), text.trim_matches(['"', '\'']).into()));
                }
            }
            _ => {
                let mut cursor = n.walk();
                stack.extend(n.named_children(&mut cursor));
            }
        }
    }
    pieces.sort_unstable();
    for (_, piece) in pieces {
        path.push_str(&piece);
    }
    // A relative path always holds a separator and a namespace never
    // does, which tells the two apart at the resolver.
    (here && path.contains('/')).then(|| super::ImportInfo {
        target: path.into(),
        names: Vec::new(),
        reach: super::Reach::Project,
    })
}

/// Node kinds every one of whose class-shaped children is a class: an
/// extends list, an implements list, an attribute, a trait use, a `new`,
/// and a type hint (every declared type in the grammar — parameter,
/// return, property, `catch` — reaches a `named_type`).
const CLASS_SITES: &[&str] = &[
    "base_clause",
    "class_interface_clause",
    "attribute",
    "use_declaration",
    "object_creation_expression",
    "named_type",
];

/// A class name is a reference whether or not a `use` introduced it.
///
/// `use` covers only the classes from ANOTHER namespace. A class in the
/// file's own namespace is named bare and imports nothing, and a class
/// in a namespace below it is written out: PHP-Parser spells
/// `Comment\Doc`, `Lexer\Emulative` and `Builder\Class_` that way and
/// imports none of them, so a reader of `use` statements alone calls
/// 181 of its 274 modules orphans while only 4 are never named by
/// another production file.
///
/// A bare name that a `use` already bound is skipped: it names that
/// import, which is recorded at the declaration, and recording it again
/// under its short name would let it match an unrelated file that
/// happens to carry the name.
fn class_references(root: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let mut found = Classes {
        blocks: namespace_blocks(root, src),
        bound: bound_names(root, src),
        seen: std::collections::HashSet::new(),
        out: Vec::new(),
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // A `use` is read as a whole above, and reading it again would
        // double every one in the file.
        if matches!(
            node.kind(),
            "namespace_use_declaration" | "namespace_definition"
        ) {
            continue;
        }
        if matches!(node.kind(), "qualified_name" | "relative_name") {
            found.take(node, src);
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        let every = CLASS_SITES.contains(&node.kind());
        for site in names_a_class(node, src, &children) {
            found.take(site, src);
        }
        for child in children {
            if every {
                found.take(child, src);
            }
            stack.push(child);
        }
    }
    found.out
}

/// The class names one node holds, where its own shape says which child
/// is one: the scope of a `::` access, the right of `instanceof`, and
/// the first child of `Foo::class` or `Foo::CONST`.
fn names_a_class<'t>(node: Node<'t>, src: &[u8], children: &[Node<'t>]) -> Vec<Node<'t>> {
    let kind = node.kind();
    let scoped = matches!(
        kind,
        "scoped_call_expression" | "scoped_property_access_expression"
    );
    let instance_of = kind == "binary_expression"
        && super::field_text_is(node, "operator", src) == Some("instanceof");
    [
        scoped.then(|| node.child_by_field_name("scope")).flatten(),
        instance_of
            .then(|| node.child_by_field_name("right"))
            .flatten(),
        (kind == "class_constant_access_expression")
            .then(|| children.first().copied())
            .flatten(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The class names read so far, with what it takes to judge the next
/// one: the file's own namespace, the short names its `use` statements
/// already bound, and the names already recorded.
struct Classes<'a> {
    blocks: Vec<(usize, Vec<&'a str>)>,
    bound: std::collections::HashSet<&'a str>,
    seen: std::collections::HashSet<Box<str>>,
    out: Vec<super::ImportInfo>,
}

/// `self`, `static` and `parent` are the declaring class under another
/// spelling, and reach no other file.
const OWN_CLASS: &[&str] = &["self", "static", "parent", "class"];

impl<'a> Classes<'a> {
    /// Record the class this node names, if it names one this file has
    /// not already accounted for.
    fn take(&mut self, node: Node, src: &'a [u8]) {
        if !matches!(node.kind(), "name" | "qualified_name" | "relative_name") {
            return;
        }
        let Ok(text) = node.utf8_text(src) else {
            return;
        };
        if OWN_CLASS.contains(&text) || self.bound.contains(text) || !self.seen.insert(text.into())
        {
            return;
        }
        // A name the code USES states no dependency — PHP-Parser writes
        // `Comment\Doc` inline and imports nothing — so it supplies an
        // edge and never a tally entry. Only the `use` statements above
        // are dependencies the file declares.
        //
        // PHP reads an unqualified name against the CURRENT namespace,
        // and that is where most of these live. monolog's
        // Handler/RotatingFileHandler.php:29 says
        // `class RotatingFileHandler extends StreamHandler` and needs
        // no `use` for it, because both sit in `Monolog\Handler`. A
        // leading `\` is the one spelling that means the global
        // namespace instead, and `qualified` has already dropped it, so
        // the test is made on the text as written.
        let rooted = text.starts_with('\\');
        let own = read_against(&self.blocks, node.start_byte());
        let relative: String = own
            .iter()
            .copied()
            .chain(std::iter::once(text))
            .collect::<Vec<&str>>()
            .join("\\");
        let read_as = if rooted || own.is_empty() {
            text
        } else {
            &relative
        };
        self.out.push(super::ImportInfo {
            reach: super::Reach::Mention,
            ..qualified(read_as, own)
        });
    }
}

/// The short names the file's own `use` statements bind: the alias when
/// one is written, the last segment otherwise.
fn bound_names<'a>(root: Node, src: &'a [u8]) -> std::collections::HashSet<&'a str> {
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "namespace_use_clause" {
            let alias = node
                .child_by_field_name("alias")
                .and_then(|a| a.utf8_text(src).ok());
            let mut inner = node.walk();
            let written = node
                .named_children(&mut inner)
                .find(|c| matches!(c.kind(), "qualified_name" | "name"))
                .and_then(|n| n.utf8_text(src).ok())
                .map(|t| t.rsplit('\\').next().unwrap_or(t));
            names.extend(alias.or(written));
            continue;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            // A `use` is a top-level or class-body statement; nothing
            // below an expression holds one.
            if !matches!(child.kind(), "name" | "qualified_name" | "comment") {
                stack.push(child);
            }
        }
    }
    names
}

/// `use Foo\Bar;`, `use Foo\Bar as Baz;`, and the grouped
/// `use Foo\{Bar, Baz};` whose clauses hang off a `namespace_use_group`
/// and are invisible to a search of the declaration's own children.
fn namespace_uses(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let blocks = namespace_blocks(node, src);
    let own = read_against(&blocks, node.start_byte()).to_vec();
    let mut cursor = node.walk();
    let kids: Vec<Node> = node.named_children(&mut cursor).collect();
    let group = kids.iter().find(|c| c.kind() == "namespace_use_group");
    let prefix = group
        .and(
            kids.iter()
                .find(|c| matches!(c.kind(), "qualified_name" | "name")),
        )
        .and_then(|p| p.utf8_text(src).ok())
        .unwrap_or("");
    let clauses: Vec<Node> = match group {
        Some(g) => {
            let mut inner = g.walk();
            g.named_children(&mut inner).collect()
        }
        None => kids,
    };
    clauses
        .iter()
        .filter(|c| c.kind() == "namespace_use_clause")
        .filter_map(|clause| {
            let mut inner = clause.walk();
            let name = clause
                .named_children(&mut inner)
                .find(|c| matches!(c.kind(), "qualified_name" | "name"))?;
            let text = name.utf8_text(src).ok()?;
            let joined = format!("{prefix}\\{text}");
            Some(qualified(
                if prefix.is_empty() { text } else { &joined },
                &own,
            ))
        })
        .collect()
}

/// One dependency, under the FULL name PHP reads it as.
///
/// PSR-4 maps a namespace ROOT onto a source directory and the two need
/// not be spelled alike — composer.json says `GuzzleHttp\` is `src/`
/// and `League\Flysystem\` is `src` — so a name could be cut down to
/// the part the importer's own namespace does not share and left to the
/// resolver to suffix-match. That works until two projects share a leaf
/// name, and then it takes the WRONG one:
/// composer/src/Composer/Util/Url.php:15 writes `use Composer\Config;`,
/// one of 46 files that do, and the stripped tail `Config` matches
/// flysystem/src/Config.php in an unrelated repository, leaving
/// composer's own Config.php depended on by nothing. Utils.php,
/// StreamHandler.php and ErrorHandler.php collide the same way.
///
/// Keeping the whole name costs nothing and lets `Index::php` resolve
/// it the way PHP does: through the PSR-4 root a manifest declares.
///
/// The reach is still decided by the shared root, because that is the
/// only thing the SOURCE says about whether the name is this project's:
/// a name sharing the importer's namespace root is here or it is a
/// miss, and one sharing nothing is a package.
fn qualified(text: &str, own: &[&str]) -> super::ImportInfo {
    let segs: Vec<&str> = text
        .trim_start_matches('\\')
        .split('\\')
        .filter(|s| !s.is_empty())
        .collect();
    let shared = own.first().is_some_and(|root| segs.first() == Some(root));
    super::ImportInfo {
        target: segs.join("\\").into(),
        names: Vec::new(),
        reach: match shared {
            false => super::Reach::Anywhere,
            true => super::Reach::Project,
        },
    }
}

/// The namespace blocks the file declares, by the byte at which each
/// opens: the only statement in the source about where PSR-4 has
/// rooted it.
///
/// A file may declare more than one. monolog's
/// tests/Monolog/Processor/IntrospectionProcessorTest.php opens
/// `namespace Acme;` at line 12 and `namespace Monolog\Processor;` at
/// line 27, and reading the first for the whole file makes every name
/// below line 27 an `Acme\` name. Eight files in gold do this, two of
/// them composer production code.
fn namespace_blocks<'a>(node: Node, src: &'a [u8]) -> Vec<(usize, Vec<&'a str>)> {
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|c| c.kind() == "namespace_definition")
        .map(|n| {
            let segs = n
                .child_by_field_name("name")
                .and_then(|x| x.utf8_text(src).ok())
                .map(|t| t.split('\\').filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            (n.start_byte(), segs)
        })
        .collect()
}

/// The namespace a name at this byte is read against: the last block
/// opened above it.
fn read_against<'a, 'b>(blocks: &'b [(usize, Vec<&'a str>)], at: usize) -> &'b [&'a str] {
    blocks
        .iter()
        .rev()
        .find(|(start, _)| *start <= at)
        .map_or(&[][..], |(_, segs)| segs.as_slice())
}

/// A parameter carries a declared type or it does not, and that bit
/// dates a PHP codebase.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if !matches!(
        node.kind(),
        "simple_parameter" | "property_promotion_parameter" | "variadic_parameter"
    ) {
        return None;
    }
    let name = node.child_by_field_name("name")?.utf8_text(src).ok()?;
    let ty = node.child_by_field_name("type");
    let type_text = ty.and_then(|t| t.utf8_text(src).ok()).unwrap_or("");
    Some(ParamInfo {
        name: name.trim_start_matches('$').into(),
        typed: ty.is_some(),
        loose: LOOSE.contains(&type_text.trim_start_matches('?')),
        optional: node.child_by_field_name("default_value").is_some()
            || node.kind() == "variadic_parameter",
        splat: node.kind() == "variadic_parameter",
        boolish: type_text.contains("bool"),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit("::").next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("name")
        .or_else(|| call.child_by_field_name("function"))?
        .utf8_text(src)
        .ok()
}

/// `eval`, variable functions and variable variables: after any of
/// them the text stops predicting which code runs.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    if node.kind() == "variable_variable_expression" {
        return true;
    }
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "eval"
                    | "extract"
                    | "compact"
                    | "call_user_func"
                    | "call_user_func_array"
                    | "create_function"
                    | "assert"
                    | "__call"
                    | "__get"
                    | "__set"
            )
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "unary_op_expression" && text.starts_with('!')).then(|| node.named_child(0))?
}

/// `catch (\Throwable $e)` is the widest net the language has, and
/// `\Exception` is the one below it.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "catch_clause" {
        return None;
    }
    // Emptiness is the wider sin and is asked first. It is answered
    // here rather than through `swallows_error`, which the core
    // consults on `if` nodes for the languages where an error is a
    // value: a catch clause never reaches it.
    if swallows_error(node, src) {
        return Some(super::CatchSin::Swallowed);
    }
    let types = node.child_by_field_name("type")?;
    let text = types.utf8_text(src).ok()?;
    let broad = text.contains("Throwable") || text.trim_matches('\\') == "Exception";
    broad.then_some(super::CatchSin::Broad)
}

/// `die()` and `exit()` stop the process where an error belonged: no
/// caller gets a chance to answer, and nothing above sees why.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("die" | "exit"))
}

/// A catch that binds the error, throws a NEW one, and never mentions
/// the original: the chain that explains WHY is gone, and `getPrevious`
/// has nothing to return.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    let Some(bound) = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return false;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    super::rethrows_without_cause(body, "throw_expression", bound.trim_start_matches('$'), src)
}

/// A catch whose body is empty silences the error.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "catch_clause" {
        return false;
    }
    node.child_by_field_name("body")
        .and_then(|b| b.utf8_text(src).ok())
        .is_some_and(|t| t.trim().trim_matches(['{', '}']).trim().is_empty())
}

/// PHPUnit: a test is a method whose name begins with `test`. The
/// `#[Test]` attribute and the `@test` annotation are not read.
fn declares_test(node: Node, src: &[u8]) -> bool {
    if node.kind() != "method_declaration" {
        return false;
    }
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| n.starts_with("test"))
}

fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(node, src),
        Some("markTestSkipped" | "markTestIncomplete")
    )
}

/// A declared return type of `array` says a list comes back but not how
/// wide. Only a tuple-shaped docblock could answer that, and a docblock
/// is an annotation rather than the language, so there is nothing here
/// to read.
fn return_arity(_node: Node, _src: &[u8]) -> u16 {
    0
}

/// An interface's width is its declared method count: the one place
/// here where the contract is written down separately from any
/// implementation, which is the thing the metric measures.
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

/// `private` and `protected` are real here, unlike in most of the
/// scripting languages this pack sits beside.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    let modifier = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "visibility_modifier")
        .and_then(|m| m.utf8_text(src).ok());
    !matches!(modifier, Some("private" | "protected"))
}

fn is_doc(node: Node) -> bool {
    node.kind() == "comment"
}

/// The `/** ... */` docblock immediately above the declaration.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &["/**"], src)
}

/// `&&`/`||`/`and`/`or` share the binary kind with arithmetic and every
/// comparison; only those four sequence a condition. `??` is a default,
/// not a branch on truth, and is deliberately excluded.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||" | "and" | "or" | "xor") => Sem::BoolOp,
            _ => Sem::None,
        },
        _ => sem,
    }
}

/// An array literal with string keys is PHP's record, and it declares
/// no shape: the idiom the language spent a decade replacing with typed
/// properties.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "array_creation_expression" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "array_element_initializer")
        .filter_map(|e| e.named_child(0))
        .filter(|k| matches!(k.kind(), "string" | "encapsed_string"))
        .filter_map(|k| k.utf8_text(src).ok())
        .map(|k| k.trim_matches(['"', '\'']).into())
        .collect();
    (!keys.is_empty()).then_some(keys)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn targets(src: &str) -> Vec<String> {
        let pack = super::Lang::Php.pack();
        let mut parser = pack.make_parser();
        let facts =
            crate::facts::extract(pack, &mut parser, Path::new("lib/PhpParser/Lexer.php"), src);
        facts.imports.iter().map(|i| i.target.to_string()).collect()
    }

    #[test]
    fn an_unqualified_class_carries_the_namespace_it_is_read_against() {
        // monolog's Handler/RotatingFileHandler.php:29 writes
        // `class RotatingFileHandler extends StreamHandler` and needs
        // no `use`, because PHP reads an unqualified name against the
        // current namespace. A leading `\\` is the one spelling that
        // means the global namespace instead.
        let pack = super::Lang::Php.pack();
        let mut parser = pack.make_parser();
        let facts = crate::facts::extract(
            pack,
            &mut parser,
            Path::new("src/Monolog/Handler/RotatingFileHandler.php"),
            "<?php\nnamespace Monolog\\Handler;\n\
             use Monolog\\Level;\n\
             class RotatingFileHandler extends StreamHandler {\n\
               public function f(\\Closure $c, Level $l): void {}\n\
             }\n",
        );
        let got: Vec<String> = facts.imports.iter().map(|i| i.target.to_string()).collect();
        assert!(
            got.iter().any(|t| t == "Monolog\\Handler\\StreamHandler"),
            "{got:?}"
        );
        // A `use` keeps its full name, so it cannot collide with a leaf
        // of the same name in another repository.
        assert!(got.iter().any(|t| t == "Monolog\\Level"), "{got:?}");
        // The global namespace stays global.
        assert!(got.iter().any(|t| t == "Closure"), "{got:?}");
        assert!(
            !got.iter().any(|t| t == "Monolog\\Handler\\Closure"),
            "{got:?}"
        );
    }

    #[test]
    fn a_php_suite_directory_is_named_freely() {
        // flysystem writes `test_files/`, PHP-Parser `test_old/`, and
        // monolog ships its harness base class as `src/Monolog/Test/`.
        // An exact `/test/` or `/tests/` match sees none of them.
        let is_test = super::pack().test_path;
        for p in [
            "flysystem/test_files/adapter.php",
            "PHP-Parser/test_old/run.php",
            "monolog/src/Monolog/Test/TestCase.php",
            "flysystem/src/AdapterTestUtilities/Adapter.php",
            "composer/tests/Composer/Test/AllFunctionalTest.php",
        ] {
            assert!(is_test(p), "{p}");
        }
        for p in [
            "composer/src/Composer/Config.php",
            "flysystem/src/Filesystem.php",
        ] {
            assert!(!is_test(p), "{p}");
        }
    }

    #[test]
    fn a_dir_relative_include_names_a_file_and_a_variable_one_names_nothing() {
        // PHP-Parser's Lexer.php:5 and flysystem's phpunit.php:4-5 are
        // production includes of files nothing else reaches. Without all
        // four node kinds in KINDS the pack reads none of the corpus's
        // 66 include statements.
        let got = targets(
            "<?php\n\
             require __DIR__ . '/compatibility_tokens.php';\n\
             include_once __DIR__.'/../src/bootstrap.php';\n\
             require_once $base . '/runtime.php';\n\
             include 'plain.php';\n",
        );
        assert!(
            got.iter().any(|t| t == "/compatibility_tokens.php"),
            "{got:?}"
        );
        assert!(got.iter().any(|t| t == "/../src/bootstrap.php"), "{got:?}");
        // Without `__DIR__` the base is the process's working directory
        // or a variable, and neither is readable from the source.
        assert!(!got.iter().any(|t| t.contains("runtime.php")), "{got:?}");
        assert!(!got.iter().any(|t| t.contains("plain.php")), "{got:?}");
    }
}
