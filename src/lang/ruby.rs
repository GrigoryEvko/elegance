//! Ruby: everything is an object and almost everything is a method call.
//!
//! Two facts shape what can be measured here. Blocks are the control
//! flow — `each`, `map`, `times` are calls taking a block, not loop
//! syntax — so a loop-based metric reads low the same way it does for
//! OCaml, and the block is measured as the lambda it is. And privacy is
//! a run-time toggle: `private` is a method call that changes the
//! default for everything after it, so visibility has to be tracked by
//! position within the class body rather than read off a keyword.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("method", Sem::FnDef),
    ("singleton_method", Sem::FnDef),
    ("class", Sem::TypeDef),
    ("singleton_class", Sem::TypeDef),
    ("module", Sem::TypeDef),
    ("block", Sem::Lambda),
    ("do_block", Sem::Lambda),
    ("lambda", Sem::Lambda),
    ("if", Sem::If),
    ("elsif", Sem::ElseIf),
    ("else", Sem::Else),
    ("unless", Sem::If),
    // Modifier forms: `x if y` is a branch wherever it is written.
    ("if_modifier", Sem::If),
    ("unless_modifier", Sem::If),
    ("while_modifier", Sem::Loop),
    ("until_modifier", Sem::Loop),
    ("conditional", Sem::Ternary),
    ("while", Sem::Loop),
    ("until", Sem::Loop),
    ("for", Sem::Loop),
    ("case", Sem::Match),
    ("case_match", Sem::Match),
    ("when", Sem::CaseArm),
    ("in_clause", Sem::CaseArm),
    ("begin", Sem::Try),
    ("rescue", Sem::Catch),
    ("rescue_modifier", Sem::Catch),
    ("ensure", Sem::With),
    ("binary", Sem::BoolOp),
    ("call", Sem::Call),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("constant", Sem::Ident),
    ("instance_variable", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("simple_symbol", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
    ("next", Sem::Jump),
    ("break", Sem::Jump),
    ("redo", Sem::Jump),
    ("retry", Sem::Jump),
    ("return", Sem::Jump),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("method", "name"),
    ("singleton_method", "name"),
    ("class", "name"),
    ("module", "name"),
    // A local is BORN at its first assignment — the language has no
    // declaration keyword. Without this the live map held no definition
    // row for any Ruby local, so every span read zero and the
    // repurposing check had nothing to compare a rewrite against.
    ("assignment", "left"),
];

/// `x = ...` again; `x += ...` is `operator_assignment` and stays exempt
/// as a collecting update.
const REASSIGNS: &[(&str, &str)] = &[("assignment", "left")];
const ATTR: (&str, &str) = ("call", "receiver");

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Ruby,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: Some(ATTR),
        sems,
        def_sites,
        reassigns,
        attr,
        scope_sep: "#",
        return_type_field: "",
        bool_op_field: "operator",
        call_target_fields: &["method"],
        types_declared: false,
        record_keys,
        // `File.open` with a block closes at the end of it, and that is
        // the idiom; the bare form is rare enough not to guess at.
        // Concurrency is Thread, Fiber and gems — never syntax.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["##", "#"],
        is_public,
        doc_span,
        docs_inside_body: true,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin,
        swallows_error,
        loses_context,
        panicky,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/spec/") || p.contains("/test/") || p.ends_with("_spec.rb"),
        asserty,
        is_hook: |_, _| false,
        // A method returns its last expression and declares nothing, so
        // returning three values is a bare array with no width to read.
        return_arity: |_, _| 0,
        // A module is a mixin, not a contract: it carries the method
        // bodies with it, so its width is an implementation's size
        // rather than the surface an implementor must satisfy.
        interfaces: |_, _| Vec::new(),
        skips_test,
        magic_exempt: &["hash"],
        assign_kinds: &["assignment", "operator_assignment"],
    }
}

fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name")
}

/// `require`, `require_relative`, `load` and `autoload` are the import
/// forms. `autoload` names the constant first and the file second, so
/// the target is the first STRING argument rather than the first.
///
/// A `"#{__dir__}/..."` literal is an import form too, whatever call it
/// is written in: `__dir__` is the directory of the file holding it, so
/// the literal states `require_relative`'s argument the long way. Ruby
/// reaches for it wherever a load must not depend on `$LOAD_PATH`, and
/// the load is then commonly wrapped in a project's own verb. rubocop
/// writes 609 of them as `register_cop :Alias, "#{__dir__}/style/alias"`
/// and 86 as `autoload :Alignment, "#{__dir__}/mixin/alignment"`; those
/// 609 cop files are 55% of every orphan in the Ruby corpus.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    // An argument list is an unmapped kind, so reading one leaves the
    // call around it a call — which matters here, because the verb is
    // whatever the project called it.
    if node.kind() == "argument_list" {
        return autoloaded(node, src)
            .map(|target| super::ImportInfo {
                target: target.into(),
                names: Vec::new(),
                reach: super::Reach::Mention,
            })
            .into_iter()
            .collect();
    }
    let Some(string) = requires(node, src).then(|| first_string(node)).flatten() else {
        return Vec::new();
    };
    if let Some(path) = dir_rooted_path(string, src) {
        return vec![super::ImportInfo {
            target: path.into(),
            reach: super::Reach::Project,
            names: Vec::new(),
        }];
    }
    if !names_a_load(node, src) {
        return Vec::new();
    }
    let Some(target) = load_target(string, src) else {
        return Vec::new();
    };
    // `require_relative` is a path from this file and must land on
    // one. `require` searches the load path, where a gem is an
    // ordinary answer.
    let reach = match callee_text(node, src) {
        Some("require_relative") => super::Reach::Project,
        _ => super::Reach::Anywhere,
    };
    let widened = widened_target(string, src)
        .filter(|w| *w != target)
        .map(|target| super::ImportInfo {
            target: target.into(),
            // The widening is read off an assignment rather than
            // stated by the load, so it names modules without
            // claiming to be a second dependency.
            reach: super::Reach::Mention,
            names: Vec::new(),
        });
    std::iter::once(super::ImportInfo {
        target: target.into(),
        reach,
        names: Vec::new(),
    })
    .chain(widened)
    .collect()
}

/// The file an autoload-shaped call names, whatever the verb is.
///
/// `sinatra/sinatra-contrib/lib/sinatra/contrib/setup.rb:14` defines
/// `register(name, path)` as `autoload(name, path, :register)` and
/// `helpers` beside it identically; `contrib.rb:12-32` names all eleven
/// extensions through them — `register :ConfigFile, 'sinatra/config_file'`
/// — and nothing else in the corpus names any of the ten files they
/// reach. The verb is the project's, so the ARGUMENTS have to be what
/// is read: a CONSTANT symbol and a bare path.
///
/// Both narrowings were measured. 1077 lines across gold pair a
/// receiverless verb with a symbol and a string; requiring the symbol
/// to name a CONSTANT leaves 785, which is what keeps `mime_type :foo,
/// 'application/x-foo'`, `set :views, 'app/views'` and `column :name,
/// "text"` out. Requiring the string to be stated OUTRIGHT leaves
/// rubocop's 609 `register_cop :Alias, "#{__dir__}/style/alias"` to the
/// `__dir__` reading above, which already resolves them as paths.
fn autoloaded(args: Node, src: &[u8]) -> Option<String> {
    let call = args.parent().filter(|c| c.kind() == "call")?;
    // A receiver makes it somebody's own method, and `autoload` itself
    // already states its dependency through the load path above.
    if call.child_by_field_name("receiver").is_some() || names_a_load(call, src) {
        return None;
    }
    let mut cursor = args.walk();
    let given: Vec<Node> = args.named_children(&mut cursor).collect();
    let constant = |n: &&Node| {
        n.kind() == "simple_symbol"
            && n.utf8_text(src)
                .ok()
                .and_then(|t| t.strip_prefix(':'))
                .is_some_and(|t| t.starts_with(char::is_uppercase))
    };
    given.iter().find(constant)?;
    let path = given.iter().find(|n| n.kind() == "string")?;
    let text = plain_text(*path, src)?;
    text.contains('/').then_some(text)
}

/// A string with nothing built into it — the only kind whose text the
/// source states outright.
fn plain_text(string: Node, src: &[u8]) -> Option<String> {
    let mut cursor = string.walk();
    let parts: Vec<Node> = string.named_children(&mut cursor).collect();
    match parts.as_slice() {
        [only] if only.kind() == "string_content" => Some(only.utf8_text(src).ok()?.to_string()),
        _ => None,
    }
}

/// The same path, with a hole widened by the assignment behind it.
///
/// `sequel/lib/sequel/database/connecting.rb:84` is
/// `require "sequel/adapters/#{file}"` and `:76`, six lines above it,
/// is `file = "#{subdir}/#{scheme}"` — so the hole is TWO components
/// wide, and the doc at `:70` says why: ":subdir :: The subdirectory of
/// sequel/adapters to look in, only to be used for loading
/// subadapters". Reading the hole as one component reached none of the
/// eleven `adapters/jdbc/*.rb` or three `adapters/odbc/*.rb`.
///
/// Bounded by an assignment in the file rather than by an unbounded
/// trailing star, which was the alternative and is a guess:
/// `glob_modules` is shared with Lua and Python, where `kong.plugins.*`
/// would then match thousands of files. The `else file = scheme` branch
/// binds a name rather than a string and is skipped, so a hole nothing
/// assigns keeps its single star.
fn widened_target(string: Node, src: &[u8]) -> Option<String> {
    let mut cursor = string.walk();
    let parts: Vec<Node> = string.named_children(&mut cursor).collect();
    let mut out = String::new();
    for (n, part) in parts.iter().enumerate() {
        match part.kind() {
            "string_content" => out.push_str(part.utf8_text(src).ok()?),
            "interpolation" if n > 0 => out.push_str(&hole(*part, src)),
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// What one hole stands for: the shape of the string a name is bound
/// to, where the file binds one, and otherwise a single component.
fn hole(interpolation: Node, src: &[u8]) -> String {
    let star = String::from("*");
    let Some(name) = interpolation
        .named_child(0)
        .filter(|n| n.kind() == "identifier")
        .and_then(|n| n.utf8_text(src).ok())
    else {
        return star;
    };
    let Some(bound) = assigned_string(interpolation, name, src) else {
        return star;
    };
    let mut cursor = bound.walk();
    let mut out = String::new();
    for part in bound.named_children(&mut cursor) {
        match part.kind() {
            "string_content" => out.push_str(part.utf8_text(src).unwrap_or("")),
            "interpolation" => out.push('*'),
            _ => return star,
        }
    }
    out
}

/// The first string literal this file assigns to `name`.
fn assigned_string<'t>(from: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    let mut root = from;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "assignment"
            && node
                .child_by_field_name("left")
                .and_then(|l| l.utf8_text(src).ok())
                == Some(name)
            && let Some(value) = node
                .child_by_field_name("right")
                .filter(|v| v.kind() == "string")
        {
            return Some(value);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

/// The path a `"#{__dir__}/x/y"` literal names, relative to the file
/// holding it — here `/x/y`, which `normalize` folds onto the file's own
/// directory exactly as it folds a `require_relative` argument.
///
/// Two narrowings keep this to loads. A FURTHER interpolation makes the
/// rest of the path a run-time value, and there is nothing to read.
/// And an extension other than `.rb` names data rather than a module:
/// rubocop's `File.exist?("#{__dir__}/../rubocop.gemspec")` is the
/// corpus's one such literal, and it is already excluded by the
/// receiver its call carries.
fn dir_rooted_path(string: Node, src: &[u8]) -> Option<String> {
    let mut cursor = string.walk();
    let mut parts = string.named_children(&mut cursor);
    let head = parts.next()?;
    if head.kind() != "interpolation" || head.named_child(0)?.utf8_text(src).ok()? != "__dir__" {
        return None;
    }
    let mut path = String::new();
    for part in parts {
        if part.kind() != "string_content" {
            return None;
        }
        path.push_str(part.utf8_text(src).ok()?);
    }
    let leaf = path.rsplit('/').next().unwrap_or("");
    let loadable = match leaf.rsplit_once('.') {
        Some((_, ext)) => ext == "rb",
        None => !leaf.is_empty(),
    };
    loadable.then_some(path)
}

/// Ruby's parameter kinds are a taxonomy in themselves: required,
/// optional, keyword, splat and block. Keyword arguments are named at
/// the call site and are the opposite of opaque; `**opts` is the one
/// that hides what it accepts.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    // A bare identifier is a parameter only where a parameter list is
    // what encloses it; elsewhere it is any other name in the file.
    let positional = node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|p| p.kind() == "method_parameters");
    let (optional, kw_splat) = match node.kind() {
        _ if positional => (false, false),
        "optional_parameter" | "splat_parameter" | "block_parameter" => (true, false),
        // A keyword parameter with a default is optional; without one
        // it is required, and still named at every call site.
        "keyword_parameter" => (node.child_by_field_name("value").is_some(), false),
        // `**opts` is the one that hides what it accepts.
        "hash_splat_parameter" => (true, true),
        // `def each((key, value))` — the grammar has one node for it,
        // in a method head and in a block's `|(k, v)|` alike.
        "destructured_parameter" => (false, false),
        _ => return None,
    };
    let holder = node.child_by_field_name("name").unwrap_or(node);
    let text = holder.utf8_text(src).ok()?;
    Some(ParamInfo {
        name: text.trim_matches(':').into(),
        optional,
        kw_splat,
        typed: false,
        destructured: node.kind() == "destructured_parameter",
        splat: matches!(node.kind(), "splat_parameter" | "hash_splat_parameter"),
        // Nothing declares a type here, so the DEFAULT is the only
        // evidence a parameter is a switch — `def write(data,
        // dry_run: false)` says as plainly as an annotation would.
        boolish: defaults_to_a_boolean(node, src),
        ..Default::default()
    })
}

/// Does this parameter default to `true` or `false`?
fn defaults_to_a_boolean(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("value")
        .and_then(|v| v.utf8_text(src).ok())
        .is_some_and(|t| matches!(t.trim(), "true" | "false"))
}

/// Direct recursion, and `self.name` which is the same call written out.
fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('#').next().unwrap_or(unit_name);
    callee_text(call, src) == Some(bare)
}

fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("method")?.utf8_text(src).ok()
}

fn first_string<'t>(call: Node<'t>) -> Option<Node<'t>> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    args.named_children(&mut cursor)
        .find(|c| c.kind() == "string")
}

/// The path a load's string names, with `*` where the source stops
/// being able to say.
///
/// `require "roda/plugins/#{name}"` assembles its name at run time, and
/// what it builds cannot be read — but everything around it can, so
/// `roda/plugins/*` is the honest reading, exactly as a Lua `..` prefix
/// is. Six such lines hold 417 of gold Ruby's 490 orphans.
///
/// Every literal must meet the run-time value at a `/`, or the `*`
/// would stand for part of a name rather than a whole one. That is
/// also what rejects the shapes with nothing to read: sinatra's
/// `require "#{engine}"` and sequel's `"#{"#{subdir}/" if subdir}#{f}"`
/// put the very ROOT of the path at run time, and reading past those
/// reported a dependency literally called `/schema`.
fn load_target(string: Node, src: &[u8]) -> Option<String> {
    let mut cursor = string.walk();
    let parts: Vec<Node> = string.named_children(&mut cursor).collect();
    let mut out = String::new();
    for (n, part) in parts.iter().enumerate() {
        match part.kind() {
            "string_content" => {
                let text = part.utf8_text(src).ok()?;
                let meets = (n == 0 || text.starts_with('/'))
                    && (n + 1 == parts.len() || text.ends_with('/'));
                if !meets {
                    return None;
                }
                out.push_str(text);
            }
            "interpolation" if n > 0 => out.push('*'),
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `raise` is the ordinary error path and is not a panic; `exit!` and
/// `abort` end the process without unwinding.
fn panicky(call: Node, src: &[u8]) -> bool {
    matches!(callee_text(call, src), Some("abort" | "exit!"))
}

/// The metaprogramming that makes a name unfindable: text evaluated at
/// run time, methods answered by `method_missing`, constants and sends
/// assembled from strings.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some(
                "eval"
                    | "instance_eval"
                    | "class_eval"
                    | "module_eval"
                    | "define_method"
                    | "method_missing"
                    | "const_get"
                    | "const_set"
                    | "instance_variable_get"
                    | "instance_variable_set"
                    | "send"
                    | "__send__"
                    | "public_send"
                    | "binding"
            )
        )
}

fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    let negated = node.kind() == "unary" && (text.starts_with('!') || text.starts_with("not "));
    negated.then(|| node.child_by_field_name("operand"))?
}

/// `rescue => e` with no class catches StandardError, and bare `rescue`
/// with no class at all is the same reach with less written down.
///
/// Emptiness is asked FIRST, and answered here rather than through
/// `swallows_error` — that hook is consulted on `if` nodes, for the
/// languages where an error is a value, and Ruby's rescue never
/// reaches it. The wider sin is the one that vanishes the error.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    if node.kind() != "rescue" {
        return None;
    }
    if swallows_error(node, src) {
        return Some(super::CatchSin::Swallowed);
    }
    let has_class = node.child_by_field_name("exceptions").is_some();
    let text = node.utf8_text(src).unwrap_or("");
    // `rescue Exception` reaches past StandardError to SignalException
    // and NoMemoryError — the widest net the language offers.
    match has_class {
        false => Some(super::CatchSin::Broad),
        true if text.contains("rescue Exception") => Some(super::CatchSin::Broad),
        true => None,
    }
}

/// A rescue whose body is empty, or is only `nil`, swallows the error.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    if node.kind() != "rescue" {
        return false;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return true;
    };
    let text = body.utf8_text(src).unwrap_or("").trim();
    text.is_empty() || text == "nil"
}

/// A rescue that binds the error, raises a NEW one, and never mentions
/// the original. `raise` with no arguments re-raises `$!` and keeps
/// everything; `raise Wrapped, "...: #{e.message}"` mentions the
/// binding and keeps the cause; only the third form loses it.
fn loses_context(node: Node, src: &[u8]) -> bool {
    if node.kind() != "rescue" {
        return false;
    }
    let Some(bound) = node
        .child_by_field_name("variable")
        .and_then(|v| v.utf8_text(src).ok())
        .map(|t| t.trim_start_matches("=>").trim())
    else {
        return false;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut stack = vec![body];
    let mut rethrows = false;
    while let Some(n) = stack.pop() {
        if n.kind() == "call" && callee_text(n, src) == Some("raise") {
            let Some(args) = n.child_by_field_name("arguments") else {
                return false;
            };
            if args.utf8_text(src).is_ok_and(|t| super::mentions(t, bound)) {
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

/// RSpec and minitest between them cover the corpus: `it`, `describe`
/// and `context` take a name and a block, and a minitest method is any
/// method whose name begins `test_`.
fn declares_test(node: Node, src: &[u8]) -> bool {
    match node.kind() {
        "method" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .is_some_and(|n| n.starts_with("test_")),
        "call" => {
            matches!(
                callee_text(node, src),
                Some("it" | "describe" | "context" | "specify" | "scenario" | "feature")
            ) && node.child_by_field_name("block").is_some()
        }
        _ => false,
    }
}

/// `xit`/`xdescribe` and `skip` are the standard disablings.
fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(
        callee_text(node, src),
        Some("xit" | "xdescribe" | "xcontext" | "skip" | "pending")
    )
}

/// RSpec's `expect(...)`, minitest's `assert_*`, and the plain `assert`.
fn asserty(call: Node, src: &[u8]) -> bool {
    callee_text(call, src).is_some_and(|t| super::assertish(t) || t == "expect" || t == "should")
}

/// Visibility is positional: `private` with no argument switches the
/// default for every method after it in the class body. Reading it
/// needs a walk back through the siblings, since nothing on the method
/// itself records what was in force when it was defined.
fn is_public(node: Node, src: &[u8]) -> bool {
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        if p.kind() == "call" || p.kind() == "identifier" {
            match p.utf8_text(src).map(str::trim) {
                Ok("private" | "protected") => return false,
                Ok("public") => return true,
                _ => {}
            }
        }
        prev = p.prev_named_sibling();
    }
    // A leading underscore is the convention for "internal anyway".
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_none_or(|n| !n.starts_with('_'))
}

/// The `#` comment run immediately above the definition.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &[], src)
}

/// A hash literal with symbol or string keys is an undeclared shape,
/// the same way an object literal is in TypeScript.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "hash" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "pair")
        .filter_map(|p| p.child_by_field_name("key"))
        .filter_map(|k| k.utf8_text(src).ok())
        .map(|k| k.trim_matches([':', '"', '\'']).into())
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// Three normalizations. `elsif` is its own kind and needs no
/// flattening. `&&`/`||`/`and`/`or` share the binary kind with every
/// arithmetic operator. And a `block` attached to `each`/`map` is the
/// language's loop, so it is measured as one rather than as a lambda —
/// otherwise Ruby would appear to contain no iteration at all.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||" | "and" | "or") => Sem::BoolOp,
            _ => Sem::None,
        },
        Sem::Lambda if iterates(node, src) => Sem::Loop,
        // Ruby's import is a CALL, and the core asks about imports at
        // `Sem::Import` nodes — so until this arm existed the pack's
        // `imports` hook was written, tested and never once asked, and
        // Ruby had no module graph at all.
        Sem::Call if requires(node, src) => Sem::Import,
        // An autoload written under the project's own verb. The
        // ARGUMENT LIST is promoted and not the call, so the twenty
        // calls in gold that match the shape and load nothing go on
        // being counted as the calls they are.
        Sem::None if node.kind() == "argument_list" && autoloaded(node, src).is_some() => {
            Sem::Import
        }
        _ => sem,
    }
}

/// Is this call a load site? Either it NAMES a load — `autoload :Base,
/// 'rack/protection/base'` defers one until the constant is touched,
/// and the file is a dependency either way — or it is handed a
/// `#{__dir__}`-rooted path, which nothing but a load is written with.
///
/// A RECEIVER disqualifies both: `require` and `load` are Kernel methods
/// called bare, `config.load(path)` is somebody's own method that
/// happens to share the name, and the receiver is what separates
/// rubocop's 609 `register_cop` loads from its three `File.exist?`,
/// `Dir[]` and `$LOAD_PATH.unshift` uses of the same literal shape.
fn requires(call: Node, src: &[u8]) -> bool {
    call.child_by_field_name("receiver").is_none()
        && (names_a_load(call, src)
            || first_string(call).is_some_and(|s| dir_rooted_path(s, src).is_some()))
}

/// The four load forms, plus whatever this file has aliased to one.
///
/// `alias orig_require require` is a statement, and sequel writes it
/// before loading all 102 of its extensions through the alias. Reading
/// it is the same standard as reading a manifest; GUESSING it is not an
/// option, since a suffix rule on `require` would take `requires`,
/// `required` and `require_valid_table` — 300 calls across the gold
/// corpus that load nothing.
fn names_a_load(call: Node, src: &[u8]) -> bool {
    let Some(name) = callee_text(call, src) else {
        return false;
    };
    LOADS.contains(&name) || aliases_a_load(call, src, name)
}

const LOADS: &[&str] = &["require", "require_relative", "load", "autoload"];

/// Did this file alias `name` onto a load? The substring guard is a
/// performance one and not a correctness one: the alias statement is
/// the authority, so a name it never mentions can only ever be absent.
fn aliases_a_load(call: Node, src: &[u8], name: &str) -> bool {
    if !name.contains("require") && !name.contains("load") {
        return false;
    }
    let mut root = call;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    // The whole tree, because an alias is written wherever the method
    // it renames is in scope: sequel's sits two levels in, inside
    // `module Sequel` and its singleton class.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let text = |i| node.named_child(i).and_then(|c| c.utf8_text(src).ok());
        if node.kind() == "alias"
            && text(0) == Some(name)
            && text(1).is_some_and(|t| LOADS.contains(&t))
        {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

/// Blocks given to the iteration methods. The list is deliberately
/// short: these are the ones that mean "again for each element", and a
/// block given to `File.open` or `synchronize` genuinely is not a loop.
fn iterates(block: Node, src: &[u8]) -> bool {
    let Some(call) = block.parent().filter(|p| p.kind() == "call") else {
        return false;
    };
    matches!(
        callee_text(call, src),
        Some(
            "each"
                | "each_with_index"
                | "each_with_object"
                | "each_pair"
                | "each_key"
                | "each_value"
                | "each_slice"
                | "each_cons"
                | "each_line"
                | "each_char"
                | "map"
                | "map!"
                | "flat_map"
                | "collect"
                | "select"
                | "filter"
                | "filter_map"
                | "reject"
                | "detect"
                | "find"
                | "find_all"
                | "reduce"
                | "inject"
                | "times"
                | "upto"
                | "downto"
                | "step"
                | "sort_by"
                | "group_by"
                | "partition"
                | "count"
                | "sum"
                | "all?"
                | "any?"
                | "none?"
                | "one?"
        )
    )
}

#[cfg(test)]
mod tests {
    use crate::facts::extract;
    use crate::lang::{Lang, Reach};
    use std::path::Path;

    fn imports_of(src: &str) -> Vec<(String, Reach)> {
        let pack = Lang::Ruby.pack();
        let facts = extract(pack, &mut pack.make_parser(), Path::new("lib/dept.rb"), src);
        facts
            .imports
            .iter()
            .map(|i| (i.target.to_string(), i.reach))
            .collect()
    }

    #[test]
    fn an_autoload_shaped_call_is_a_load_whatever_the_verb() {
        // sinatra-contrib/lib/sinatra/contrib/setup.rb:14 defines
        // `register(name, path)` as `autoload(name, path, :register)`
        // and `helpers` beside it identically; contrib.rb names all
        // eleven extensions through them and nothing else names any of
        // the files they reach. Requiring a CONSTANT symbol is what
        // separates a load from `column :name, "text"`: 1077 lines in
        // gold pair a verb with a symbol and a string, and 785 of them
        // name a constant.
        let got = imports_of(concat!(
            "register :ConfigFile, 'sinatra/config_file'\n",
            "helpers :ContentFor, 'sinatra/content_for'\n",
            "mime_type :foo, 'application/x-foo'\n",
            "set :views, 'app/views'\n",
            "column :name, 'text'\n",
        ));
        assert_eq!(
            got,
            [
                ("sinatra/config_file".to_string(), Reach::Mention),
                ("sinatra/content_for".to_string(), Reach::Mention),
            ],
            "a lower-case symbol names a setting, not a constant"
        );
    }

    #[test]
    fn a_hole_is_as_wide_as_the_string_the_file_assigns_to_it() {
        // sequel/lib/sequel/database/connecting.rb:84 is the require
        // and :76, six lines above it, is the assignment; the doc at
        // :70 says ":subdir :: The subdirectory of sequel/adapters to
        // look in". Reading the hole as one component reached none of
        // the eleven jdbc or three odbc adapters. The `else file =
        // scheme` branch binds a name rather than a string and is
        // skipped, so a hole nothing assigns keeps its single star.
        let got = imports_of(concat!(
            "if subdir = opts[:subdir]\n",
            "  file = \"#{subdir}/#{scheme}\"\n",
            "else\n",
            "  file = scheme\n",
            "end\n",
            "require \"sequel/adapters/#{file}\"\n",
            "require \"roda/plugins/#{name}\"\n",
        ));
        assert_eq!(
            got,
            [
                ("sequel/adapters/*".to_string(), Reach::Anywhere),
                ("sequel/adapters/*/*".to_string(), Reach::Mention),
                ("roda/plugins/*".to_string(), Reach::Anywhere),
            ],
            "the plain reading is kept beside the widened one"
        );
    }

    #[test]
    fn a_dir_rooted_literal_is_a_path_from_this_file_whatever_call_holds_it() {
        // rubocop names 609 cop files with `register_cop` and 86 mixins
        // with `autoload`, both over a `#{__dir__}` literal. Its three
        // other uses of the same shape carry a receiver, and that is
        // what keeps a gemspec check and a glob out of the graph.
        let got = imports_of(concat!(
            "register_cop :Alias, \"#{__dir__}/style/alias\"\n",
            "autoload :Alignment, \"#{__dir__}/mixin/alignment\"\n",
            "$LOAD_PATH.unshift(\"#{__dir__}/../lib\")\n",
            "features = Dir[\"#{__dir__}/**/*.rb\"]\n",
            "warn 'x' unless File.exist?(\"#{__dir__}/../a.gemspec\")\n",
            "require_relative 'sibling'\n",
            "require 'set'\n",
        ));
        assert_eq!(
            got,
            [
                ("/style/alias".to_string(), Reach::Project),
                ("/mixin/alignment".to_string(), Reach::Project),
                ("sibling".to_string(), Reach::Project),
                ("set".to_string(), Reach::Anywhere),
            ]
        );
    }

    #[test]
    fn a_name_this_file_aliased_onto_require_is_one() {
        // sequel writes `alias orig_require require` inside
        // `module Sequel`, then loads all 102 of its extensions through
        // the alias. Reading the statement is the only honest way in: a
        // suffix rule on `require` would take `requires`, `required`
        // and `require_valid_table`, 300 calls across the gold corpus
        // that load nothing.
        let got = imports_of(concat!(
            "module Sequel\n",
            "  class << self\n",
            "    alias orig_require require\n",
            "    def extension(*es)\n",
            "      es.each{|e| orig_require(\"sequel/extensions/#{e}\")}\n",
            "      require_valid_table(\"sequel/nope\")\n",
            "    end\n",
            "  end\n",
            "end\n",
        ));
        assert_eq!(got, [("sequel/extensions/*".to_string(), Reach::Anywhere)]);
    }

    #[test]
    fn a_second_interpolation_leaves_no_path_to_read() {
        // `"#{__dir__}/#{cop_path}"` names a file chosen at run time.
        assert!(imports_of("register_cop :X, \"#{__dir__}/#{dept}/x\"\n").is_empty());
    }

    #[test]
    fn a_require_assembled_at_run_time_still_names_what_it_can() {
        // roda's `plugin` method, rodauth's feature loader and four of
        // sequel's -- six lines holding 417 of gold Ruby's 490 orphans.
        // What the interpolation builds cannot be read; everything
        // around it can, and `*` stands for exactly one component.
        //
        // Every literal has to meet the run-time value at a `/`, which
        // is what rejects the shapes with nothing to read: `"#{engine}"`
        // puts the ROOT of the path at run time, and reading past it
        // reported a dependency literally called `/schema`.
        let got = imports_of(concat!(
            "require \"roda/plugins/#{name}\"\n",
            "require_relative \"connection_pool/#{pc}\"\n",
            "require \"rubocop/#{feature}/version\"\n",
            "require \"#{adapter}/schema\"\n",
            "require \"sequel/adapters/mock\"\n",
        ));
        assert_eq!(
            got,
            [
                ("roda/plugins/*".to_string(), Reach::Anywhere),
                ("connection_pool/*".to_string(), Reach::Project),
                // rubocop's own line: `rubocop/*/version`, not the 44
                // modules `lib/rubocop` itself holds.
                ("rubocop/*/version".to_string(), Reach::Anywhere),
                ("sequel/adapters/mock".to_string(), Reach::Anywhere),
            ]
        );
    }
}
