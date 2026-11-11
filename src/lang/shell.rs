//! Shell: the language that provisions the cluster.
//!
//! Deployment scripts are where a mistake costs the most and review
//! reaches least. One k8s repository here holds 22.6k lines of shell,
//! including a 3,287-line registry provisioner. This pack closes the
//! gap between "we measure nine languages" and "we measure the code
//! that runs production".
//!
//! Limits taken deliberately:
//! - A shell function declares NO parameters: `$1` and `$2` are read
//!   from the caller's frame. Every interface metric is therefore
//!   structurally silent, which is a fact about the language rather
//!   than a gap in the pack.
//! - `.sh`/`.bash` only. An extensionless script with a shebang is
//!   real and common, but `Lang::from_path` is a pure path predicate
//!   that several modes call during the walk, and sniffing every
//!   extensionless file would change what a walk costs. The deferral
//!   has a measured price: the shell gold corpus holds 29
//!   extensionless scripts carrying a shell shebang, among them the
//!   eleven `bats-core/libexec/bats-core/bats-*` files that source
//!   that project's ENTIRE library. Those hold 28 sourcing statements,
//!   24 of which resolve on sight, and they give five of
//!   `lib/bats-core/`'s libraries their first importer.
//! - A library is written at its INSTALLED name, which has lost the
//!   extension the file still carries: git spells `. git-sh-setup` and
//!   ships `git-sh-setup.sh`. 15 of the corpus's 68 sourcing statements
//!   are that shape and none of them resolve, because the basename
//!   index is keyed by the file name including its extension.
//! - `cmd || true` is EXPLICIT silencing, which the Zen exempts, so it
//!   is not counted as a swallowed error.

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
    // `&&`/`||` and ONLY those: `;` sequencing produces no node of its
    // own and `|` is a pipeline, so this needs no refinement. A chain
    // nests (`a && b && c` is list(list(a,b),c)), so the cognitive
    // sequence-dedup works without an operator field to compare.
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
        call_target_fields: &["name"],
        types_declared: false,
        refine,
        name_node: |_| None,
        composed_name: |_, _| None,
        imports,
        // A shell function's parameters are `$1`, `$2`, read from the
        // caller's frame and declared nowhere.
        param_info: |_, _| None,
        is_self_call,
        is_doc: |_| false,
        doc_markers: &[],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        unparsed_ctrl: |_, _| Vec::new(),
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
        is_async: |_, _| false,
        declares_test: |_, _| false,
        // Shell has NO test-declaration form. bats and shunit2 assert
        // with the `[` builtin, which is indistinguishable from
        // ordinary control flow, so a `test_`-named function reads as
        // assertionless whatever it does. The corpus agrees at 100%. A
        // shell test is recognized by its FILE and exempted from
        // production metrics; it is never judged as a declared test,
        // because nothing here can tell a real one from a helper.
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path,
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
            // `>&2`: a file descriptor is not an unnamed constant.
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
        // `API_KEY=abc123` is the same literal as `API_KEY="abc123"`.
        // It just parses as a `word`. Without this the credential
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
    // `source <(grep ... file)` sources a PROCESS, not a path. The file
    // name inside the substitution is an argument to grep rather than
    // the thing being sourced, so there is nothing here to resolve. The
    // corpus spells it once, at bats-core/contrib/release.sh:23.
    if target.starts_with("<(") || target.starts_with(">(") {
        return Vec::new();
    }
    // Quoting is per-word, not per-argument: `. "$TEST_DIRECTORY"/lib.sh`
    // arrives with a quote in the MIDDLE, which trimming the ends leaves
    // in place. Two targets in the corpus are spelled that way.
    //
    // Sourced paths are usually interpolated (`. "$DIR/lib.sh"`); the
    // graph resolves what it can and counts the rest as unresolved,
    // which is the honest bucket for a path assembled at run time.
    let unquoted: String = target
        .chars()
        .filter(|c| !matches!(c, '"' | '\''))
        .collect();
    vec![super::ImportInfo {
        target: unquoted.into(),
        names: Vec::new(),
        reach: super::Reach::Anywhere,
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
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    super::doc_run(node, &["comment"], &[], src)
}

/// A test DIRECTORY, never the file's own name: `bats` names the
/// runner's entry script as well as the directory a project vendors it
/// into. Matching the last segment would file bats-core's own bin/bats
/// and libexec/bats-core/bats as tests, and those two scripts source
/// its whole library, so every edge they carry would leave the
/// production graph. `bats` therefore stays an EXACT segment; only the
/// "test" family widens.
///
/// `test.sh` is the other spelling, matched as a whole FILENAME rather
/// than a stem, so a rule about `test.sh` says nothing about
/// `unittest.sh`. All 24 files so named in gold are the entry script of
/// a suite: 19 transformer-engine CI stages, curl's cmake harness,
/// redis's and vscode's `scripts/test.sh`, phoenix's integration
/// driver, and one colorize FIXTURE already inside a `test/` tree.
fn test_path(p: &str) -> bool {
    p.ends_with("_test.sh")
        || p.ends_with(".test.sh")
        || p.rsplit_once('/')
            .is_some_and(|(_, name)| name == "test.sh")
        || p.rsplit_once('/')
            .is_some_and(|(dirs, _)| dirs.split('/').any(|seg| seg == "bats"))
        || super::test_dir(p)
}

/// `eval` is the shell's own name for the gap between the text and the
/// run: whatever the string holds becomes code.
fn spooky(node: Node, sem: Sem, src: &[u8]) -> bool {
    sem == Sem::Call && command_name(node, src) == Some("eval")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn a_test_directory_is_test_code_and_a_runner_named_bats_is_not() {
        // bats-core/bin/bats and bats-core/libexec/bats-core/bats are
        // the only two files in the shell gold corpus whose OWN NAME is
        // the sole "test" segment; the second is the script that sources
        // the whole library, so filing it as a test would delete its
        // edges from the production graph.
        let is_test = super::pack().test_path;
        for p in [
            "bats-core/test/bats.bash",
            "git/t/tests/lib.sh",
            "proj/lib/thing_test.sh",
        ] {
            assert!(is_test(p), "{p}");
        }
        for p in [
            "bats-core/bin/bats",
            "bats-core/libexec/bats-core/bats",
            "bats-core/lib/bats-core/common.bash",
            "git/ci/lib.sh",
        ] {
            assert!(!is_test(p), "{p}");
        }
    }

    #[test]
    fn a_suite_directory_is_named_freely_and_a_product_is_not() {
        // swift-nio's IntegrationTests/run-tests.sh:102-108 globs
        // `for f in tests_*` then `for t in test_*.sh`, and
        // transformer-engine files 21 CI stages as
        // `qa/L0_pytorch_unittest/test.sh`. 45 shell orphans across
        // cuda, swift and java were one of those two shapes.
        let is_test = super::pack().test_path;
        for p in [
            "swift-nio/IntegrationTests/tests_01_http/run.sh",
            "transformer-engine/qa/L0_pytorch_unittest/test.sh",
            "bats-core/blackbox-tests/run.sh",
            "vscode/extensions/vscode-colorize-tests/colorize.sh",
            "netty/testsuite-native-image/x.sh",
            "flysystem/test_files/gen.sh",
        ] {
            assert!(is_test(p), "{p}");
        }
        // `test` BETWEEN two hyphenated words qualifies a product name
        // rather than saying what the directory holds:
        // vscode-test-resolver is a shipped extension whose
        // package.json declares `"main": "./out/extension"`, and it is
        // the only non-suite among the 29 test-bearing directories in
        // gold that carry a shell or PHP file.
        for p in [
            "vscode/extensions/vscode-test-resolver/scripts/terminateProcess.sh",
            "bats-core/lib/bats-core/common.bash",
            "curl/scripts/latest.sh",
        ] {
            assert!(!is_test(p), "{p}");
        }
    }

    #[test]
    fn a_process_substitution_is_not_a_sourced_file() {
        // bats-core/contrib/release.sh:23 sources the OUTPUT of a grep;
        // the path inside belongs to grep, not to the shell.
        let pack = super::Lang::Shell.pack();
        let mut parser = pack.make_parser();
        let src = "source <(grep '^export V=' libexec/bats-core/bats)\n\
                   . \"$TEST_DIRECTORY\"/test-lib.sh\n";
        let facts = crate::facts::extract(pack, &mut parser, Path::new("t.sh"), src);
        let got: Vec<&str> = facts.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(got, ["$TEST_DIRECTORY/test-lib.sh"]);
    }
}
