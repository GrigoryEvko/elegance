//! Elixir: there is no syntax, so the ontology is built from names.
//!
//! `def`, `defmodule`, `if`, `case` and `try` are all ordinary calls
//! taking a block — the language is homoiconic and its grammar says so,
//! offering `call` where every other pack here reads a keyword. So this
//! pack does its whole classification in `refine`, dispatching on the
//! call's target text. A macro a library defines is indistinguishable
//! from one the language ships, which is the language working as
//! designed and a real bound on what can be read: `Enum.each` is a
//! function call and reads as one, so loops read low the way they do for
//! OCaml and Ruby.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    // Everything interesting arrives here and is sorted in `refine`.
    ("call", Sem::Call),
    ("anonymous_function", Sem::Lambda),
    // The `->` clause: a case arm, a rescue arm, a function head.
    ("stab_clause", Sem::CaseArm),
    ("binary_operator", Sem::BoolOp),
    ("comment", Sem::Comment),
    ("identifier", Sem::Ident),
    ("alias", Sem::Ident),
    ("integer", Sem::NumLit),
    ("float", Sem::NumLit),
    ("string", Sem::StrLit),
    ("charlist", Sem::StrLit),
    ("atom", Sem::StrLit),
    ("boolean", Sem::BoolLit),
];

/// Nothing: a local is bound by `=`, which shares its node with every
/// comparison and arithmetic operator, so pointing at it would call
/// every `==` a definition.
const DEF_SITES: &[(&str, &str)] = &[];

/// Rebinding is the norm here and carries none of the meaning it does
/// elsewhere — `x = transform(x)` is a pipeline written without `|>`.
const REASSIGNS: &[(&str, &str)] = &[];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_elixir::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    Pack {
        lang: Lang::Elixir,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        attr_name: None,
        sems,
        def_sites,
        reassigns: Box::new([]),
        attr: None,
        scope_sep: ".",
        return_type_field: "",
        bool_op_field: "operator",
        call_target_fields: &["target"],
        types_declared: false,
        record_keys,
        // A process owns its resources and dies with them; that is the
        // supervision tree's job rather than a scope guard's.
        // Concurrency is processes and Task, never a keyword.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &["@doc", "@moduledoc"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        negation_operand,
        catch_sin,
        swallows_error,
        loses_context,
        // raise/throw/exit are Elixir's raise, not a panic beside it:
        // `unwraps` is declared dead here.
        panicky: |_, _| false,
        declares_test,
        names_test: declares_test,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/test/") || p.ends_with("_test.exs"),
        asserty,
        is_hook: |_, _| false,
        // A function returns a term, commonly `{:ok, value}`. The tuple
        // is a value rather than a declaration, so there is no width.
        return_arity: |_, _| 0,
        // A behaviour declares callbacks with `@callback` attributes,
        // which are module attributes rather than a body this pack can
        // count members of.
        interfaces: |_, _| Vec::new(),
        skips_test,
        magic_exempt: &["map", "keywords", "list"],
        assign_kinds: &[],
    }
}

/// The target of a call: `def` for a definition, `Enum.map` for a call.
fn target_text<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name("target")?.utf8_text(src).ok()
}

/// The argument list is a KIND here, not a field — the grammar labels
/// only `target`, `left`, `right`, `operator`, `key` and `value`.
fn args_of(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "arguments")
}

/// A definition's name lives one level down: `def run(x)` is a call to
/// `def` whose first argument is itself a call, named `run`.
fn name_node(node: Node) -> Option<Node> {
    let args = args_of(node)?;
    let first = args.named_child(0)?;
    match first.kind() {
        // `def run(x)` — a call whose target is the name.
        "call" => first.child_by_field_name("target"),
        // `def run do`, and `defmodule Foo do`.
        "identifier" | "alias" => Some(first),
        // `test "keeps the total" do` — the name is PROSE, the way a
        // Zig test label is.
        "string" => Some(first),
        // `def run(x) when is_list(x)` — the guard wraps the head.
        "binary_operator" => first
            .child_by_field_name("left")
            .and_then(|l| l.child_by_field_name("target").or(Some(l))),
        _ => None,
    }
}

/// One segment of a module name as Elixir's own `Macro.underscore`
/// writes it, which is how a module name becomes a file path:
/// `Plug.CSRFProtection` lives at `plug/csrf_protection.ex`.
///
/// An underscore goes before an upper-case letter that follows a
/// lower-case one or a digit, and before the last of a run of
/// upper-case letters when a lower-case one follows it. So `HTML`
/// stays `html` while `CSRFProtection` splits.
pub(crate) fn underscore(segment: &str) -> String {
    let chars: Vec<char> = segment.chars().collect();
    let mut out = String::with_capacity(segment.len() + 2);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && starts_a_word(&chars, i) {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Whether the upper-case letter at `i` begins a word rather than
/// continuing an acronym.
fn starts_a_word(chars: &[char], i: usize) -> bool {
    let prev = chars[i - 1];
    prev.is_lowercase()
        || prev.is_ascii_digit()
        || (prev.is_uppercase() && chars.get(i + 1).is_some_and(|n| n.is_lowercase()))
}

/// `import`, `alias`, `require` and `use` pull a module in, and so does
/// writing its name: the `alias` NODE promoted by `refine` arrives here
/// too.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    // A module named in expression position — `Plug.Conn.send_resp(...)`
    // — is the reference itself, with no statement around it.
    if node.kind() == "alias" {
        return node.utf8_text(src).map(module).into_iter().collect();
    }
    if !matches!(
        target_text(node, src),
        Some("import" | "alias" | "require" | "use")
    ) {
        return Vec::new();
    }
    let Some(first) = args_of(node).and_then(|a| a.named_child(0)) else {
        return Vec::new();
    };
    match first.kind() {
        "alias" => first.utf8_text(src).map(module).into_iter().collect(),
        // `alias Foo.{Bar, Baz}` binds two modules, written as a dot
        // onto a tuple. Reading the argument as one alias found none of
        // it: gold holds 225 such statements naming 597 modules, so a
        // filter on the alias kind dropped every one to nothing.
        "dot" => braces(first, src),
        _ => Vec::new(),
    }
}

fn module(target: &str) -> super::ImportInfo {
    super::ImportInfo {
        target: target.into(),
        names: Vec::new(),
        reach: super::Reach::Anywhere,
    }
}

/// The members of `alias Foo.{Bar, Baz}`, each spelled out in full.
fn braces(dot: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let Some(base) = dot
        .named_child(0)
        .filter(|b| b.kind() == "alias")
        .and_then(|b| b.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    let Some(tuple) = dot.named_child(1).filter(|t| t.kind() == "tuple") else {
        return Vec::new();
    };
    let mut cursor = tuple.walk();
    tuple
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "alias")
        .filter_map(|c| c.utf8_text(src).ok())
        .map(|leaf| module(&format!("{base}.{leaf}")))
        .collect()
}

/// Whether this `alias` node is a module reference rather than a name
/// some statement around it already declares.
///
/// Elixir needs no import to depend on a module: `Plug.Conn.send_resp`
/// spelled out in full is an edge, and gold expresses most of its
/// dependency that way — 9308 dotted `alias` nodes stand in expression
/// position across the five repos against 2458 alias/import/require/use
/// statements, and 6686 of them name a module the corpus defines.
///
/// A single segment is excluded. It is how the language spells its OWN
/// modules — `Mix`, `Config`, `Repo`, `Inspect` — and a one-component
/// suffix match lands on whatever file bears that name: of 165 distinct
/// single-segment names that resolved, 9 pointed at a file declaring no
/// such module, and those 9 carried 537 of the occurrences. Requiring a
/// dot leaves 620 distinct names of which 619 land on a file that
/// declares them.
fn names_a_module(node: Node, src: &[u8]) -> bool {
    node.utf8_text(src).is_ok_and(|t| t.contains('.')) && !declared_here(node, src)
}

fn parent_of<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    node.parent().filter(|p| p.kind() == kind)
}

/// The name an import statement or a `defmodule` writes down, which
/// `imports` already reads from the statement: `alias A.B`, `use Plug`,
/// the base and the members of `alias Foo.{Bar, Baz}`, and the module's
/// own name in `defmodule Foo.Bar do`.
fn declared_here(node: Node, src: &[u8]) -> bool {
    let head = parent_of(node, "tuple").unwrap_or(node);
    let head = parent_of(head, "dot").unwrap_or(head);
    let Some(args) = parent_of(head, "arguments") else {
        return false;
    };
    if args.named_child(0).map(|c| c.id()) != Some(head.id()) {
        return false;
    }
    args.parent().is_some_and(|call| {
        matches!(
            target_text(call, src),
            // `defimpl Draft, for: Blueprint` is absent on purpose: it
            // names the protocol and the struct, both dependencies.
            Some("import" | "alias" | "require" | "use" | "defmodule" | "defprotocol")
        )
    })
}

/// Parameters are the patterns in the head, and a pattern is not always
/// a name — `def handle(%User{id: id})` destructures.
///
/// A shape is still a PARAMETER: the head takes it, a caller passes it,
/// and reading only the plain identifiers made `def put(%Entry{} = e,
/// state)` look like a function of one argument. It is recorded under
/// its own text and marked `destructured`, so the checks that need a
/// binding NAME can pass over it and the ones that only count arguments
/// count it.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    let parent = node.parent()?;
    if parent.kind() != "arguments" {
        return None;
    }
    // The head of a `def`: its parent call sits under a `def` call.
    let head = parent.parent()?;
    let outer = head.parent()?.parent()?;
    if !matches!(target_text(outer, src), Some("def" | "defp" | "defmacro")) {
        return None;
    }
    let (holder, boolish) = named_default(node, src).unwrap_or((node, false));
    Some(ParamInfo {
        name: holder.utf8_text(src).ok()?.into(),
        typed: false,
        boolish,
        destructured: holder.kind() != "identifier",
        ..Default::default()
    })
}

/// `dry_run \\ false` — the one binary operator in a head that names a
/// parameter, and the value beside it is the only evidence a head has
/// that a parameter is a switch. Every other operator here is a pattern
/// match (`%Entry{} = e`, `[h | t]`), which binds by shape.
fn named_default<'t>(node: Node<'t>, src: &[u8]) -> Option<(Node<'t>, bool)> {
    let name = node.child_by_field_name("left")?;
    let default = node.child_by_field_name("right")?;
    (node.kind() == "binary_operator"
        && name.kind() == "identifier"
        && super::field_text_is(node, "operator", src) == Some("\\\\"))
    .then_some((name, default.kind() == "boolean"))
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    let bare = unit_name.rsplit('.').next().unwrap_or(unit_name);
    target_text(call, src).is_some_and(|t| t.rsplit('.').next().unwrap_or(t) == bare)
}

/// Code compiled at run time, and a function named by a term.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call
        && target_text(node, src).is_some_and(|t| {
            matches!(
                t.rsplit('.').next().unwrap_or(t),
                "eval_string" | "eval_quoted" | "apply" | "compile_string" | "compile_quoted"
            )
        })
}

/// `!x` and the word form `not x`.
fn negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    if node.kind() != "unary_operator" {
        return None;
    }
    let text = node.utf8_text(src).ok()?;
    let negated = text.starts_with('!') || text.starts_with("not ");
    let operand = negated.then(|| node.child_by_field_name("operand"))??;
    // `not (a and b)` — the parentheses arrive as a one-child `block`,
    // which is a GROUP here rather than a body, and leaving it wrapped
    // hid every De Morgan candidate in the language.
    match operand.kind() == "block" && operand.named_child_count() == 1 {
        true => operand.named_child(0),
        false => Some(operand),
    }
}

/// The `rescue` block of a `try`, which the grammar hangs inside the
/// try's do_block rather than beside it. Every handler check below is
/// asked of the TRY, because that is the node the ontology names — a
/// rescue_block carries no Sem of its own and the core never reaches it.
fn rescue_block<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    if target_text(node, src) != Some("try") {
        return None;
    }
    let mut cursor = node.walk();
    let body = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "do_block")?;
    let mut inner = body.walk();
    body.named_children(&mut inner)
        .find(|c| c.kind() == "rescue_block")
}

/// The `->` arms of a rescue.
fn rescue_arms<'t>(block: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = block.walk();
    block
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "stab_clause")
        .collect()
}

/// A `rescue` with no arms at all, or whose only arm does nothing.
fn swallows_error(node: Node, src: &[u8]) -> bool {
    let Some(block) = rescue_block(node, src) else {
        return false;
    };
    let arms = rescue_arms(block);
    arms.is_empty()
        || arms.iter().all(|arm| {
            arm.child_by_field_name("right")
                .and_then(|b| b.utf8_text(src).ok())
                .is_none_or(|t| matches!(t.trim(), "" | "nil" | ":ok"))
        })
}

/// `rescue e ->` binds every exception the runtime can raise;
/// `rescue e in RuntimeError ->` names one and is the narrow form.
fn catch_sin(node: Node, src: &[u8]) -> Option<super::CatchSin> {
    let block = rescue_block(node, src)?;
    let broad = rescue_arms(block).iter().any(|arm| {
        arm.child_by_field_name("left")
            .and_then(|l| l.utf8_text(src).ok())
            .is_some_and(|t| !t.contains(" in "))
    });
    broad.then_some(super::CatchSin::Broad)
}

/// A rescue arm that raises a NEW error and never names the one it
/// bound: the stacktrace that explains the failure is gone.
fn loses_context(node: Node, src: &[u8]) -> bool {
    let Some(block) = rescue_block(node, src) else {
        return false;
    };
    rescue_arms(block).iter().any(|arm| {
        let Some(bound) = arm
            .child_by_field_name("left")
            .and_then(|l| l.named_child(0))
            .and_then(|n| n.utf8_text(src).ok())
        else {
            return false;
        };
        let Some(body) = arm.child_by_field_name("right") else {
            return false;
        };
        raises_without(body, bound, src)
    })
}

/// Does this body `raise` something that never mentions `bound`?
fn raises_without(body: Node, bound: &str, src: &[u8]) -> bool {
    let mut stack = vec![body];
    let mut raises = false;
    while let Some(n) = stack.pop() {
        if target_text(n, src) == Some("raise") {
            let text = n.utf8_text(src).unwrap_or("");
            if super::mentions(text, bound) {
                return false;
            }
            raises = true;
            continue;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    raises
}

/// ExUnit declares a test with `test "name" do`.
fn declares_test(node: Node, src: &[u8]) -> bool {
    matches!(
        target_text(node, src),
        Some("test" | "property" | "describe")
    )
}

/// `@tag :skip` is an attribute written ABOVE the test, so the sibling
/// is where the evidence lives — the test's own text never holds it.
fn skips_test(node: Node, src: &[u8]) -> bool {
    let tagged = |n: Node| {
        n.utf8_text(src)
            .is_ok_and(|t| t.starts_with("@tag :skip") || t.starts_with("@tag :pending"))
    };
    tagged(node) || node.prev_named_sibling().is_some_and(tagged)
}

fn asserty(call: Node, src: &[u8]) -> bool {
    target_text(call, src).is_some_and(|t| {
        super::assertish(t) || matches!(t, "assert" | "refute" | "assert_receive" | "assert_raise")
    })
}

/// `defp` is the private form; `def` is public and there is no third.
fn is_public(node: Node, src: &[u8]) -> bool {
    target_text(node, src) != Some("defp")
}

/// `@doc """..."""` above the definition. The attribute is a call, so
/// this reads the sibling rather than a comment run.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    let prev = node.prev_named_sibling()?;
    match target_text(prev, src) {
        Some("@doc" | "@moduledoc") => super::node_span(prev),
        _ => None,
    }
}

/// A map literal with atom keys is a shape nothing declares — the same
/// argument as a Ruby hash or a Lua table.
fn record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>> {
    if node.kind() != "map" {
        return None;
    }
    let mut cursor = node.walk();
    let keys: Vec<Box<str>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "map_content")
        .flat_map(|content| {
            let mut inner = content.walk();
            content
                .named_children(&mut inner)
                .filter_map(|p| p.child_by_field_name("key"))
                .filter_map(|k| k.utf8_text(src).ok())
                .map(Box::<str>::from)
                .collect::<Vec<_>>()
        })
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// Which construct a call IS, decided by the name being called. A
/// `call` is the only structural node the grammar offers, so every
/// keyword this language appears to have is recognised here or not at
/// all.
fn called_construct(target: Option<&str>) -> Sem {
    match target {
        Some("def" | "defp" | "defmacro" | "defmacrop" | "defdelegate" | "defguard") => Sem::FnDef,
        Some("defmodule" | "defprotocol" | "defimpl" | "defstruct" | "defexception") => {
            Sem::TypeDef
        }
        // ExUnit's `test "name" do` is a call, and promoting it is what
        // gives the test metrics a unit to judge.
        Some("test" | "property" | "describe") => Sem::FnDef,
        Some("if" | "unless") => Sem::If,
        Some("case" | "cond" | "with" | "receive") => Sem::Match,
        // `for` is a comprehension, which is this language's loop.
        Some("for") => Sem::Loop,
        Some("try") => Sem::Try,
        Some("import" | "alias" | "require" | "use") => Sem::Import,
        _ => Sem::Call,
    }
}

/// The rest of the ontology: the operators the grammar does name.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::Call => called_construct(target_text(node, src)),
        // `and`/`or`/`&&`/`||` sequence a condition; `|>`, `<>`, `++`
        // and every comparison share the node and do not.
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("and" | "or" | "&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        // A module name written out is the dependency; there is no
        // import statement to read instead.
        Sem::Ident if node.kind() == "alias" && names_a_module(node, src) => Sem::Import,
        _ => sem,
    }
}

#[cfg(test)]
mod tests {
    use crate::lang::Lang;

    fn targets(src: &str) -> Vec<String> {
        let pack = Lang::Elixir.pack();
        let mut parser = pack.make_parser();
        let f = crate::facts::extract(pack, &mut parser, std::path::Path::new("a.ex"), src);
        f.imports.iter().map(|i| i.target.to_string()).collect()
    }

    #[test]
    fn a_braces_alias_binds_every_module_it_lists() {
        // gold holds 225 of these naming 597 modules; reading the
        // argument as one alias node found none of them, because the
        // grammar writes the form as a dot onto a tuple.
        assert_eq!(
            targets("defmodule X do\n  alias Oban.{Config, Job, Notifier}\nend\n"),
            ["Oban.Config", "Oban.Job", "Oban.Notifier"]
        );
    }

    #[test]
    fn a_module_written_out_is_a_dependency_with_no_statement() {
        // Every form the corpus uses: a qualified call, a struct, an
        // argument, a raise. The module's own name and the target of an
        // alias are excluded — the statement path already reads those.
        let src = "defmodule App.Worker do\n\
                   \x20 alias App.Repo\n\
                   \x20 def run(c) do\n\
                   \x20   Plug.Conn.send_resp(c, 200, \"\")\n\
                   \x20   %Absinthe.Blueprint.Input.Field{}\n\
                   \x20   Repo.insert(Ecto.Changeset.change(c))\n\
                   \x20 end\n\
                   end\n";
        let mut got = targets(src);
        got.sort();
        assert_eq!(
            got,
            [
                "Absinthe.Blueprint.Input.Field",
                "App.Repo",
                "Ecto.Changeset",
                "Plug.Conn",
            ]
        );
    }

    #[test]
    fn a_single_segment_name_is_not_read_as_a_dependency() {
        // `Mix`, `Config`, `Repo` are the language's own, and a
        // one-component suffix match lands on whatever file bears the
        // name: gold resolved 165 such names of which 9 pointed at a
        // file declaring no such module, carrying 537 occurrences.
        assert!(
            targets("defmodule X do\n  def f, do: Enum.map(Mix.env(), & &1)\nend\n").is_empty()
        );
    }

    #[test]
    fn a_module_segment_underscores_the_way_elixir_does() {
        // Checked against `Macro.underscore/1`. The acronym cases are
        // the ones a plain lowercase would get wrong, and gold holds
        // both: Plug.CSRFProtection and Phoenix.HTML.
        let cases = [
            ("Conn", "conn"),
            ("CSRFProtection", "csrf_protection"),
            ("KnownDirectives", "known_directives"),
            ("HTML", "html"),
            ("V2Handler", "v2_handler"),
            ("JSONSerializer", "json_serializer"),
            ("Plug", "plug"),
        ];
        for (name, want) in cases {
            assert_eq!(super::underscore(name), want, "{name}");
        }
    }
}
