//! OCaml: the calibration reference for the bar this tool is named for.
//!
//! An ML-family language with exhaustive matching, no nulls and no
//! exceptions in the happy path is the closest thing to a control group
//! for "what does code look like when the type system is doing the
//! work", the question the type-hygiene and wildcard-match metrics ask
//! of everyone else. Jane Street's Base is in the gold corpus for that.

use tree_sitter::Node;

use super::{Lang, Pack, ParamInfo, sem_table};
use crate::sem::Sem;

const KINDS: &[(&str, Sem)] = &[
    // A let binding is a definition whether it names a function or a
    // value; `open_unit` measures the ones with parameters.
    ("let_binding", Sem::FnDef),
    ("fun_expression", Sem::Lambda),
    ("type_definition", Sem::TypeDef),
    ("module_definition", Sem::TypeDef),
    ("if_expression", Sem::If),
    // `&&`/`||` are infix applications; refine keeps only those two.
    ("infix_expression", Sem::BoolOp),
    ("else_clause", Sem::Else),
    ("for_expression", Sem::Loop),
    ("while_expression", Sem::Loop),
    ("match_expression", Sem::Match),
    ("match_case", Sem::CaseArm),
    ("try_expression", Sem::Try),
    ("application_expression", Sem::Call),
    ("comment", Sem::Comment),
    // A module reference is a dependency however it is spelled, and
    // `module_path` is the node every spelling shares: `open Stdune`,
    // `include Array_intf.Definitions`, `module M = Path`, `Path.t`.
    // The corpus holds 2317 `open`s against 78747 module references,
    // and resolving only the opens leaves 2242 of its 2444 files with
    // no dependent.
    ("module_path", Sem::Import),
    // A type or module-type path carries the extended form, which also
    // admits functor application: `Foo.t`, `F(X).t`, `Foo.S`.
    ("extended_module_path", Sem::Import),
    ("value_name", Sem::Ident),
    ("type_constructor", Sem::Ident),
    ("constructor_name", Sem::Ident),
    ("module_name", Sem::Ident),
    ("number", Sem::NumLit),
    ("string", Sem::StrLit),
    ("character", Sem::StrLit),
    ("boolean", Sem::BoolLit),
];

const DEF_SITES: &[(&str, &str)] = &[];

/// The grammar an OCaml file is parsed with. A `.mli` is a SIGNATURE,
/// and tree-sitter-ocaml ships a separate grammar for it: parsing one
/// with the implementation grammar leaves 140 of 300 gold `.mli` files
/// carrying parse errors against 0 of 300 `.ml`, and
/// dune/otherlibs/stdune/src/path.mli alone yields 58 ERROR nodes where
/// path.ml yields none. Every fact read off such an interface is
/// unreliable, not merely its imports.
pub(crate) fn grammar(path: &std::path::Path) -> tree_sitter::Language {
    match path.extension().is_some_and(|e| e == "mli") {
        true => tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
        false => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
    }
}

pub fn pack() -> Pack {
    built(tree_sitter_ocaml::LANGUAGE_OCAML.into())
}

/// The pack for a `.mli`, whose tables are built from the INTERFACE
/// grammar.
///
/// `Pack::sems`, `def_sites` and `reassigns` are all indexed by
/// `node.kind_id()`, and tree-sitter numbers a grammar's kinds by that
/// grammar's own rule order. Tables built from the implementation
/// grammar therefore address the wrong row for every id read off an
/// interface: a `.mli` saying `open Import` supplies no edge at all,
/// while the identical `.ml` supplies one. That mis-types all 1147
/// `.mli` files in the gold corpus, imports and units and comments
/// alike, which is also what `Lang::is_sink` means when it says "the
/// interface's own references still count".
pub fn interface_pack() -> Pack {
    built(tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into())
}

fn built(ts: tree_sitter::Language) -> Pack {
    let kinds: &[&[(&str, Sem)]] = &[KINDS];
    let sems = sem_table(&ts, kinds);
    let def_sites = super::def_table(&ts, DEF_SITES);
    Pack {
        lang: Lang::OCaml,
        ts,
        kind_names: kinds,
        def_site_names: DEF_SITES,
        // A let is a fresh binding, and `:=` writes through a ref cell
        // without rebinding the name; reassignment does not exist.
        reassign_names: &[],
        attr_name: None,
        sems,
        def_sites,
        reassigns: Box::new([]),
        attr: None,
        scope_sep: ".",
        return_type_field: "",
        bool_op_field: "operator",
        call_target_fields: &["function"],
        types_declared: true,
        // Anonymous records exist, but a record literal is checked
        // against a declared type; there is no undeclared shape.
        record_keys: |_, _| None,
        // No exceptions in the happy path and no scope-guard statement:
        // resources are released by the same `let ... in` structure that
        // scopes them.
        // Async is a library (Lwt, Async), not syntax.
        is_async: |_, _| false,
        refine,
        name_node,
        composed_name: |_, _| None,
        imports,
        param_info,
        is_self_call,
        is_doc: |_| false,
        // `(** ... *)` is OCaml's documentation comment.
        doc_markers: &["(**"],
        is_public,
        doc_span,
        docs_inside_body: false,
        file_level_scope: false,
        is_override: |_, _| false,
        spooky,
        unparsed_ctrl: |_, _| Vec::new(),
        negation_operand: |_, _| None,
        catch_sin: |_, _| None,
        swallows_error: |_, _| false,
        loses_context: |_, _| false,
        // failwith/invalid_arg ARE OCaml's raise: `unwraps` is
        // declared dead here rather than counting every precondition.
        panicky: |_, _| false,
        declares_test: |_, _| false,
        names_test: |_, _| false,
        is_test_code: |_, _| false,
        test_path: |p| p.contains("/test/") || p.contains("/tests/"),
        asserty: |call, src| {
            callee_text(call, src).is_some_and(|t| super::assertish(t) || t.starts_with("[%test"))
        },
        // Hooks are a JS/TS framework idea; no analogue here.
        is_hook: |_, _| false,
        // A tuple is the language's ordinary single value and the
        // result is just the tail expression; there is no declared
        // return position to read a width from.
        return_arity: |_, _| 0,
        // The interface unit is the module signature, whose width is
        // the module's surface: already measured, and not a type's
        // method contract.
        interfaces: |_, _| Vec::new(),
        // No test-declaration form, so nothing to switch off.
        skips_test: |_, _| false,
        magic_exempt: &["type_definition"],
        assign_kinds: &[],
    }
}

/// `let f x = ...` names the binding; the pattern holds the name.
///
/// `let _ = ...` names it `_`. tree-sitter-ocaml 0.24 spelled that pattern
/// as a `value_name` and 0.26 spells it as an `any_pattern`, so both kinds
/// are read. Without the second, each such unit loses its name.
fn name_node(node: Node) -> Option<Node> {
    (node.kind() == "let_binding")
        .then(|| node.child_by_field_name("pattern"))?
        .filter(|p| matches!(p.kind(), "value_name" | "any_pattern"))
}

/// Naming a module makes it visible, so every module path is an
/// import edge: `open Core`, `include Import`, `module M = Path` and
/// the `Path` of `Path.to_string` alike.
///
/// A path nests: `A.B.C` is a path holding `A.B` holding `A`. Only the
/// outermost is the reference; the inner ones are its own prefixes and
/// would each be counted again.
///
/// A module name may be a package (`Ppxlib`), the standard library
/// (`Printf`) or this project's own, and the source does not say which,
/// so a miss is an ordinary dependency.
fn imports(node: Node, src: &[u8]) -> Vec<super::ImportInfo> {
    let parent = node.parent();
    let nested = parent.is_some_and(|p| matches!(p.kind(), "module_path" | "extended_module_path"));
    // `with module Printf := Shadow_stdlib.Printf` names Printf to
    // REBIND it; the dependency is the constraint on the right.
    let binder = parent.is_some_and(|p| {
        p.kind() == "constrain_module" && p.child_by_field_name("constraint") != Some(node)
    });
    let Ok(text) = node.utf8_text(src) else {
        return Vec::new();
    };
    let head = text.split('.').next().unwrap_or_default();
    if nested || binder || head.is_empty() || bound_here(node, head, src) {
        return Vec::new();
    }
    vec![super::ImportInfo {
        target: text.into(),
        names: Vec::new(),
        reach: super::Reach::Anywhere,
    }]
}

/// Does a module definition in scope already own this name?
///
/// base/src/string.ml opens with `module Bytes = Bytes0`, so every
/// `Bytes.create` below it means Bytes0, while base/src/bytes.ml opens
/// with `module String = String0` and means String0. Reading both as
/// references to the sibling file puts bytes.ml and string.ml in a
/// cycle, and OCaml has no such thing: a cycle between compilation
/// units does not compile. 106 pairs of files answer to each other
/// that way.
///
/// The alias itself is still an edge — `module Bytes = Bytes0` names
/// Bytes0 — so nothing is lost by declining the uses of the name it
/// binds.
fn bound_here(node: Node, name: &str, src: &[u8]) -> bool {
    let mut cur = node;
    loop {
        let mut prev = cur.prev_named_sibling();
        while let Some(item) = prev {
            if binds_module(item, name, src) {
                return true;
            }
            prev = item.prev_named_sibling();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return false,
        }
    }
}

/// `module M = ...`, `module M = struct .. end` and the parameter of a
/// functor all bind M.
fn binds_module(item: Node, name: &str, src: &[u8]) -> bool {
    if item.kind() != "module_definition" {
        return false;
    }
    fn named<'a>(n: Node<'a>) -> Vec<Node<'a>> {
        n.named_children(&mut n.walk()).collect()
    }
    named(item)
        .into_iter()
        .filter(|b| b.kind() == "module_binding")
        .flat_map(named)
        .flat_map(|c| match c.kind() {
            "module_parameter" => named(c),
            _ => vec![c],
        })
        .any(|n| n.kind() == "module_name" && n.utf8_text(src) == Ok(name))
}

/// A let binding lists its parameters as direct children, so every
/// other child of the binding is offered here too and declined.
fn param_info(node: Node, src: &[u8]) -> Option<ParamInfo> {
    if node.kind() != "parameter" {
        return None;
    }
    let name = node.utf8_text(src).ok()?;
    // Labelled arguments carry their name with a leading `~` or `?`;
    // the `?` ones are optional at every call site.
    let bare = name.trim_start_matches(['~', '?']);
    // `let f (x, y) = ...` and `let g {a; b} = ...` bind by shape. A
    // `value_pattern` is the only child kind that binds ONE name, so
    // anything else — tuple, record, constructor, and the unit `()`
    // which binds none — is a pattern.
    let bound =
        node.child_by_field_name("pattern")
            .map(|p| match p.kind() == "parenthesized_pattern" {
                true => p.named_child(0).unwrap_or(p),
                false => p,
            });
    (!bare.is_empty()).then(|| ParamInfo {
        name: bare.into(),
        optional: name.starts_with('?'),
        typed: false,
        destructured: bound.is_some_and(|p| p.kind() != "value_pattern"),
        ..Default::default()
    })
}

fn is_self_call(call: Node, src: &[u8], unit_name: &str) -> bool {
    callee_text(call, src) == Some(unit_name)
}

/// The leftmost term of an application is what is being called.
fn callee_text<'a>(call: Node, src: &'a [u8]) -> Option<&'a str> {
    call.named_child(0)?.utf8_text(src).ok()
}

/// `Obj` is the hole in the type system, and the manual says so: the
/// module's own documentation opens with a warning that using it is
/// "not type-safe" and may crash the program. `Obj.magic` is Rust's
/// `transmute` in a different alphabet, and `repr`/`obj` are the same
/// erasure written as a pair.
///
/// Matched on the qualified NAME rather than on an application,
/// because the idiom is `Obj.magic @@ f x` at least as often as
/// `Obj.magic (f x)`: the first is an application of `@@`, and the
/// hatch appears there as a bare path. `Obj.reachable_words` and
/// `Obj.size` measure a value without reinterpreting it and are left
/// alone.
fn spooky(node: Node, _sem: Sem, src: &[u8]) -> bool {
    node.kind() == "value_path"
        && matches!(
            node.utf8_text(src),
            Ok("Obj.magic" | "Obj.repr" | "Obj.obj" | "Obj.field" | "Obj.set_field")
        )
}

/// Without an interface file the whole module is surface. A binding
/// prefixed with `_` is conventionally internal.
fn is_public(node: Node, src: &[u8]) -> bool {
    node.child_by_field_name("pattern")
        .and_then(|p| p.utf8_text(src).ok())
        .is_some_and(|n| !n.starts_with('_'))
}

/// `(** ... *)` immediately above the binding.
fn doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)> {
    // A value_definition wraps the binding, so look outward once.
    let carrier = match node.prev_named_sibling() {
        Some(_) => node,
        None => node.parent()?,
    };
    super::doc_run(carrier, &["comment"], &["(**"], src)
}

/// `else if` nests an if inside an else clause, as in Rust and
/// TypeScript, and flattens the same way. `&&`/`||` share the infix
/// kind with every arithmetic and comparison operator, so only those
/// two count as boolean sequences.
fn refine(node: Node, src: &[u8], sem: Sem) -> Sem {
    match sem {
        Sem::If if node.parent().is_some_and(|p| p.kind() == "else_clause") => Sem::ElseIf,
        Sem::Else
            if node
                .named_child(0)
                .is_some_and(|c| c.kind() == "if_expression") =>
        {
            Sem::None
        }
        Sem::BoolOp => match super::field_text_is(node, "operator", src) {
            Some("&&" | "||") => Sem::BoolOp,
            _ => Sem::None,
        },
        _ => sem,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Every syntax that names a module is an edge, and each names it
    /// once. Reading `open` alone finds 2317 references in the gold
    /// corpus where the module paths find 78747, and leaves 2242 of its
    /// 2444 files with no dependent at all.
    #[test]
    fn every_module_reference_is_an_import_and_is_counted_once() {
        const SRC: &str = r#"
open! Import
include Array_intf.Definitions
module P = Stdune.Path
let f (x : Path.Build.t) = Memo.O.(String.length (Path.to_string x))
let g = Stdune__Env.get
type t = Dune_lang.Decoder.t
module type S = Foo.S
let h = Some (List.map ~f:ignore)
open struct
  let z = 1
end
"#;
        let pack = crate::lang::Lang::OCaml.pack();
        let mut parser = pack.make_parser();
        let f = crate::facts::extract(pack, &mut parser, Path::new("t.ml"), SRC);
        let got: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(
            got,
            [
                "Import",
                "Array_intf.Definitions",
                "Stdune.Path",
                "Path.Build",
                "Memo.O",
                "String",
                "Path",
                "Stdune__Env",
                "Dune_lang.Decoder",
                "Foo",
                "List",
            ],
            "a nested path is its own prefix and must not be counted again"
        );
        // `open struct .. end` names no module, and `Some` is a
        // constructor rather than a module qualifier.
        assert!(!got.contains(&"Some"));
    }

    /// A `.mli` is parsed with the INTERFACE grammar, and its tables
    /// have to come from that grammar too. `Pack::sems` is indexed by
    /// `node.kind_id()`, and tree-sitter numbers each grammar's kinds by
    /// its own rule order, so reading an interface's nodes through the
    /// implementation's table addresses the wrong rows, silently, since
    /// every id is in range. Read that way, all 1147 `.mli` files in
    /// gold supply no edges at all.
    #[test]
    fn an_interface_names_the_modules_it_references_as_an_implementation_does() {
        const SRC: &str = r#"
open! Import
module P = Stdune.Path
val f : Path.Build.t -> string
type t = Dune_lang.Decoder.t
"#;
        let iface = crate::lang::Lang::OCaml.pack_for(Path::new("t.mli"));
        let mut parser = iface.make_parser();
        let f = crate::facts::extract(iface, &mut parser, Path::new("t.mli"), SRC);
        let got: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(
            got,
            ["Import", "Stdune.Path", "Path.Build", "Dune_lang.Decoder"]
        );
        // And the selection is by PATH, not by language: a `.ml` still
        // gets the implementation pack.
        let impl_pack = crate::lang::Lang::OCaml.pack_for(Path::new("t.ml"));
        assert!(std::ptr::eq(impl_pack, crate::lang::Lang::OCaml.pack()));
        assert!(!std::ptr::eq(iface, impl_pack));
    }

    /// A top-level `let _ = ...` is a unit named `_`. The grammar spells
    /// that pattern as `any_pattern` from tree-sitter-ocaml 0.26 and as a
    /// `value_name` before it, and the name must not depend on which.
    #[test]
    fn a_wildcard_binding_is_named_by_its_pattern() {
        let pack = crate::lang::Lang::OCaml.pack();
        let mut parser = pack.make_parser();
        let f = crate::facts::extract(
            pack,
            &mut parser,
            Path::new("t.ml"),
            "let _ = print_string \"x\"\nlet f x = x + 1\n",
        );
        let names: Vec<&str> = f.units[1..].iter().map(|u| &*u.name).collect();
        assert_eq!(names, ["_", "f"]);
    }
}
