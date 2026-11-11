//! Every fact of a scan as rows, for a reader that compares two scans.
//!
//! A report says how many units broke a budget. It cannot say WHICH call
//! a change of the parser gave back and which one it took away, because
//! a call reaches no field of the report: the extractor keeps the name of
//! a callee only while it walks the file, and the metrics that survive
//! are counts.
//!
//! So a change to the parser, to a language pack or to the seed of a C++
//! project moves a number in the report and says nothing about what
//! moved. THE COUNT IS NOT THE MEASUREMENT. Two scans that differ by one
//! parser can hold the same number of calls and name different ones.
//!
//! This dump writes one row for each unit, each call and each control
//! event of a scan, as TSV, sorted by the reader. A diff of the rows of
//! two scans names the sites.
//!
//! IT IS AN INSTRUMENT AND NOT A REPORT. `--errors FILE` is its
//! neighbor: both serve the person who develops a language pack, and
//! neither belongs in a pipeline.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::facts::FileFacts;

/// The open file of the dump. A scan writes it from every rayon worker,
/// so the rows of two files interleave and the reader sorts them.
static SINK: OnceLock<Mutex<std::io::BufWriter<std::fs::File>>> = OnceLock::new();

/// Whether a dump is open. The walk asks this at each call node, so the
/// question is an atomic load and not a lock.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Open the dump, one time, before any file is read.
pub fn open(path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("cannot write the fact dump {}: {e}", path.display()))?;
    if SINK.set(Mutex::new(std::io::BufWriter::new(file))).is_err() {
        return Err("the fact dump is open".to_owned());
    }
    ARMED.store(true, Ordering::Release);
    Ok(())
}

/// Whether a dump is open, for a walk that would build a row.
pub fn armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

/// Write one row. The lock is held for one row, and a scan with no dump
/// never reaches this function.
fn row(text: &str) {
    if let Some(sink) = SINK.get()
        && let Ok(mut out) = sink.lock()
    {
        let _ = out.write_all(text.as_bytes());
    }
}

/// One call, at the moment the walk reads it.
///
/// A call is written here and not at the end of the file, because the
/// extractor keeps no call once it has counted it. `-` stands for a
/// callee whose name the pack cannot read.
pub fn call(path: &Path, unit: &str, line: u32, callee: &str) {
    row(&format!("call\t{}\t{unit}\t{line}\t{callee}\n", path.display()));
}

/// The hash of a tree: the kind and the byte range of each named node.
///
/// THE ROWS OF A FILE ARE A FUNCTION OF ITS TREE, so the dump names the
/// tree. Without it, two scans whose rows agree say nothing about
/// whether the two parsers agreed: a change of the tree that no fact
/// reads is invisible, and it reads exactly like a parser that never
/// took the change. O(n) in the nodes of the tree.
pub fn tree_hash(tree: &tree_sitter::Tree) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |value: u64| {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    };
    let mut cursor = tree.walk();
    let mut down = true;
    loop {
        if down {
            let node = cursor.node();
            mix(u64::from(node.kind_id()));
            mix(node.start_byte() as u64);
            mix(node.end_byte() as u64);
        }
        if down && cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            down = true;
            continue;
        }
        if !cursor.goto_parent() {
            return hash;
        }
        down = false;
    }
}

/// Every fact of one file, at the end of its walk.
///
/// A row of the class `fact` is one field of one unit, and a row of the
/// class `ffact` is one field of the file. A field whose value is zero or
/// empty writes no row, because a row for each field of each unit of a
/// large project is mostly zeros and a diff of them says nothing.
///
/// THE DUMP IS COMPLETE OR IT IS A BLIND ZERO. A dump of three classes
/// reported no change on three projects where 791 files read a different
/// tree, and the report of the run moved two violations at the same time.
/// A field that the dump leaves out cannot be seen to move.
pub fn file(facts: &FileFacts, tree: u64) {
    if !armed() {
        return;
    }
    let path = facts.path.display().to_string();
    let mut text = String::with_capacity(facts.units.len() * 256);
    text.push_str(&format!(
        "file\t{path}\t{}\t{}\t{}\t{tree:016x}\n",
        facts.lines, facts.parse_errors, facts.units.len()
    ));
    let mut ffact = |field: &str, value: usize| {
        if value != 0 {
            text.push_str(&format!("ffact\t{path}\t{field}\t{value}\n"));
        }
    };
    ffact("mass", facts.mass as usize);
    ffact("comment_lines", facts.comment_lines as usize);
    ffact("blank_lines", facts.blank_lines as usize);
    ffact("too_deep", usize::from(facts.too_deep));
    ffact("clone_sites", facts.clone_sites.len());
    ffact("echo_comments", facts.echo_comments.len());
    ffact("suppressions", facts.suppressions.len());
    ffact("secrets", facts.secrets.len());
    ffact("debt_markers", facts.debt_markers.len());
    ffact("commented_code", facts.commented_code.len());
    ffact("skipped_tests", facts.skipped_tests.len());
    ffact("magic_strings", facts.magic_strings.len());
    ffact("sql_built", facts.sql_built.len());
    ffact("shelled_out", facts.shelled_out.len());
    ffact("spooky_lines", facts.spooky_lines.len());
    ffact("test_refs", facts.test_refs.len());
    ffact("switch_sigs", facts.switch_sigs.len());
    ffact("record_shapes", facts.record_shapes.len());
    ffact("imports", facts.imports.len());
    ffact("exports", facts.exports.len());
    ffact("receiver_units", facts.receiver_units.len());
    ffact("mentioned", facts.mentioned.len());
    ffact("step_refs", facts.step_refs.0 as usize + facts.step_refs.1 as usize);
    ffact("pub_order", facts.pub_order.0 as usize + facts.pub_order.1 as usize);
    ffact("classes", facts.classes.len());
    ffact("interfaces", facts.interfaces.len());
    ffact("comments", facts.comments.len());
    for unit in &facts.units {
        let name = &unit.qualname;
        text.push_str(&format!("unit\t{path}\t{name}\t{}\t{}\n", unit.line, unit.lines));
        for event in &unit.ctrl {
            text.push_str(&format!(
                "ctrl\t{path}\t{name}\t{}\t{:?}\t{}\n",
                event.line, event.sem, event.cog_depth
            ));
        }
        let mut fact = |field: &str, value: usize| {
            if value != 0 {
                text.push_str(&format!("fact\t{path}\t{name}\t{field}\t{value}\n"));
            }
        };
        fact("is_method", usize::from(unit.is_method));
        fact("is_public", usize::from(unit.is_public));
        fact("params", unit.params.len());
        fact("doc_lines", unit.doc_lines as usize);
        fact("documented_params", unit.documented_params.len());
        fact("body", unit.body as usize);
        fact("is_override", usize::from(unit.is_override));
        fact("max_vis_depth", unit.max_vis_depth as usize);
        fact("max_expr_depth", unit.max_expr_depth as usize);
        fact("magic_numbers", unit.magic_numbers as usize);
        fact("max_live_span", unit.max_live_span as usize);
        fact("repurposed", unit.repurposed as usize);
        fact("demeter", unit.demeter as usize);
        fact("negations", unit.negations as usize);
        fact("is_passthrough", usize::from(unit.is_passthrough));
        fact("self_accesses", unit.self_accesses as usize);
        fact("own_members", unit.own_members.len());
        fact("envy_count", unit.envy_count as usize);
        fact("swallowed", unit.swallowed as usize);
        fact("broad_catch", unit.broad_catch as usize);
        fact("lost_context", unit.lost_context as usize);
        fact("unwraps", unit.unwraps as usize);
        fact("casts", unit.casts as usize);
        fact("is_async", usize::from(unit.is_async));
        fact("blocking_calls", unit.blocking_calls as usize);
        fact("bool_traps", unit.bool_traps as usize);
        fact("sleep_calls", unit.sleep_calls as usize);
        fact("unawaited", unit.unawaited as usize);
        fact("fingerprints", unit.fingerprints.len());
        fact("max_loop_depth", unit.max_loop_depth as usize);
        fact("allocs_in_loop", unit.allocs_in_loop as usize);
        fact("conditional_hooks", unit.conditional_hooks as usize);
        fact("wildcard_matches", unit.wildcard_matches as usize);
        fact("dropped_tasks", unit.dropped_tasks as usize);
        fact("is_test", usize::from(unit.is_test));
        fact("named_test", usize::from(unit.named_test));
        fact("assert_calls", unit.assert_calls as usize);
        fact("vacuous_asserts", unit.vacuous_asserts as usize);
        fact("return_arity", unit.return_arity as usize);
        fact("mut_receiver", usize::from(unit.mut_receiver));
        fact("self_recursive", usize::from(unit.self_recursive));
        for (field, value) in [
            ("max_live_var", &unit.max_live_var),
            ("envy_object", &unit.envy_object),
            ("returns", &unit.returns),
            ("receiver_name", &unit.receiver_name),
            ("name", &unit.name),
        ] {
            if !value.is_empty() {
                text.push_str(&format!("fact\t{path}\t{name}\t{field}\t{value}\n"));
            }
        }
    }
    row(&text);
}

/// Write what is left in the buffer, at the end of the run.
///
/// A buffered writer that nothing flushes loses its last rows, and a
/// dump that is short by its last file reads as a scan that found less.
pub fn close() {
    if let Some(sink) = SINK.get()
        && let Ok(mut out) = sink.lock()
    {
        let _ = out.flush();
    }
}
