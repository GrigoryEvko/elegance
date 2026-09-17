//! Lua: the embedded language, measured as the host language it became.
//!
//! Lua is where configuration grew into a program — nginx routing, game
//! logic, editor plugins — and that history shows in the shape of the
//! code. There are no classes, so the class metrics are structurally
//! silent rather than merely quiet: a "method" is a function stored in a
//! table and `self` is a calling convention (`a:b()`) rather than a
//! declaration. Visibility is the same story told with `local`, which is
//! the only privacy the language has.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_declaration", Sem::FnDef),
    ("function_definition", Sem::Lambda),
    ("if_statement", Sem::If),
    ("elseif_statement", Sem::ElseIf),
    ("else_statement", Sem::Else),
    ("for_statement", Sem::Loop),
    ("while_statement", Sem::Loop),
    ("repeat_statement", Sem::Loop),
    // `and`/`or` share the binary kind with arithmetic; refine keeps
    // only the two that sequence a condition.
    ("binary_expression", Sem::BoolOp),
    ("function_call", Sem::Call),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("number", Sem::NumLit),
    ("string", Sem::StrLit),
    ("true", Sem::BoolLit),
    ("false", Sem::BoolLit),
    ("break_statement", Sem::Jump),
    ("return_statement", Sem::Jump),
    // `goto` is the flat +1 the Sem::Goto variant exists for.
    ("goto_statement", Sem::Goto),
];

/// The grammar labels an assignment's target list with no field, so the
/// binding is the first named child. `local x = 1` wraps the same
/// `assignment_statement` in a `variable_declaration`, which is why one
/// entry covers both a fresh local and a rebound global.
const DEF_SITES: &[(&str, &str)] = &[
    ("function_declaration", "name"),
    ("assignment_statement", ""),
];

/// `x = ...` again. Lua has no compound assignment, so every rebinding
/// of a name that is already bound arrives through this one kind.
const REASSIGNS: &[(&str, &str)] = &[("assignment_statement", "")];

/// `a.b` is the only member access the language has; `a:b()` is a call.
const ATTR: (&str, &str) = ("dot_index_expression", "table");

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_lua::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    let attr = super::attr_site(&ts, ATTR.0, ATTR.1);
    Pack {
        lang: Lang::Lua,
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
        return_type_field: "",
        bool_op_field: "operator",
        call_target_fields: &["name"],
        types_declared: false,
        record_keys,
        // `pcall` is the error mechanism and there is no scope guard,
        // so a resource is released by the line that remembers to.
        // Coroutines are a library, not syntax.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        // LuaDoc and its LDoc successor both mark a doc block this way.
        doc_markers: &["---", "--[["],
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
        loses_context: |_, _| false,
        // Lua's `error` is how the language RAISES; it has no panic
        // distinct from raising, so `unwraps` is declared dead rather
        // than measuring how thoroughly a function validates its
        // arguments. See DECLARED_DEAD.
        panicky: |_, _| false,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| {
            p.contains("/spec/")
                || p.contains("/test/")
                || p.contains("/tests/")
                || p.ends_with("_spec.lua")
        },
        // Read the WHOLE callee, not its trailing segment: busted spells
        // every assertion `assert.same`, `assert.is_true`, `assert.falsy`,
        // and the last segment alone is `same`/`is_true`/`falsy`, none of
        // which is assertish. On the segment only a bare `assert(...)`
        // reads as an assertion, and 872 of the 893 gold Lua tests that
        // then report asserting nothing are asserting.
        asserty: |call, src| callee_qualified(call, src).is_some_and(super::assertish),
        is_hook: |_, _| false,
        // Multiple returns are the language's ordinary idiom and carry
        // no declaration, so there is no width to read without running
        // the function.
        return_arity: |_, _| 0,
        // No interface construct: a table either answers a call or it
        // does not, and nothing writes that contract down.
        interfaces: |_, _| Vec::new(),
        skips_test,
        magic_exempt: &["table_constructor"],
        assign_kinds: &["assignment_statement"],
    }
}

/// `function M.run(...)` and `local function helper(...)` both name the
/// unit; an anonymous `function()` assigned to a field does not, and is
/// measured as the lambda it is.
fn name_node(node: Node) -> Option<Node> {
    node.child_by_field_name("name").or_else(|| {
        // A promoted test takes its name from the call's first
        // argument, which is PROSE rather than an identifier.
        let args = node.parent().filter(|p| p.kind() == "arguments")?;
        args.named_child(0).filter(|n| n.kind() == "string")
    })
}

/// Lua's standard library and LuaJIT's extensions are PRELOADED:
/// `require "debug"` returns `package.loaded.debug` and consults no
/// file, so no file can be its target.
pub(crate) fn preloaded(target: &str) -> bool {
    matches!(
        target.split('.').next().unwrap_or(target),
        "string"
            | "table"
            | "math"
            | "io"
            | "os"
            | "debug"
            | "coroutine"
            | "utf8"
            | "package"
            | "bit"
            | "bit32"
            | "ffi"
            | "jit"
    )
}

/// Is this call the import form? Spelled against the WHOLE callee, not
/// its trailing component, so a `config.require(...)` of somebody's own
/// is not read as a module edge. `pcall(require, "x")` is the same
/// dependency written to tolerate its absence.
fn requires(call: Node, src: &[u8]) -> bool {
    let text = |n: Node| n.utf8_text(src).ok();
    match call.child_by_field_name("name").and_then(text) {
        Some("require") => true,
        Some("pcall" | "xpcall") => {
            let mut cursor = call.walk();
            call.child_by_field_name("arguments")
                .and_then(|a| a.named_children(&mut cursor).next())
                .and_then(text)
                == Some("require")
        }
        _ => false,
    }
}

/// `require "x"`, `require("x")` and `pcall(require, "x")` — the target
/// is the first string argument in every spelling — plus the
/// concatenation that builds a module name wherever it is written.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    if node.kind() == "table_constructor" {
        return listed_names(node, src)
            .into_iter()
            .map(|name| super::ImportInfo {
                target: name.into(),
                names: Vec::new(),
                reach: super::Reach::Mention,
            })
            .collect();
    }
    // An argument list is an unmapped kind, so reading one costs the
    // call around it nothing: `fmt` goes on being a call and `dofile`
    // goes on being spooky.
    if node.kind() == "arguments" {
        return templated(node, src)
            .map(|t| super::ImportInfo {
                target: t.into(),
                names: Vec::new(),
                reach: super::Reach::Mention,
            })
            .or_else(|| {
                loaded_file(node, src).map(|t| super::ImportInfo {
                    target: t.into(),
                    // A path from the calling script, which is what a
                    // miss has to be measured against.
                    reach: super::Reach::Project,
                    names: Vec::new(),
                })
            })
            .into_iter()
            .collect();
    }
    let built = node.kind() == "binary_expression";
    let target = match built {
        true => concatenated(node, src),
        false => requires(node, src)
            .then(|| first_string(node, src))
            .flatten(),
    };
    // A name built where no `require` is looking states no dependency:
    // the file writes a string, and whether it is a module is settled
    // by whether one exists. kong assigns
    // `"kong.plugins." .. plugin .. ".handler"` to a local and requires
    // it three lines later, so the reach follows the CALL and not the
    // concatenation's own position.
    let stated = !built || required(node, src);
    target
        .into_iter()
        .map(|target| super::ImportInfo {
            target: target.into(),
            names: Vec::new(),
            reach: match stated {
                true => super::Reach::Anywhere,
                false => super::Reach::Mention,
            },
        })
        .collect()
}

/// Is this expression the argument of a `require`?
fn required(node: Node, src: &[u8]) -> bool {
    node.parent()
        .filter(|p| p.kind() == "arguments")
        .and_then(|a| a.parent())
        .is_some_and(|call| requires(call, src))
}

/// Parameters carry no types and no defaults; `...` is the variadic
/// tail, which is opaque at the call site the way `**kwargs` is.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    match node.kind() {
        "identifier" if node.parent().is_some_and(|p| p.kind() == "parameters") => {
            Some(ParamInfo {
                name: node.utf8_text(src).ok()?.into(),
                typed: false,
                ..Default::default()
            })
        }
        // `...` is POSITIONAL varargs, the `*args` of this language.
        // Lua has no keyword arguments; the idiom is to pass a table.
        "vararg_expression" if node.parent().is_some_and(|p| p.kind() == "parameters") => {
            Some(ParamInfo {
                name: "...".into(),
                typed: false,
                splat: true,
                ..Default::default()
            })
        }
        _ => None,
    }
}

/// Direct recursion, and the `self:helper()` form that a colon call
/// desugars into.
fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    callee_text(call, src).is_some_and(|t| t == unit_name || t.ends_with(unit_name))
}

/// The callee text of a call, trailing component only: `M.run` reads as
/// `run` so a table-stored function is recognised by its own name.
fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    let text = callee_qualified(call, src)?;
    Some(text.rsplit(['.', ':']).next().unwrap_or(text))
}

/// The callee text with its qualifier intact: what a rule about the
/// NAMESPACE a call comes from has to read.
fn callee_qualified<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.child_by_field_name("name")?.utf8_text(src).ok()
}

fn first_string(call: Node, src: &[u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let arg = args
        .named_children(&mut cursor)
        .find(|c| c.kind() == "string")?;
    Some(quoted(arg, src).to_string())
}

/// The bare names a table LISTS, where it lists at least two.
///
/// `kong/db/migrations/core/init.lua` returns
/// `{"000_base", "003_100_to_110", ...}` and a migration runner
/// requires each beside it. Unread, every one of kong's 24 core
/// migrations and the 39 its plugins ship is depended on by nothing. A
/// registry is a LIST, and asking for two separates one from a lone
/// string that happens to share a sibling's name.
///
/// A field's VALUE is its last named child, whatever precedes it.
/// Reading the first instead reads `["migrations"] = "..."` as the name
/// `migrations` and `name = "core"` as the name `name`, which leaves
/// every keyed registry in the corpus stating nothing:
/// `kong/db/migrations/subsystems.lua` returns
/// `{ name = "core", namespace = "kong.db.migrations.core" }` and
/// `db/migrations/state.lua:98` does `require(ss.namespace)`.
///
/// A dot is allowed for the same reason. `luarocks/src/bin/luarocks`
/// maps its 22 commands to `luarocks.cmd.<name>` and `cmd.lua:570`
/// loads each with `pcall(require, module)`; `kong/pdk/init.lua:193`
/// lists `service.request` for the `require("kong.pdk." .. module_name)`
/// on line 263. Where a dotted name reaches nothing it states nothing,
/// which is the same bargain the bare form already made.
fn listed_names(table: Node, src: &[u8]) -> Vec<String> {
    let mut cursor = table.walk();
    let names: Vec<String> = table
        .named_children(&mut cursor)
        .filter(|f| f.kind() == "field")
        .filter_map(|f| f.named_child(f.named_child_count().checked_sub(1)? as u32))
        .filter(|v| v.kind() == "string")
        .map(|v| quoted(v, src).to_string())
        .filter(|t| !t.is_empty() && !t.contains(['/', ' ']))
        .collect();
    match names.len() >= 2 || is_the_module(table) {
        true => names,
        false => Vec::new(),
    }
}

/// Is this table the whole thing the chunk returns?
///
/// The two-entry threshold keeps a lone incidental string from claiming
/// a sibling file, and a module's own export is where that risk is
/// absent: `kong/plugins/ai-proxy/migrations/init.lua` is exactly
/// `return { "001_360_to_370" }`, six more plugins ship the same
/// one-line index, and `db/migrations/state.lua:107` builds
/// `fmt("%s.%s", subsys.namespace, mig_name)` out of the names in it.
fn is_the_module(table: Node) -> bool {
    let Some(list) = table.parent().filter(|p| p.kind() == "expression_list") else {
        return false;
    };
    list.named_child_count() == 1
        && list
            .parent()
            .filter(|r| r.kind() == "return_statement")
            .and_then(|r| r.parent())
            .is_some_and(|c| c.kind() == "chunk")
}

/// The module name a format template builds, with `*` where the source
/// stops being able to say: the same reading `..` already gets, for the
/// spelling `string.format` gives it.
///
/// `kong/db/strategies/init.lua:24` is
/// `require(fmt("kong.db.strategies.%s.connector", database))`, `:27`
/// drops the leaf, and `:63` builds `"kong.db.strategies.%s.%s"` for
/// the `load_module_if_exists` that `kong/tools/module.lua:18` defines
/// as `xpcall(require, debug.traceback, module_name)`. Which function
/// formats is deliberately not read: kong's is `fmt`, luarocks writes
/// `("luarocks.fs.%s.tools"):format(...)`, and the template says the
/// same thing either way.
///
/// The separator before the first hole is the only filter. 67 strings
/// in 30 files pass it, and the ones that are not module paths cost
/// nothing: kong's URL patterns `"/%s"` and `"/%s/:%s"`
/// (api/endpoints.lua:747,755) reduce to a leading star, which
/// `glob_modules` refuses outright, and `"lambda.%s.amazonaws.com"`
/// states a prefix no module answers to.
fn templated(args: Node, src: &[u8]) -> Option<String> {
    let call = args.parent()?;
    // A literal template written straight into a `require` is already
    // read as the target it states; reading it twice would say it twice.
    if requires(call, src) {
        return None;
    }
    let mut cursor = args.walk();
    let text = quoted(
        args.named_children(&mut cursor)
            .find(|c| c.kind() == "string")?,
        src,
    );
    let (before, _) = text.split_once("%s")?;
    before
        .ends_with(['.', '/'])
        .then(|| text.replace("%s", "*"))
}

/// The file a `dofile` or `loadfile` names, relative to the script
/// holding the call.
///
/// `c/lua/testes/all.lua` is 28 `dofile('...')` lines — `dofile('main.lua')`,
/// `assert(dofile('attrib.lua') == 27)` — and is the only file naming
/// any of them, so unread Lua's own test suite is 29 files nothing
/// references. The `.lua` requirement separates a module from the
/// suite's other arguments to the same call: gold's literals also
/// include `"nomenaoexistente"` and `"# a non-ending comment"`, which
/// are inputs to an error path rather than files.
fn loaded_file(args: Node, src: &[u8]) -> Option<String> {
    let call = args.parent()?;
    if !matches!(callee_text(call, src), Some("dofile" | "loadfile")) {
        return None;
    }
    let mut cursor = args.walk();
    let text = quoted(
        args.named_children(&mut cursor)
            .find(|c| c.kind() == "string")?,
        src,
    );
    Some(text.strip_suffix(".lua")?.to_string())
}

/// The module name a concatenation builds, with `*` where the source
/// stops being able to say.
///
/// `"kong.plugins." .. name .. ".schema"` names every module of the
/// form `kong.plugins.<anything>.schema`. Both literals are readable
/// and only the middle is not. The prefix must end at a separator; a
/// concatenation starting anywhere else states no path at all.
///
/// The suffix tells kong's 150 plugin DIRECTORIES apart from the files
/// inside them: nothing sits directly under `kong/plugins`,
/// so the prefix alone reaches nothing while `kong.plugins.*.handler`
/// names 35 real files.
fn concatenated(node: Node, src: &[u8]) -> Option<String> {
    // `..` is RIGHT-associative, so the two literals sit at the ends of
    // the two spines rather than as children of the outermost node.
    let head = spine(node, "left");
    let bound;
    let prefix = match head.kind() {
        "identifier" => {
            bound = file_scope_string(head, src)?;
            bound.as_str()
        }
        _ => quoted(head, src),
    };
    let suffix = quoted(spine(node, "right"), src);
    if !prefix.ends_with(['.', '/']) {
        return None;
    }
    // A literal must meet the run-time value at a separator, or `*`
    // would stand for part of a name rather than a whole one.
    let tail = suffix.starts_with(['.', '/']).then_some(suffix);
    Some(format!("{prefix}*{}", tail.unwrap_or("")))
}

/// The string a file-scope `local` binds to this name.
///
/// `kong/observability/tracing/propagation/init.lua:16` writes
/// `local INJECTORS_PATH = "kong.observability.tracing.propagation.injectors."`
/// and `:148` writes `require(INJECTORS_PATH .. injector_m)`; the eight
/// injectors it loads are named nowhere else. The rule is narrow by
/// design: reading any standalone string that ends in a separator as a
/// glob prefix covers 218 files for the same 8 orphans, while requiring
/// a name a load concatenates leaves one call site in the whole corpus.
fn file_scope_string(name: Node, src: &[u8]) -> Option<String> {
    let wanted = name.utf8_text(src).ok()?;
    let mut chunk = name;
    while let Some(parent) = chunk.parent() {
        chunk = parent;
    }
    let mut cursor = chunk.walk();
    chunk
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "variable_declaration")
        .filter_map(|d| d.named_child(0))
        .find_map(|assign| {
            let bound = assign.named_child(0)?.named_child(0)?;
            let value = assign.named_child(1)?.named_child(0)?;
            (bound.utf8_text(src).ok()? == wanted && value.kind() == "string")
                .then(|| quoted(value, src).to_string())
        })
}

/// The far end of a concatenation's spine: the outermost literal on
/// one side, which is as far as the source keeps saying.
fn spine<'t>(mut node: Node<'t>, side: &str) -> Node<'t> {
    while node.kind() == "binary_expression" {
        match node.child_by_field_name(side) {
            Some(next) => node = next,
            None => break,
        }
    }
    node
}

/// A string literal's text, without its quotes.
fn quoted<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("").trim_matches(['"', '\''])
}

/// Text compiled at run time, and the environment swapped underneath a
/// function: after either, the source stops predicting the run.
///
/// `rawget`/`rawset` are deliberately NOT here: they bypass a metatable
/// EXPLICITLY, and an object system written by hand is where that form
/// belongs. Counting them scores Penlight's class module at 22.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && matches!(
            callee_text(node, src),
            Some("load" | "loadstring" | "dofile" | "setfenv" | "loadfile")
        )
}

/// `not x` is the only negation operator.
fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let text = node.utf8_text(src).ok()?;
    (node.kind() == "unary_expression" && text.trim_start().starts_with("not"))
        .then(|| node.named_child(0))?
}

/// A table literal whose keys are all names is an undeclared shape: no
/// type catches a typo in one, and adding a field means finding every
/// site by hand.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "table_constructor" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "field")
        .filter_map(|f| f.child_by_field_name("name"))
        .filter_map(|n| n.utf8_text(src).ok())
        .map(Into::into)
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// busted and its ancestors all spell a test the same way: a call to
/// `it`, `describe` or a sibling of theirs, taking a name and a
/// function. The test is that FUNCTION — the same shape jest gives
/// TypeScript — so the evidence is read from the call the body was
/// handed to, and `refine` promotes the body to a unit so there is
/// something to judge.
fn declaring_test<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let args = node.parent().filter(|p| p.kind() == "arguments")?;
    let call = args.parent().filter(|c| c.kind() == "function_call")?;
    matches!(
        callee_text(call, src),
        Some("it" | "describe" | "test" | "spec" | "context")
    )
    .then_some(args)
}

fn declares_test(node: Node, src: &[u8]) -> bool {
    declaring_test(node, src).is_some()
}

/// `pending("...")` is busted's skip.
fn skips_test(node: Node, src: &[u8]) -> bool {
    matches!(callee_text(node, src), Some("pending"))
}

/// `local` is the only privacy the language has, so anything not
/// declared local is reachable by whoever holds the table.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src)
        .is_ok_and(|t| !t.trim_start().starts_with("local"))
}

/// A run of `---` or an `--[[ ]]` block immediately above the function.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &["---", "--[["], src)
}

/// The nodes this pack reads a module name out of that are not calls.
///
/// A table LISTING names is how Lua writes a registry, and the names
/// are not otherwise said anywhere; an argument list holds a format
/// template or a `dofile` path. Both kinds are unmapped, so promoting
/// either costs nothing: the table's strings stay strings for the
/// secret, repetition and clone checks, and `dofile` stays a call for
/// the spooky one.
fn states_a_module(node: Node, src: &[u8]) -> bool {
    match node.kind() {
        "table_constructor" => !listed_names(node, src).is_empty(),
        "arguments" => templated(node, src).is_some() || loaded_file(node, src).is_some(),
        _ => false,
    }
}

/// `elseif` is its own kind here rather than a nested if, so it needs
/// no flattening. `and`/`or` share the binary kind with every
/// arithmetic operator, and only those two sequence a condition.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        // A busted test is an anonymous function handed to `it`; without
        // promoting it there is no unit for the test metrics to judge.
        Sem::Lambda if declaring_test(node, src).is_some() => Sem::FnDef,
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("and" | "or") => Sem::BoolOp,
            // A concatenation that BUILDS a module name is where the
            // dependency is stated, and it is often nowhere near the
            // `require`: kong writes `local plugin_handler =
            // "kong.plugins." .. plugin .. ".handler"` on one line and
            // requires the local on another.
            Some("..") if concatenated(node, src).is_some() => Sem::Import,
            _ => Sem::None,
        },
        Sem::None if states_a_module(node, src) => Sem::Import,
        // Lua's import is a CALL and the core asks about imports at
        // `Sem::Import` nodes, so without this arm the pack's `imports`
        // hook is never once asked and Lua has no module graph.
        Sem::Call if requires(node, src) => Sem::Import,
        // `return` at the tail of a chunk is the module's export, not a
        // jump out of control flow.
        Sem::Jump
            if node.kind() == "return_statement"
                && node.parent().is_some_and(|p| p.kind() == "chunk") =>
        {
            Sem::None
        }
        _ => sem,
    }
}

#[cfg(test)]
mod tests {
    use crate::lang::{Lang, Reach};

    fn imports_of(src: &str) -> Vec<(String, Reach)> {
        let pack = Lang::Lua.pack();
        let f = crate::facts::extract(
            pack,
            &mut pack.make_parser(),
            std::path::Path::new("kong/db/init.lua"),
            src,
        );
        f.imports
            .iter()
            .map(|i| (i.target.to_string(), i.reach))
            .collect()
    }

    #[test]
    fn a_registry_reads_a_fields_value_and_keeps_its_dots() {
        // kong/db/migrations/subsystems.lua returns exactly this, and
        // db/migrations/state.lua:98 does `require(ss.namespace)`.
        // Reading the field's FIRST child takes the key `name`, and
        // rejecting a dot drops the namespace itself, which leaves
        // every keyed registry in the corpus stating nothing.
        let src = "return {\n  { name = \"core\", namespace = \"kong.db.migrations.core\" },\n  { name = \"acl\", namespace = \"kong.plugins.acl.migrations\" },\n}\n";
        assert_eq!(
            imports_of(src),
            [
                ("core".to_string(), Reach::Mention),
                ("kong.db.migrations.core".to_string(), Reach::Mention),
                ("acl".to_string(), Reach::Mention),
                ("kong.plugins.acl.migrations".to_string(), Reach::Mention),
            ]
        );
    }

    #[test]
    fn the_table_a_chunk_returns_may_hold_one_name() {
        // kong/plugins/ai-proxy/migrations/init.lua is exactly this,
        // and six more plugins ship the same one-line index. The
        // two-entry threshold keeps a lone incidental string from
        // claiming a sibling; a module's whole export is not one.
        assert_eq!(
            imports_of("return {\n  \"001_360_to_370\",\n}\n"),
            [("001_360_to_370".to_string(), Reach::Mention)]
        );
        // A lone string inside an ordinary table still states nothing.
        assert!(imports_of("local t = { \"001_360_to_370\" }\nreturn t\n").is_empty());
    }

    #[test]
    fn a_format_template_and_a_dofile_are_read_without_costing_the_call() {
        // kong/db/strategies/init.lua:24 and :63; c/lua/testes/all.lua
        // is 28 `dofile` lines and is the only file naming any of them.
        let src = "local a = require(fmt(\"kong.db.strategies.%s.connector\", db))\n\
                   local b = load_module_if_exists(fmt(\"kong.db.strategies.%s.%s\", db, s))\n\
                   dofile('main.lua')\n\
                   local c = fmt(\"lambda.%s.amazonaws.com\", region)\n\
                   local d = fmt(\"%s.%s\", a, b)\n";
        assert_eq!(
            imports_of(src),
            [
                ("kong.db.strategies.*.connector".to_string(), Reach::Mention),
                ("kong.db.strategies.*.*".to_string(), Reach::Mention),
                // A path from the calling script, so a miss is a miss.
                ("main".to_string(), Reach::Project),
                // A hostname is shaped like a dotted path and is read
                // as one; no file answers to it, so it costs nothing.
                ("lambda.*.amazonaws.com".to_string(), Reach::Mention),
            ],
            "a template whose hole opens the string states no prefix at all"
        );
    }

    #[test]
    fn a_prefix_a_file_scope_local_holds_is_still_a_prefix() {
        // kong/observability/tracing/propagation/init.lua binds the
        // path at :16 and concatenates it at :148; the eight injectors
        // it loads are named nowhere else.
        let src = "local INJECTORS_PATH = \"kong.observability.tracing.propagation.injectors.\"\n\
                   local function f(m) return require(INJECTORS_PATH .. m) end\n";
        assert_eq!(
            imports_of(src),
            [(
                "kong.observability.tracing.propagation.injectors.*".to_string(),
                Reach::Anywhere
            )]
        );
        // A name the file never binds to a string says nothing.
        assert!(imports_of("local function f(m) return require(NOPE .. m) end\n").is_empty());
    }

    #[test]
    fn a_tests_directory_is_test_code_like_a_test_directory() {
        // Penlight files its suite under tests/, and matching only
        // /test/ counts 43 of its 115 modules as production.
        let is_test = super::pack().test_path;
        for p in [
            "Penlight/tests/test-pretty.lua",
            "lua-language-server/test/main.lua",
            "Penlight/spec/utils_spec.lua",
            "kong/kong/plugins/acl/handler_spec.lua",
        ] {
            assert!(is_test(p), "{p}");
        }
        for p in ["kong/kong/router/init.lua", "Penlight/lua/pl/latest.lua"] {
            assert!(!is_test(p), "{p}");
        }
    }
}
