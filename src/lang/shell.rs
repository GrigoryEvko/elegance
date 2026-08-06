//! Shell: the language that provisions the cluster.
//!
//! Deployment scripts are where a mistake costs the most and review
//! reaches least — one k8s repository here holds 22.6k lines of it,
//! including a 3,287-line registry provisioner — so the gap between
//! "we measure nine languages" and "we measure the code that runs
//! production" was this pack.
//!
//! Honest caveats, by decision:
//! - A shell function declares NO parameters: `$1` and `$2` are read
//!   from the caller's frame. Every interface metric is therefore
//!   structurally silent, which is a fact about the language rather
//!   than a gap in the pack.
//! - `.sh`/`.bash` only. An extensionless script with a shebang is
//!   real and common, but `Lang::from_path` is a pure path predicate
//!   that several modes call during the walk, and sniffing every
//!   extensionless file would change what a walk costs. Deferred, not
//!   denied.
//! - `cmd || true` is EXPLICIT silencing — the Zen's own exemption —
//!   so it is not counted as a swallowed error.

use tree_sitter::Node;

use super::{Lang, Pack, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    ("function_definition", Sem::FnDef),
    ("if_statement", Sem::If),
    ("elif_clause", Sem::ElseIf),
    ("else_clause", Sem::Else),
    ("for_statement", Sem::Loop),
    ("c_style_for_statement", Sem::Loop),
    // `until` shares the while node: both are a loop to a reader.
    ("while_statement", Sem::Loop),
    ("case_statement", Sem::Match),
    ("case_item", Sem::CaseArm),
    // `&&`/`||` and ONLY those — `;` sequencing produces no node of
    // its own and `|` is a pipeline, so this needs no refinement. A
    // chain nests (`a && b && c` is list(list(a,b),c)), which is what
    // makes the cognitive sequence-dedup work without an operator
    // field to compare.
    ("list", Sem::BoolOp),
    ("command", Sem::Call),
    ("comment", Sem::Comment),
    ("command_name", Sem::Ident),
    ("variable_name", Sem::Ident),
    ("word", Sem::Ident),
    ("number", Sem::NumLit),
    ("string", Sem::StrLit),
    ("raw_string", Sem::StrLit),
];

const DEF_SITES: &[(&str, &str)] = &[
    ("variable_assignment", "name"),
    ("for_statement", "variable"),
];
/// One kind covers `=` and `+=` alike; the check filters by the
/// spelled operator.
const REASSIGNS: &[(&str, &str)] = &[("variable_assignment", "name")];

/// How a script pulls in another one. `.` is POSIX; `source` is its
/// readable spelling.
const SOURCING: &[&str] = &["source", "."];

pub fn pack() -> Pack {
    let ts: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    let reassigns = super::def_table(&ts, REASSIGNS);
    Pack {
        lang: Lang::Shell,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        reassign_names: REASSIGNS,
        // No member access: a shell value is a string, not an object.
        attr_name: None,
        sems,
        def_sites,
        reassigns,
        attr: None,
        scope_sep: ".",
        // No declared return types, and no operator field on `list`.
        return_type_field: "",
        bool_op_field: "",
        types_declared: false,
        refine,
        name_node: |_| None,
        composed_name: |_, _| None,
        imports,
        // A shell function's parameters are `$1`, `$2` — read from the
        // caller's frame, declared nowhere.
        param_info: |_, _| None,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &[],
        is_public,
        unit_docs,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        negation_operand: |node, _| {
            (node.kind() == "negated_command").then(|| node.named_child(0))?
        },
        catch_sin: |_, _| None,
        // `cmd || true` is explicit silencing, which the Zen exempts.
        swallows_error: |_, _| false,
        loses_context: |_, _| false,
        // `exit 1` is ordinary control flow in a script, not a panic.
        panicky: |_, _| false,
        record_keys: |_, _| None,
        // `trap ... EXIT` is the idiom; recognizing its absence needs
        // the trap, not the mktemp. Same deferral as Go's defer.
        unguarded_resource: |_, _| false,
        is_async: |_, _| false,
        declares_test: |_, _| false,
        // Shell has NO test-declaration form. bats and shunit2 assert
        // with the `[` builtin, which is indistinguishable from
        // ordinary control flow, so a `test_`-named function reads as
        // assertionless whatever it does — the corpus said so at
        // 100%. A shell test is recognized by its FILE and exempted
        // from production metrics; it is never judged as a declared
        // test, because nothing here can tell a real one from a helper.
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path: |p| {
            p.ends_with("_test.sh")
                || p.ends_with(".test.sh")
                || p.split('/')
                    .any(|seg| matches!(seg, "test" | "tests" | "bats"))
        },
        asserty: |call, src| command_name(call, src).is_some_and(super::assertish),
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        // A function returns an exit status; values leave through
        // streams and globals, neither of which has a width.
        return_arity: |_, _| 0,
        // No types, so no interfaces.
        interfaces: |_, _| Vec::new(),
        // bats `skip` is a runner builtin indistinguishable from a
        // command of that name; nothing declares a test to begin with.
        skips_test: |_, _| false,
        magic_exempt: &[
            // `>&2` — a file descriptor is not an unnamed constant.
            "file_redirect",
            "case_item",
            "subscript",
        ],
        assign_kinds: &["variable_assignment"],
    }
}

/// The word a command invokes: `kubectl` for `kubectl apply -f x`.
fn command_name<'a>(command: Node, src: &'a [u8]) -> Option<&'a str> {
    command
        .child_by_field_name("name")?
        .utf8_text(src)
        .ok()
        .map(str::trim)
}

/// `source lib.sh` and `. lib.sh` are the module system: the sourced
/// file's functions become this one's. Reclassifying the command as an
/// import is the same move the Zig pack makes for `@import`.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::Call if command_name(node, src).is_some_and(|n| SOURCING.contains(&n)) => Sem::Import,
        // A bare word being ASSIGNED is a value, not a name. Shell has
        // no scalar but the string, and quoting is optional, so
        // `API_KEY=abc123` is the same literal as `API_KEY="abc123"` —
        // it just parses as a `word`. Without this the credential
        // detector could only see the quoted half of the language.
        Sem::Ident if node.kind() == "word" && is_assigned_value(node) => Sem::StrLit,
        _ => sem,
    }
}

/// Is this node the value of a `NAME=value` assignment?
fn is_assigned_value(node: Node) -> bool {
    node.parent().is_some_and(|p| {
        p.kind() == "variable_assignment"
            && p.child_by_field_name("value")
                .is_some_and(|v| v.id() == node.id())
    })
}

fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let Some(target) = node
        .child_by_field_name("argument")
        .and_then(|a| a.utf8_text(src).ok())
    else {
        return Vec::new();
    };
    // Sourced paths are usually interpolated (`. "$DIR/lib.sh"`); the
    // graph resolves what it can and counts the rest as unresolved,
    // which is the honest bucket for a path assembled at run time.
    vec![super::ImportInfo {
        target: target.trim_matches(['"', '\'']).into(),
        names: Vec::new(),
    }]
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    command_name(call, src) == Some(unit_name)
}

/// Shell has no visibility keyword. The convention a sourced library
/// follows is the underscore prefix, same as Python's.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|n| !n.starts_with('_'))
}

/// Comment run directly above the definition (contiguous rows).
fn unit_docs(node: Node, _src: &[u8]) -> u32 {
    let mut lines = 0;
    let mut prev = node.prev_named_sibling();
    let mut expected_row = node.start_position().row;
    while let Some(p) = prev {
        if p.kind() != "comment" || p.end_position().row + 1 != expected_row {
            break;
        }
        lines += 1;
        expected_row = p.start_position().row;
        prev = p.prev_named_sibling();
    }
    lines
}

/// `eval` is the shell's own name for the gap between the text and the
/// run: whatever the string holds becomes code.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call && command_name(node, src) == Some("eval")
}
