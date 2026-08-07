//! Language packs: everything language-specific lives behind this boundary.
//! A pack is a dense `kind_id -> Sem` table plus a few small hooks for the
//! irreducibly syntactic bits (context refinement, parameter shapes,
//! self-call detection). Packs classify syntax; only the core assigns meaning.

mod c;
mod cpp;
mod csharp;
mod elixir;
/// A module name written the way Elixir writes it as a path — the
/// bridge the graph needs to match `Plug.Conn` against `plug/conn.ex`.
pub(crate) use elixir::underscore;
/// Lua's preloaded library names, which consult no file.
pub(crate) use lua::preloaded;
mod go;
pub(crate) mod hooks;
mod java;
mod js;
mod lua;
mod ocaml;
mod perl;
mod php;
mod python;
mod ruby;
mod rust;
mod scala;
mod shell;
mod solidity;
mod swift;
mod typescript;
mod zig;

use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::{Language, Node, Parser};

use crate::sem::Sem;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Python,
    Rust,
    TypeScript,
    Tsx,
    Go,
    JavaScript,
    Zig,
    Lua,
    Ruby,
    Perl,
    Php,
    Java,
    CSharp,
    Swift,
    Scala,
    Elixir,
    Solidity,
    C,
    OCaml,
    Shell,
    Cpp,
    Cuda,
}

pub const LANGS: [Lang; 22] = [
    Lang::Python,
    Lang::Rust,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::Go,
    Lang::JavaScript,
    Lang::Zig,
    Lang::Lua,
    Lang::Ruby,
    Lang::Perl,
    Lang::Php,
    Lang::Java,
    Lang::CSharp,
    Lang::Swift,
    Lang::Scala,
    Lang::Elixir,
    Lang::Solidity,
    Lang::C,
    Lang::OCaml,
    Lang::Shell,
    Lang::Cpp,
    Lang::Cuda,
];

/// Everything that varies per language, in one place.
///
/// `from_path`, `name`, `corpus_dir` and `pack` were four parallel
/// `match self` arms, and a language added to three of them and
/// forgotten in the fourth still compiles. That drift happened twice:
/// `corpus_dir` silently emptied the `[rs]` and `[ml]` calibration
/// sections, and later did the same to `[sol]`. One table cannot drift
/// against itself.
struct Desc {
    lang: Lang,
    /// What the report and calibration.toml call it.
    name: &'static str,
    /// What gold.toml calls it, and the corpus directory a repository
    /// fetched for it lands in. Mostly `name` and deliberately not
    /// always — the manifest is read by people and spells several out.
    corpus: &'static str,
    exts: &'static [&'static str],
    make: fn() -> Pack,
}

/// Indexed by `Lang as usize`, so the order is the enum's order. The
/// test below pins that.
const DESCS: [Desc; LANGS.len()] = [
    Desc {
        lang: Lang::Python,
        name: "py",
        corpus: "py",
        exts: &["py"],
        make: python::pack,
    },
    Desc {
        lang: Lang::Rust,
        name: "rs",
        corpus: "rust",
        exts: &["rs"],
        make: rust::pack,
    },
    Desc {
        lang: Lang::TypeScript,
        name: "ts",
        corpus: "ts",
        exts: &["ts"],
        make: || typescript::pack(typescript::Dialect::Ts),
    },
    Desc {
        lang: Lang::Tsx,
        name: "tsx",
        corpus: "tsx",
        exts: &["tsx"],
        make: || typescript::pack(typescript::Dialect::Tsx),
    },
    Desc {
        lang: Lang::Go,
        name: "go",
        corpus: "go",
        exts: &["go"],
        make: go::pack,
    },
    Desc {
        lang: Lang::JavaScript,
        name: "js",
        corpus: "js",
        exts: &["js", "mjs", "cjs", "jsx"],
        make: js::pack,
    },
    Desc {
        lang: Lang::Zig,
        name: "zig",
        corpus: "zig",
        exts: &["zig"],
        make: zig::pack,
    },
    Desc {
        lang: Lang::Lua,
        name: "lua",
        corpus: "lua",
        exts: &["lua"],
        make: lua::pack,
    },
    Desc {
        lang: Lang::Ruby,
        name: "rb",
        corpus: "ruby",
        exts: &["rb", "rake", "gemspec"],
        make: ruby::pack,
    },
    Desc {
        lang: Lang::Perl,
        name: "pl",
        corpus: "perl",
        exts: &["pl", "pm", "t"],
        make: perl::pack,
    },
    Desc {
        lang: Lang::Php,
        name: "php",
        corpus: "php",
        exts: &["php"],
        make: php::pack,
    },
    Desc {
        lang: Lang::Java,
        name: "java",
        corpus: "java",
        exts: &["java"],
        make: java::pack,
    },
    Desc {
        lang: Lang::CSharp,
        name: "cs",
        corpus: "csharp",
        exts: &["cs"],
        make: csharp::pack,
    },
    Desc {
        lang: Lang::Swift,
        name: "swift",
        corpus: "swift",
        exts: &["swift"],
        make: swift::pack,
    },
    Desc {
        lang: Lang::Scala,
        name: "scala",
        corpus: "scala",
        exts: &["scala", "sc"],
        make: scala::pack,
    },
    Desc {
        lang: Lang::Elixir,
        name: "ex",
        corpus: "elixir",
        exts: &["ex", "exs"],
        make: elixir::pack,
    },
    Desc {
        lang: Lang::Solidity,
        name: "sol",
        corpus: "solidity",
        exts: &["sol"],
        make: solidity::pack,
    },
    // `.h` reads as C here; `of_source` decides it, because the
    // extension genuinely does not.
    Desc {
        lang: Lang::C,
        name: "c",
        corpus: "c",
        exts: &["c", "h"],
        make: c::pack,
    },
    Desc {
        lang: Lang::OCaml,
        name: "ml",
        corpus: "ocaml",
        exts: &["ml", "mli"],
        make: ocaml::pack,
    },
    // Extension only: an extensionless script with a shebang is real,
    // but this is a pure path predicate the walk calls on every file,
    // and sniffing would change what a walk costs.
    Desc {
        lang: Lang::Shell,
        name: "sh",
        corpus: "shell",
        exts: &["sh", "bash"],
        make: shell::pack,
    },
    Desc {
        lang: Lang::Cpp,
        name: "cpp",
        corpus: "cpp",
        exts: &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
        make: || cpp::pack(cpp::Dialect::Cpp),
    },
    // CUDA is C++ plus a launch operator, and the pack says so; the
    // extensions stay separate because the budgets do.
    Desc {
        lang: Lang::Cuda,
        name: "cu",
        corpus: "cuda",
        exts: &["cu", "cuh"],
        make: || cpp::pack(cpp::Dialect::Cuda),
    },
];

impl Lang {
    pub fn from_path(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_str()?;
        DESCS.iter().find(|d| d.exts.contains(&ext)).map(|d| d.lang)
    }

    /// The language a file is MEASURED as. `.h` is the one extension in
    /// this tool that underdetermines its language, and reading it as C
    /// unconditionally was measurably wrong: of leveldb's 56 headers 47
    /// failed to parse as C and 17 as C++, re2's 20 against 1, fmt's 23
    /// against 11. Headers are where C++ keeps its classes, so a third
    /// of every C++ repository was being dropped as unreadable.
    ///
    /// The grammar alone does not settle it, because the C++ grammar is
    /// never worse on C headers either (musl 14% against 15%, redis 3%
    /// against 2%, curl 9% against 7%) — so routing every `.h` to C++
    /// would parse fine and then file musl's 655 headers under `cpp`,
    /// which is the corpus trap gold.toml warns about. The LABEL has to
    /// be decided too, and only the text can decide it.
    ///
    /// The rule is four line-anchored spellings that are not C. It was
    /// validated before it was written: 0 of 1,027 headers from lua,
    /// musl, redis and curl match, and 98 of 104 from fmt, leveldb and
    /// re2 do. The six that do not are `c.h` (leveldb's C API),
    /// `export.h`, `port.h` and `thread_annotations.h` — headers that
    /// hold no C++ at all, so reading them as C is the right answer
    /// rather than a missed one.
    /// CUDA is decided the same way and for the same reason: a launch is
    /// not valid C++, so a `.h` full of `__global__` is handed to the C++
    /// grammar and dies on the first `<<<`. flash-attention keeps its
    /// launch templates in `flash_fwd_launch_template.h` and cutlass
    /// keeps 76 headers that way.
    pub fn of_source(path: &Path, source: &str) -> Option<Lang> {
        match Lang::from_path(path)? {
            Lang::C | Lang::Cpp if looks_like_cuda(source) => Some(Lang::Cuda),
            Lang::C if path.extension()? == "h" && looks_like_cpp(source) => Some(Lang::Cpp),
            lang => Some(lang),
        }
    }

    /// `of_source` for a caller holding only a path — it reads the file.
    /// Used where files are PARTITIONED by language before scanning, so
    /// that a C++ header cannot land in the `[c]` calibration section.
    pub fn of(path: &Path) -> Option<Lang> {
        match Lang::from_path(path)? {
            // Every C-family extension is now readable: `.h` may be any
            // of the three, and `.cpp`/`.hpp` may be CUDA.
            lang @ (Lang::C | Lang::Cpp) => match std::fs::read_to_string(path) {
                Ok(source) => Lang::of_source(path, &source),
                Err(_) => Some(lang),
            },
            lang => Some(lang),
        }
    }

    /// Is this file a SINK in the dependency graph by construction?
    ///
    /// Nothing `#include`s a `.c`. A translation unit's fan-in is zero
    /// in every C-family repository ever written — kakoune 57 of 58,
    /// curl 377 of 377, musl 1566 of 1608 — so "nothing depends on
    /// this" states a fact about the language rather than about the
    /// code. Which translation unit needs which is settled by the
    /// LINK graph, and no `#include` expresses it.
    ///
    /// So they are left out of the population the dependency metrics
    /// judge, and the reported rate becomes the header rate, which is
    /// the one that carries information. Their own includes still
    /// count: kakoune's buffer.cc is what gives buffer.hh its 13
    /// importers.
    ///
    /// An OCaml `.mli` is the same module as its `.ml`, not a second
    /// one: a reference names `Path`, and `path.ml` is what answers.
    /// The gold corpus holds 1147 such pairs among 2444 files, so
    /// counting the interface separately put 1147 modules in the
    /// orphan list by construction — no import could ever reach them.
    /// The implementation is the module that is judged, and the
    /// interface's own references still count.
    pub fn is_sink(self, path: &Path) -> bool {
        let ext = path.extension().and_then(|e| e.to_str());
        match self {
            Lang::C | Lang::Cpp | Lang::Cuda => {
                matches!(ext, Some("c" | "cc" | "cpp" | "cxx" | "c++" | "cu"))
            }
            Lang::OCaml => ext == Some("mli"),
            _ => false,
        }
    }

    pub fn name(self) -> &'static str {
        DESCS[self as usize].name
    }

    /// The name gold.toml uses for this language, which is also the
    /// corpus directory a repository fetched for it lands in. Mostly
    /// `name()` and deliberately not always — the manifest is read by
    /// people and spells three of them out. Matching on `name()`
    /// instead silently emptied the `[rs]` and `[ml]` sections, since
    /// no directory is called `rs` or `ml`.
    pub fn corpus_dir(self) -> &'static str {
        DESCS[self as usize].corpus
    }

    pub fn pack(self) -> &'static Pack {
        static PACKS: [OnceLock<Pack>; LANGS.len()] = [const { OnceLock::new() }; LANGS.len()];
        PACKS[self as usize].get_or_init(|| (DESCS[self as usize].make)())
    }
}

/// How a catch clause mishandles errors. Zen of Python: "Errors should
/// never pass silently. Unless explicitly silenced."
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatchSin {
    /// Handler body is only pass/ellipsis/empty — the error vanishes.
    Swallowed,
    /// Catches Exception/BaseException or binds nothing — too broad.
    Broad,
}

/// One import edge as written in source: where it points and which
/// local names it binds (bound names are modules in disguise — they
/// must not read as envied objects or unnamed magic).
/// How far a specifier can possibly reach.
///
/// The distinction decides what a MISS means, and three resolvers here
/// already act on it: a Python relative import, a quoted C include and a
/// shell `source` all report `Unresolved` when they find nothing, while
/// an absolute Python import and an angled include report `External`.
/// The generic arm could not express it, so ten languages could only
/// ever answer Internal or External — and a corpus that resolved nothing
/// reported itself 100% resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Reach {
    /// A package name. A miss is an ordinary third-party dependency.
    #[default]
    Anywhere,
    /// A path, or a member of a namespace this project declares. It
    /// names something that is supposed to be HERE, so a miss is a
    /// failure to resolve and belongs in the honesty bucket.
    Project,
}

#[derive(Default)]
pub struct ImportInfo {
    /// Normalized target: `a.b`, `..pkg.x`, `./util`, `crate::x::y`,
    /// `<stdio.h>` (angle brackets preserved: definitionally external).
    pub target: Box<str>,
    /// Local names this import introduces.
    pub names: Vec<Box<str>>,
    /// Whether a miss is a dependency or a failure. See `Reach`.
    pub reach: Reach,
}

/// The field names of an anonymous record, when a node is one.
pub type RecordKeys = fn(Node, &[u8]) -> Option<Vec<Box<str>>>;

/// The byte range of a definition's documentation, when it has any.
pub type DocSpan = fn(Node, &[u8]) -> Option<(u32, u32)>;

/// What a parameter contributes to interface metrics.
#[derive(Clone, Default)]
pub struct ParamInfo {
    /// Binding name (pattern text for destructuring params).
    pub name: Box<str>,
    /// Boolean-typed or boolean-defaulted — a flag parameter candidate.
    pub boolish: bool,
    /// Receiver (`self`/`cls`) — excluded from parameter counts on methods.
    pub selfish: bool,
    /// Receiver taken mutably (`&mut self`).
    pub mut_receiver: bool,
    /// Mutable default value (Python `def f(x=[])`) — shared across calls.
    pub mutable_default: bool,
    /// `**kwargs`-style splat — an interface that reveals nothing.
    pub kw_splat: bool,
    /// Optional at the call site (default value, `?`, splat): adding
    /// one to a signature breaks no caller.
    pub optional: bool,
    /// Carries a declared type. In a gradually-typed language an untyped
    /// parameter is a promise the compiler cannot keep for you.
    pub typed: bool,
    /// The declared type is an escape hatch (`Any`, `any`, `interface{}`,
    /// `void *`, `anytype`): annotated, but asserting nothing.
    pub loose: bool,
    /// Declared type as written, empty when absent. Adjacent parameters
    /// of the SAME type are silently swappable at every call site.
    pub type_name: Box<str>,
    /// Accepts arguments it does not NAME: `*args`, `...rest`,
    /// `params object[]`, `Int*`.
    ///
    /// The mirror of `destructured`. A splat is one binding standing
    /// for any number of arguments, so documentation that names them
    /// individually — `Args: inst:` against `def evolve(*args,
    /// **changes)` — is describing the interface correctly while
    /// naming nothing the signature declares.
    pub splat: bool,
    /// A PATTERN rather than a name: `{ limitLength, headerName }`,
    /// `(a, b)`, `%User{id: id}`.
    ///
    /// The signature binds several names — or none — where the
    /// documentation names one thing, and which of the two the author
    /// meant is undecidable from syntax: JSDoc documenting `options`
    /// against a signature writing `{ limitLength, headerName }` is
    /// CORRECT, and nothing short of resolving the type says so. Any
    /// check comparing documented names against declared ones passes
    /// over the whole unit when this is set.
    pub destructured: bool,
}

/// Every hook below is PRIVATE and asked through the method of the same
/// name in [`hooks`], which records that the question was put and
/// whether the pack answered. That is the only route: a hook the core
/// never consults reads zero exactly as a hook that correctly finds
/// nothing does, and nine detectors died in that gap at once.
pub struct Pack {
    pub lang: Lang,
    pub ts: Language,
    /// Raw grammar-name tables, retained for drift validation: a
    /// tree-sitter upgrade must never silently zero a metric (the Zig
    /// @import lesson). Runtime stays lenient; tests stay merciless.
    ///
    /// A LIST of tables, because dialects of one grammar genuinely differ:
    /// `<X>expr` is a cast in .ts and ambiguous with JSX in .tsx, so the
    /// TSX grammar has no `type_assertion` node to map.
    kind_names: &'static [&'static [(&'static str, Sem)]],
    def_site_names: &'static [(&'static str, &'static str)],
    reassign_names: &'static [(&'static str, &'static str)],
    attr_name: Option<(&'static str, &'static str)>,
    sems: Box<[Sem]>,
    /// Local-binding sites resolved to kind ids: (kind, field holding the
    /// bound pattern). Fuels live-span tracking.
    def_sites: Box<[(u16, &'static str)]>,
    /// Plain-reassignment sites: (kind, field holding the target).
    /// Reassignment only, never fresh bindings — a Rust `let` shadow or
    /// a Go `:=` is a NEW binding, and judging those without scopes
    /// would flag sibling blocks. Fuels the repurposing check.
    reassigns: Box<[(u16, &'static str)]>,
    /// Member-access node: (kind id, receiver field). Fuels Demeter chains.
    attr: Option<(u16, &'static str)>,
    /// Separator for scope-qualified unit names (`.` or `::`).
    pub scope_sep: &'static str,
    /// Field holding a definition's declared return type (`return_type`,
    /// Go `result`, Zig `type`) — the name-contract metric's evidence.
    pub return_type_field: &'static str,
    /// Field holding a boolean operator's operator token (for sequence dedup).
    pub bool_op_field: &'static str,
    /// Fields that may hold a call's TARGET, in the order to try them.
    ///
    /// The core reads a callee to answer four questions — is this a
    /// sleep, does it reach a shell, does it spawn something whose
    /// handle is dropped, does it park an async unit — and it read them
    /// through a hardcoded `function`/`macro` pair. Ruby fields a call's
    /// target as `method`, Java and Lua as `name`, Elixir as `target`,
    /// so all four questions answered "no" in those languages whatever
    /// the code said. An EMPTY list means the grammar fields nothing and
    /// the target is the first named child (Swift).
    pub call_target_fields: &'static [&'static str],
    /// Does this language have type syntax at all? JavaScript does not,
    /// so "untyped" there is a fact about the language, not the code.
    pub types_declared: bool,
    /// Context-dependent classification the kind table cannot express:
    /// `else if` chains, operator disambiguation, promoting named lambdas.
    refine: fn(Node, &[u8], Sem) -> Sem,
    /// Node holding a unit's name when no `name` field exists (Zig `test`
    /// labels, C declarator chains). Consulted before the generic chain.
    name_node: for<'t> fn(Node<'t>) -> Option<Node<'t>>,
    /// A name no single node spells. gtest writes a test's identity as
    /// two macro arguments — `TEST(args_test, basic)` — and prints it
    /// back as `args_test.basic`; judging `basic` alone judges half a
    /// name and read 61% of the C++ gold corpus as lazily named, worse
    /// than a corpus of notorious code. Returns an owned String because
    /// the name is COMPOSED, which is exactly why `name_node` cannot
    /// answer. Consulted before everything else.
    composed_name: fn(Node, &[u8]) -> Option<String>,
    /// Import edges of one Import node (a `use` tree or `from` list may
    /// carry several).
    imports: fn(Node, &[u8]) -> Vec<ImportInfo>,
    /// Classify one node of a parameter list.
    param_info: fn(Node, &[u8]) -> Option<ParamInfo>,
    /// Does this call target the named enclosing unit (direct recursion)?
    is_self_call: fn(Node, &[u8], &str) -> bool,
    /// Is this statement a documentation node (e.g. Python docstring)?
    is_doc: fn(Node) -> bool,
    /// Comment prefixes this ecosystem reads as documentation rather than
    /// commentary (Sphinx `#:`, section `##`). The universal `///`,
    /// `//!` and `/**` are handled centrally.
    pub doc_markers: &'static [&'static str],
    /// Is this definition part of the public surface? Conservative: when
    /// unsure, say no — coverage findings must be precise.
    is_public: fn(Node, &[u8]) -> bool,
    /// Byte range of the contract documentation attached to this
    /// definition — a docstring, a `///` run, a JSDoc block. Interface
    /// docs, not implementation notes.
    ///
    /// The pack LOCATES and stops there: how many lines that is, how
    /// many words, and what they say are one question in every language
    /// and are answered once, in the core.
    doc_span: DocSpan,
    /// Does documentation live INSIDE the body?
    ///
    /// Python and Ruby put the docstring in the first statement, so a
    /// documented stub's body is prose and nothing else. Everywhere else
    /// a bare string in a body is a VALUE — Rust's `fn s() -> &str
    /// { "x" }` — and treating it as documentation would lose a real
    /// finding to fix a false one.
    pub docs_inside_body: bool,
    /// Does a TypeDef here scope what FOLLOWS it rather than what it
    /// encloses?
    ///
    /// Perl's `package Foo;` is a statement, and the subs it governs sit
    /// beside it rather than inside it, so an ancestor walk finds
    /// nothing. Two `base` subs under different packages in one file
    /// then share a qualified name — and the ratchet's identity is
    /// (metric, path, qualified unit), so they share a baseline entry
    /// too.
    pub file_level_scope: bool,
    /// Is this declaration an OVERRIDE POINT — a trait or interface
    /// member carrying a default for implementors, or a method marked as
    /// overriding one?
    ///
    /// Such a body is documented at length ON PURPOSE, so implementors
    /// know when to replace it. Excluding them removed all 79 of
    /// `ceremony`'s false positives on the gold corpus and cost 6 of
    /// 3,187 real hits. The extractor also applies a generic check —
    /// a member of a declaration that `interfaces` recognises — so this
    /// hook covers only what that misses: an explicit marker on a method
    /// whose enclosing type is an ordinary class.
    is_override: fn(Node, &[u8]) -> bool,
    /// Constructs where the text stops predicting the run (Dijkstra's gap):
    /// eval/exec, computed attribute access, metaclasses, transmute.
    /// Consulted for EVERY node — it once saw only calls and typedefs,
    /// and Solidity's `assembly` block and Perl's `eval "..."` were
    /// invisible for exactly as long as that lasted.
    spooky: fn(Node, Sem, &[u8]) -> bool,
    /// If this node is a logical NOT, its operand.
    negation_operand: for<'t> fn(Node<'t>, &[u8]) -> Option<Node<'t>>,
    /// Error-handling sin of a Catch node, if any.
    catch_sin: fn(Node, &[u8]) -> Option<CatchSin>,
    /// An error-check whose handler is EMPTY (`if err != nil { }`).
    /// Languages where errors are values have no Catch node to judge,
    /// and without this their whole error-discipline family reads
    /// zero. Consulted on If and Try nodes; counts into `swallowed`.
    swallows_error: fn(Node, &[u8]) -> bool,
    /// Does this handler bind the error, raise a NEW one, and never
    /// mention the original? A separate hook rather than another
    /// `CatchSin`, so the already-calibrated swallowed and broad-catch
    /// numbers do not shift under it.
    loses_context: fn(Node, &[u8]) -> bool,
    /// Call that panics instead of returning an error (`unwrap`/`expect`).
    panicky: fn(Node, &[u8]) -> bool,
    /// The keys of an anonymous record literal, if this node is one.
    /// Rust and Go build records through declared types, so their packs
    /// answer None and the metric stays silent rather than wrong.
    record_keys: RecordKeys,
    /// Is this definition async? A blocking call inside one stalls the
    /// whole executor, not just this task.
    is_async: fn(Node, &[u8]) -> bool,
    /// Does this definition DECLARE a test by context-free evidence —
    /// an attribute (`#[test]`), structure (`test('...', fn)`), or a
    /// dedicated kind (Zig `test` blocks)? Judged by the test-quality
    /// metrics wherever it appears.
    declares_test: fn(Node, &[u8]) -> bool,
    /// Does this definition's NAME follow the ecosystem's test naming
    /// convention (`test_*`, `TestXxx`)? Honored only inside test
    /// files: a production `test_connection` health check is neither a
    /// test to judge nor test code to pardon.
    names_test: fn(Node, &[u8]) -> bool,
    /// Test code by construction beyond the above (Rust `#[cfg(test)]`
    /// modules): exempt from production metrics, never judged as a test.
    is_test_code: fn(Node, &[u8]) -> bool,
    /// Is this file path a test file by ecosystem convention?
    test_path: fn(&str) -> bool,
    /// Call/macro that asserts (assert_eq!, self.assertEqual, expect).
    asserty: fn(Node, &[u8]) -> bool,
    /// A React-style hook call, whose identity is its CALL ORDER
    /// rather than its name. Reached through a branch, it renumbers
    /// every hook after it the first time the condition flips.
    is_hook: fn(Node, &[u8]) -> bool,
    /// How many values this definition makes its callers destructure:
    /// a Go result list's width, a Rust or TS tuple return type's
    /// width, the widest tuple a Python `return` ships. Languages
    /// where a compound result is already a single value (a JS array,
    /// an OCaml tuple, a Zig struct) answer 0 — there is nothing to
    /// destructure that a name would not fix.
    return_arity: fn(Node, &[u8]) -> u16,
    /// Does this node switch a test off UNCONDITIONALLY — `#[ignore]`,
    /// `it.skip(...)`, `xit(...)`, `t.Skip()`? Consulted on calls and
    /// on definitions, since languages spell it in both places. A
    /// conditional skip (`skipif`, a platform guard) is stated
    /// judgment and must never count.
    skips_test: fn(Node, &[u8]) -> bool,
    /// Declared method bundles under this TypeDef node — a Go
    /// `interface`, a Rust `trait`, a TS `interface` — each with how
    /// many methods it demands. A Vec because one Go `type (...)`
    /// block declares several. Languages whose interfaces are
    /// conventions rather than declarations answer nothing.
    interfaces: fn(Node, &[u8]) -> Vec<crate::facts::InterfaceFact>,
    /// Ancestor kinds that legitimize a numeric literal: const items,
    /// parameter defaults, indexing, types, patterns.
    pub magic_exempt: &'static [&'static str],
    /// Assignment-ish kinds whose SCREAMING_CASE binding names a constant.
    pub assign_kinds: &'static [&'static str],
}

/// Node kinds a grammar spells ONLY for a member of a type.
///
/// Whether a declaration is a method is normally read from the tree:
/// walk the ancestors and look for a TypeDef. That walk is exactly as
/// good as the parse, and a grammar that gives up inside a class body
/// hands everything after it to the enclosing scope. tree-sitter-c-sharp
/// cannot parse a `#if`-guarded `else if` between an `if` and its
/// `else`, which is how Newtonsoft.Json's JsonTextReader.cs is written;
/// the class ends at the first one and the 30 members below it read as
/// free functions, one of which — `HasLineInfo`, a documented
/// `return true` implementing IJsonLineInfo — was the whole of
/// `ceremony`'s remaining gold false positives. The same grammar parses
/// a `method_declaration` sitting directly in a namespace WITHOUT an
/// error node, so nothing downstream can tell the two apart.
///
/// A kind listed here settles the question without the tree, because
/// the language settles it: C# spells a free function
/// `local_function_statement` and it is deliberately absent below, and
/// Java has no free function at all. Languages missing from the table
/// either write one kind for both positions (Python, C++, Rust,
/// TypeScript), carry a receiver instead, which `is_method` already
/// reads (Go, Ruby), or refuse the orphan outright: PHP's
/// `method_declaration` outside a class is an ERROR node that yields no
/// unit, so there is nothing there to correct.
const MEMBER_KINDS: &[(Lang, &[&str])] = &[
    (
        Lang::CSharp,
        &[
            "method_declaration",
            "constructor_declaration",
            "destructor_declaration",
            "operator_declaration",
            "accessor_declaration",
        ],
    ),
    (
        Lang::Java,
        &["method_declaration", "constructor_declaration"],
    ),
];

/// Where a grammar spells a return type its declaration-type field
/// cannot hold.
///
/// C++11 writes `auto f() -> bool`, putting the real type AFTER the
/// parameter list, so the `type` field reads `auto` and every predicate
/// written that way looked to `lying name` like one returning something
/// that is not a boolean — fmt's `FMT_CONSTEXPR auto has_foreground()
/// const noexcept -> bool` among them. C has no trailing return type
/// (it is a C++11 construct) and every other language in the table
/// fields the whole thing, so two rows is the whole list.
const TRAILING_RETURN: &[(Lang, &str)] = &[
    (Lang::Cpp, "trailing_return_type"),
    (Lang::Cuda, "trailing_return_type"),
];

impl Pack {
    /// The type node of a trailing return, when this grammar spells one
    /// and this declaration wrote it — see [`TRAILING_RETURN`]. Follows
    /// the `declarator` chain, because a pointer or reference return
    /// wraps the function declarator that carries the arrow.
    pub fn trailing_return<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        let kind = self.trailing_return_kind()?;
        let mut decl = node.child_by_field_name("declarator")?;
        loop {
            let mut cursor = decl.walk();
            let found = decl
                .named_children(&mut cursor)
                .find(|n| n.kind() == kind)
                .and_then(|n| n.named_child(0));
            if found.is_some() {
                return found;
            }
            drop(cursor);
            decl = decl.child_by_field_name("declarator")?;
        }
    }

    fn trailing_return_kind(&self) -> Option<&'static str> {
        TRAILING_RETURN
            .iter()
            .find(|(l, _)| *l == self.lang)
            .map(|(_, k)| *k)
    }

    /// (kind id, receiver field) of member-access nodes, if declared.
    pub fn attr(&self) -> Option<(u16, &'static str)> {
        self.attr
    }

    /// Kinds this grammar uses only inside a type — see [`MEMBER_KINDS`].
    fn member_kinds(&self) -> &'static [&'static str] {
        MEMBER_KINDS
            .iter()
            .find(|(l, _)| *l == self.lang)
            .map_or(&[], |(_, kinds)| *kinds)
    }

    /// Is this declaration a type member by its SPELLING, whatever the
    /// tree around it says? False everywhere the language writes one
    /// kind for both a method and a free function.
    pub fn is_type_member(&self, node: Node) -> bool {
        self.member_kinds().contains(&node.kind())
    }

    /// The node naming a call's target, per this grammar's spelling.
    pub fn call_target<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        match self.call_target_fields.is_empty() {
            true => call.named_child(0),
            false => self
                .call_target_fields
                .iter()
                .find_map(|f| call.child_by_field_name(f)),
        }
    }

    /// Field holding the bound pattern when this node defines locals.
    pub fn def_field(&self, kind_id: u16) -> Option<&'static str> {
        self.def_sites
            .iter()
            .find(|(k, _)| *k == kind_id)
            .map(|(_, f)| *f)
    }

    /// Does a literal under this kind sit in a position that already
    /// names it — a const item, a parameter default, an index, a type,
    /// a pattern?
    pub fn exempts_literal(&self, kind: &str) -> bool {
        self.magic_exempt.contains(&kind)
    }

    /// Is this kind a binding site whose name could name a literal?
    pub fn binds_value(&self, kind: &str) -> bool {
        self.assign_kinds.contains(&kind)
    }

    /// Field holding a plain reassignment's target, if this kind is one.
    pub fn reassign_field(&self, kind_id: u16) -> Option<&'static str> {
        self.reassigns
            .iter()
            .find(|(k, _)| *k == kind_id)
            .map(|(_, f)| *f)
    }

    /// Table classification only; most callers want [`Pack::sem_of`].
    pub fn table_sem(&self, node: Node) -> Sem {
        self.sems
            .get(node.kind_id() as usize)
            .copied()
            .unwrap_or(Sem::None)
    }

    /// The table's verdict, then the pack's chance to overrule it.
    ///
    /// `refine` is the one hook whose ANSWER is a change rather than a
    /// value, so it is asked here by hand instead of through the
    /// generated wrappers in [`hooks`].
    pub fn sem_of(&self, node: Node, src: &[u8]) -> Sem {
        let table = self.table_sem(node);
        let refined = (self.refine)(node, src, table);
        hooks::note(self.lang, hooks::Hook::refine, refined != table);
        refined
    }

    pub fn make_parser(&self) -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&self.ts)
            .expect("grammar/runtime version mismatch");
        parser
    }

    /// Every declared grammar name that does NOT resolve in the compiled
    /// grammar — kinds and fields alike. Healthy packs return empty.
    pub fn unresolved(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let kind = |bad: &mut Vec<String>, name: &str| {
            if self.ts.id_for_node_kind(name, true) == 0 {
                bad.push(format!("kind {name:?}"));
            }
        };
        let field = |bad: &mut Vec<String>, name: &str| {
            if !name.is_empty() && self.ts.field_id_for_name(name).is_none() {
                bad.push(format!("field {name:?}"));
            }
        };
        for (name, _) in self.kind_names.iter().copied().flatten() {
            kind(&mut bad, name);
        }
        for (k, f) in self.def_site_names {
            kind(&mut bad, k);
            field(&mut bad, f);
        }
        for (k, f) in self.reassign_names {
            kind(&mut bad, k);
            field(&mut bad, f);
        }
        if let Some((k, f)) = self.attr_name {
            kind(&mut bad, k);
            field(&mut bad, f);
        }
        for k in self.magic_exempt {
            kind(&mut bad, k);
        }
        for k in self.assign_kinds {
            kind(&mut bad, k);
        }
        for k in self.member_kinds() {
            kind(&mut bad, k);
        }
        if let Some(k) = self.trailing_return_kind() {
            kind(&mut bad, k);
        }
        field(&mut bad, self.return_type_field);
        field(&mut bad, self.bool_op_field);
        for f in self.call_target_fields {
            field(&mut bad, f);
        }
        bad
    }
}

/// Resolve one (kind name, field) pair to a kind id.
fn attr_site(
    ts: &Language,
    kind: &'static str,
    field: &'static str,
) -> Option<(u16, &'static str)> {
    let id = ts.id_for_node_kind(kind, true);
    (id != 0).then_some((id, field))
}

/// Resolve (kind name, field) pairs to kind ids; unknown kinds drop out.
fn def_table(ts: &Language, sites: &[(&'static str, &'static str)]) -> Box<[(u16, &'static str)]> {
    sites
        .iter()
        .filter_map(|&(kind, field)| {
            let id = ts.id_for_node_kind(kind, true);
            (id != 0).then_some((id, field))
        })
        .collect()
}

/// Build the dense kind table from (kind name, Sem) pairs. Unknown names
/// are skipped so a grammar bump degrades instead of crashing — but they
/// are never harmless (an unmapped kind silently zeroes a metric), so
/// debug builds complain and [`Pack::unresolved`] fails CI.
fn sem_table(ts: &Language, kinds: &[&[(&str, Sem)]]) -> Box<[Sem]> {
    let mut sems = vec![Sem::None; ts.node_kind_count()];
    for &(name, sem) in kinds.iter().copied().flatten() {
        let id = ts.id_for_node_kind(name, true);
        if id != 0 {
            sems[id as usize] = sem;
        } else if cfg!(debug_assertions) {
            eprintln!("elegance: grammar has no node kind {name:?} — pack drift");
        }
    }
    sems.into_boxed_slice()
}

/// Every language that has async spells it the same way, at the front of
/// the declaration. Checking the head of the text rather than a field
/// keeps this working across `async fn`, `pub async fn`, `async def` and
/// `export async function` without four different node shapes.
fn declared_async(node: Node, src: &[u8]) -> bool {
    let Ok(text) = node.utf8_text(src) else {
        return false;
    };
    // Only the head matters, and it must be cut on a char boundary: a
    // multi-byte character straddling the cut panics, and one `'2✓'` in
    // a vscode string took the whole TypeScript run down.
    let head: String = text.chars().take(40).collect();
    head.split(|c: char| !c.is_alphanumeric() && c != '_')
        .take(4)
        .any(|word| word == "async")
}

/// A handler loses its context when it binds the error, throws a new
/// one, and never mentions the original: the stack that explains WHY is
/// gone, and the report says only that something failed at the top.
///
/// Shared across the languages that have exception chaining. `raiser`
/// names the kind that re-raises; the binding is the caught error's
/// name; a re-raise whose text mentions that name has kept the cause,
/// whether by `from e`, `cause: e`, or interpolation.
///
/// An OPERANDLESS raiser — C#'s `throw;`, Python's and Ruby's bare
/// `raise` — carries the caught error onward untouched, and it is the
/// one form that preserves the stack exactly. It mentions the binding
/// nowhere because it mentions NOTHING, so the text test read it as a
/// fresh error and the rule ran backwards: Microsoft's own guidance is
/// that `throw;` is right and `throw ex;` is the bug. 34 of C#'s 36
/// gold findings and 11 of Python's 20 were this.
fn rethrows_without_cause(body: Node, raiser: &str, bound: &str, src: &[u8]) -> bool {
    let mut stack = vec![body];
    let mut rethrows = false;
    while let Some(n) = stack.pop() {
        if n.kind() == raiser {
            // Exonerating the whole handler, as a cause-carrying raise
            // already does: one path that keeps the stack is the
            // evidence the author did not mean to drop it.
            if !raises_something(n) {
                return false;
            }
            let text = n.utf8_text(src).unwrap_or("");
            if mentions(text, bound) {
                return false;
            }
            rethrows = true;
            continue;
        }
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    rethrows
}

/// Does this raiser name a value to raise? A comment is a named node in
/// every tree-sitter grammar, so `throw; // rethrow` must not read as
/// `throw x`.
fn raises_something(raiser: Node) -> bool {
    let mut cursor = raiser.walk();
    raiser
        .named_children(&mut cursor)
        .any(|c| !c.kind().contains("comment"))
}

/// Whole-word containment, so binding `e` is not found inside `err`.
fn mentions(text: &str, name: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|token| token == name)
}

/// A variable name without the sigil its language spells it with.
///
/// `$this` and `this` are the same receiver; `$self` and `self` are the
/// same object. Only ONE character comes off, so PHP's variable-variable
/// `$$name` still reads as `$name` and Ruby's `@@count` as `@count` —
/// both of which are genuinely different names from `name`.
///
/// A name that is NOTHING BUT a sigil keeps it. `$` is a legal
/// identifier in TypeScript and in Solidity, and stripping it left the
/// empty string, which matched the empty receiver name a free function
/// carries — so vscode's `$('.chart')` and OpenZeppelin's
/// `$._initializing` briefly read as accesses to their own object.
pub(crate) fn unsigiled(name: &str) -> &str {
    match name.strip_prefix(['$', '@', '%', '&']) {
        Some(rest) if !rest.is_empty() => rest,
        _ => name,
    }
}

/// Does an assertion callee name the SUBJECT it is about, or does its
/// receiver hold it?
///
/// An xUnit assertion names both on the callee — `Assert.True(..)`,
/// `$this->assertTrue(..)`, `assert.Equal(..)`, `self.assertTrue(..)` —
/// so a lone literal argument is the thing being asserted. A FLUENT
/// assertion hangs off the value it is about: in
/// `resultA.Name.ShouldBe("name1")` the subject is the receiver and the
/// literal is the expected answer. The qualifier is what tells them
/// apart, and a bare callee has no qualifier to doubt.
pub(crate) fn assertion_names_its_own_subject(callee: &str) -> bool {
    let parts: Vec<&str> = callee
        .split(|c: char| !identifierish(c) && c != '$' && c != '@')
        .filter(|p| !p.is_empty())
        .collect();
    let Some(head) = parts.len().checked_sub(2).and_then(|i| parts.get(i)) else {
        return true;
    };
    let head = unsigiled(head);
    matches!(head, "this" | "self" | "cls") || assertish(head) || expectish(head)
}

/// Does a caught type reach EVERY failure the runtime can raise?
///
/// Three packs asked this with a SUBSTRING — `text.contains("Exception ")`
/// in Java and C#, `pattern.contains("Exception")` in Scala — and
/// `IOException e` contains it. 2,089 of Java's 3,417 gold findings were
/// a specific type caught deliberately: IOException 520,
/// AssertionFailedError 316, MismatchedInputException 212. A root type
/// is now matched EXACTLY, as the last dotted segment of a token, so
/// `java.lang.Exception` still counts and `ArgumentException` does not.
///
/// `RuntimeException` is in the set because it reaches every unchecked
/// failure, and because the old substring rule already billed it —
/// leaving it out would have been a second, silent change.
fn catches_every_failure(decl: &str) -> bool {
    decl.split(|c: char| !identifierish(c) && c != '.')
        .any(|token| {
            matches!(
                token.rsplit('.').next().unwrap_or(token),
                "Throwable" | "Exception" | "Error" | "RuntimeException"
            )
        })
}

/// Does a declared return type promise a BOOLEAN — the thing an `is_`,
/// `has_`, `can_` or `should_` name says the answer will be?
///
/// The test was `returns.contains("bool")`, case-sensitively, and it was
/// wrong about four languages at once. Scala writes `Boolean`, Swift
/// writes `Bool`, Java's boxed form is `Boolean`: 660 of `lying name`'s
/// 6,323 gold findings were a predicate returning exactly what its name
/// promised, spelled with a capital. TypeScript's 1,212 were the
/// STRONGEST available form — `value is FormData` is a boolean at
/// runtime and carries the narrowing besides — reported as the metric's
/// violation. And C89 has no `bool`: `int` IS the boolean there, and
/// gold's C predicates return it 364 times against 94 `bool`s.
///
/// Tokenised rather than substring-matched, so `Option<bool>`,
/// `Task<bool>` and `bool?` still promise one while a type merely
/// SPELLED with the letters does not.
pub(crate) fn promises_a_boolean(lang: Lang, returns: &str) -> bool {
    let tokens = || {
        returns
            .split(|c: char| !identifierish(c))
            .filter(|t| !t.is_empty())
    };
    if tokens().any(|t| t.eq_ignore_ascii_case("bool") || t.eq_ignore_ascii_case("boolean")) {
        return true;
    }
    match lang {
        // A type predicate: `value is FormData`, `asserts x is T`. The
        // `is` is a keyword between two type positions, which no other
        // language in the table spells in a return type.
        Lang::TypeScript | Lang::Tsx => tokens().any(|t| t == "is"),
        // Truthiness. Every other C-family language has `bool`, so the
        // excuse stops at the one language that does not — and it is
        // `int` alone: gold's three pointer-returning predicates are not
        // enough to widen a rule on.
        Lang::C => returns.trim() == "int",
        _ => false,
    }
}

/// Does an identifier read as an assertion helper? The convention across
/// ecosystems is an `assert` prefix or suffix, case-insensitively:
/// serverAssert, redisassert, ASSERT, assertEqual, assertEqualStrings.
/// Slices via `get` so a non-ASCII identifier cannot split a char.
fn assertish(name: &str) -> bool {
    const PAT: &str = "assert";
    name.get(..PAT.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PAT))
        || name
            .len()
            .checked_sub(PAT.len())
            .and_then(|at| name.get(at..))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(PAT))
}

/// `expect` or `expectSomething` — a test helper. The capital matters:
/// without it `expected_value()` in production code reads as an
/// assertion.
fn expectish(name: &str) -> bool {
    const PAT: &str = "expect";
    name == PAT
        || name
            .strip_prefix(PAT)
            .is_some_and(|rest| rest.starts_with(char::is_uppercase))
}

/// The execution-space qualifiers, which are CUDA and are nothing else.
/// Judged at token boundaries rather than as substrings, so a name like
/// `my__device__id` cannot vote — and never on a comment line, because
/// "never call `__device__` functions from this translation unit" is a
/// sentence a HOST file writes about the boundary it sits on. The first
/// version skipped that guard and a comment mention flipped a plain
/// C++ file into the `[cu]` budget section; the C++ rule below was
/// line-anchored against exactly this and the asymmetry was the bug.
/// A qualifier inside a string literal still votes (a jitify-style
/// host file embedding kernel SOURCE reads as CUDA), which is accepted:
/// the grammar is a superset, so only the budget section moves.
///
/// Validated the same way the C++ rule was: 0 of 4,588 headers across
/// the C and C++ gold corpora match, and 15 of flash-attention's and
/// 76 of cutlass's do — figures unchanged by the comment guard,
/// because a real qualifier is a declaration, not a remark.
fn looks_like_cuda(source: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "__global__",
        "__device__",
        "__shared__",
        "__constant__",
        "__host__",
    ];
    source
        .lines()
        .filter(|line| !commentish(line))
        .flat_map(|line| line.split(|c: char| !identifierish(c)))
        .any(|token| MARKERS.contains(&token))
}

/// A line that reads as commentary: `//`, a block opener, or the `*`
/// continuation of one.
fn commentish(line: &str) -> bool {
    let head = line.trim_start();
    head.starts_with("//") || head.starts_with('*') || head.starts_with("/*")
}

/// A character an identifier may contain, so its absence ends a token.
fn identifierish(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Four spellings that are not C, judged at the START of a line so a
/// mention inside a comment or a string cannot vote. `::` and
/// `operator` were considered and dropped: both turn up in C comments,
/// and these four already separate the corpora perfectly.
fn looks_like_cpp(source: &str) -> bool {
    source.lines().any(|line| {
        let head = line.trim_start();
        head.starts_with("namespace ")
            || head.starts_with("template<")
            || head.starts_with("template <")
            || head.starts_with("public:")
            || head.starts_with("private:")
            || head.starts_with("protected:")
            || head
                .strip_prefix("class ")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_alphabetic() || c == '_'))
    })
}

/// Does this declared type text name one of the language's escape
/// hatches? Split into identifier tokens so a hatch counts wherever it
/// appears — `dict[str, Any]` and `Record<string, any>` hide behind a
/// generic but assert exactly as little as the bare hatch does. Token
/// equality, not substring: `AnyOf` and `voidptr_t` are ordinary names.
fn is_loose(text: &str, hatches: &[&str]) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|token| hatches.contains(&token))
}

/// Does this node kind bind by SHAPE rather than by name?
///
/// Every C-family grammar that has destructuring at all spells it with
/// the same two node kinds, and the TypeScript grammar is the
/// JavaScript one with annotations — so the list is shared rather than
/// repeated in each pack. Grammars whose pattern vocabulary is their
/// own (Rust, OCaml, Ruby, Elixir) answer in their own pack.
fn destructures(kind: &str) -> bool {
    matches!(kind, "object_pattern" | "array_pattern")
}

/// Shared helper: does `node`'s field hold a boolean-ish type name?
/// A build- or tool-configuration file: `vite.config.ts`,
/// `jest.config.js`, `rollup.config.mjs`. Named after what it
/// configures, and run once by the tool it configures.
/// Is this declaration an anonymous function EXPRESSION rather than a
/// named declaration?
///
/// A one-expression lambda is a forwarder by definition — that is what a
/// lambda IS — and it exists to adapt a callback's shape: `decode:
/// (str) => BigInt(str)`, `lookupLanguageModel: id => models.get(id)`.
/// Fowler's Middle Man is about a NAME that promises a layer and
/// delivers a hop, and a lambda has no name of its own to be shallow
/// about; the one it reports comes from the property it was bound to.
///
/// A kind list rather than a pack hook, because it names the same
/// construct in every grammar that has one and no pack has a say in the
/// answer. A grammar that renamed its lambda node would silently widen
/// this metric rather than break it, which is why the list is here in
/// the open and not spelled inside a rule.
pub(crate) fn is_a_lambda(kind: &str) -> bool {
    matches!(
        kind,
        "arrow_function"
            | "function_expression"
            | "generator_function"
            | "lambda"
            | "lambda_expression"
            | "lambda_literal"
            | "closure_expression"
            | "anonymous_function"
            | "anonymous_function_creation_expression"
            | "fun_literal"
            | "block_argument"
            | "do_block"
            | "anon_fn"
    )
}

/// Does the declaration MARK itself an override of someone else's
/// signature? Java and Kotlin write `@Override`, C#, Swift, Scala and
/// TypeScript a leading `override` modifier.
///
/// Read from the declaration HEAD — everything before the body — so a
/// body that happens to mention the word cannot answer for it. Where a
/// language marks nothing, this says nothing: Rust's trait impls and
/// Go's interface satisfaction leave no token to read, and inventing
/// one from the surrounding type would be the over-broad rule
/// `Pack::is_override` already is for `ceremony`.
pub(crate) fn declares_an_override(node: Node, src: &[u8]) -> bool {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |b| b.start_byte());
    let Some(head) = src
        .get(start..end)
        .and_then(|h| std::str::from_utf8(h).ok())
    else {
        return false;
    };
    head.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '@'))
        .any(|word| word == "@Override" || word == "override")
}

pub(crate) fn config_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.ends_with(".config") || stem == "config"
}

pub(crate) fn field_text_is<'a>(node: Node, field: &str, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)?.utf8_text(src).ok()
}

/// Byte span of the comment run directly above a definition — the shape
/// documentation takes in all but a handful of languages.
///
/// The run is CONTIGUOUS: a comment cut off from the definition by a
/// blank line is a note about the neighbourhood, not this definition's
/// contract. `kinds` are the node kinds that can carry one, and
/// `prefixes` the spellings this ecosystem reads as documentation —
/// empty where any comment above a definition documents it.
///
/// Fourteen packs share this. What differs between them is which nodes
/// to look at, which is exactly what a pack is for; walking the run and
/// measuring it are the same everywhere and are not.
fn doc_run(node: Node, kinds: &[&str], prefixes: &[&str], src: &[u8]) -> Option<(u32, u32)> {
    let mut span: Option<(u32, u32)> = None;
    let mut prev = node.prev_named_sibling();
    let mut expected = node.start_position().row;
    while let Some(p) = prev {
        if !kinds.contains(&p.kind()) || last_row(p) + 1 != expected {
            break;
        }
        if !prefixes.is_empty()
            && !p
                .utf8_text(src)
                .is_ok_and(|t| prefixes.iter().any(|m| t.starts_with(m)))
        {
            break;
        }
        let end = span.map_or(p.end_byte() as u32, |(_, end)| end);
        span = Some((p.start_byte() as u32, end));
        expected = p.start_position().row;
        prev = p.prev_named_sibling();
    }
    span
}

/// The span of one node, for the packs whose documentation is a single
/// node rather than a run.
fn node_span(node: Node) -> Option<(u32, u32)> {
    Some((node.start_byte() as u32, node.end_byte() as u32))
}

/// The last row this node puts text on.
///
/// A grammar may end a line comment AFTER its newline —
/// tree-sitter-rust does, so a `///` node ends at column 0 of the row
/// below. Taken literally, every Rust doc comment sits one row further
/// down than it looks, and a run would never touch the item it
/// documents.
pub(crate) fn last_row(node: Node) -> usize {
    let row = node.end_position().row;
    if node.end_position().column == 0 && row > node.start_position().row {
        row - 1
    } else {
        row
    }
}

#[cfg(test)]
mod conformance {
    //! The "languages are very alike" claim, made executable: the same
    //! function written in each supported language must produce identical
    //! metrics. This suite is what keeps the Sem ontology honest as packs
    //! are added.

    use super::Lang;

    use crate::facts::{FileFacts, extract};
    use crate::metrics::complexity;
    use std::path::Path;

    fn facts(lang: Lang, source: &str) -> FileFacts {
        facts_at(lang, "t", source)
    }

    /// Facts with a real path, for behavior that depends on WHERE the
    /// file lives (test-file conventions).
    fn facts_at(lang: Lang, path: &str, source: &str) -> FileFacts {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        extract(pack, &mut parser, Path::new(path), source)
    }

    /// (cognitive, cyclomatic, depth, params) of the first named unit.
    fn signature(lang: Lang, source: &str) -> (u32, u32, u16, u16) {
        let f = facts(lang, source);
        let u = &f.units[1];
        let (cog, cyc) = complexity(u);
        (cog, cyc, u.max_vis_depth, u.params.len() as u16)
    }

    const PY: &str = "
def classify(items, limit):
    total = 0
    for item in items:
        if item > 0 and item < limit:
            total += item
        elif item < 0:
            total -= item
        else:
            continue
    return total
";

    const RS: &str = "
fn classify(items: Vec<i64>, limit: i64) -> i64 {
    let mut total = 0;
    for item in items {
        if item > 0 && item < limit {
            total += item;
        } else if item < 0 {
            total -= item;
        } else {
            continue;
        }
    }
    total
}
";

    const TS: &str = "
function classify(items: number[], limit: number): number {
    let total = 0;
    for (const item of items) {
        if (item > 0 && item < limit) {
            total += item;
        } else if (item < 0) {
            total -= item;
        } else {
            continue;
        }
    }
    return total;
}
";

    const GO: &str = "
package main

func classify(items []int64, limit int64) int64 {
    total := int64(0)
    for _, item := range items {
        if item > 0 && item < limit {
            total += item
        } else if item < 0 {
            total -= item
        } else {
            continue
        }
    }
    return total
}
";

    const JS: &str = "
function classify(items, limit) {
    let total = 0;
    for (const item of items) {
        if (item > 0 && item < limit) {
            total += item;
        } else if (item < 0) {
            total -= item;
        } else {
            continue;
        }
    }
    return total;
}
";

    const ZIG: &str = "
fn classify(items: []const i64, limit: i64) i64 {
    var total: i64 = 0;
    for (items) |item| {
        if (item > 0 and item < limit) {
            total += item;
        } else if (item < 0) {
            total -= item;
        } else {
            continue;
        }
    }
    return total;
}
";

    const C: &str = "
long classify(const long *items, long limit) {
    long total = 0;
    for (int i = 0; items[i]; i++) {
        if (items[i] > 0 && items[i] < limit) {
            total += items[i];
        } else if (items[i] < 0) {
            total -= items[i];
        } else {
            continue;
        }
    }
    return total;
}
";

    /// C++ writes the loop as a range-for, which is its own grammar
    /// kind — so this column also proves `for_range_loop` costs what
    /// every other language's loop costs.
    const CPP: &str = "
long classify(const std::vector<long>& items, long limit) {
    long total = 0;
    for (long item : items) {
        if (item > 0 && item < limit) {
            total += item;
        } else if (item < 0) {
            total -= item;
        } else {
            continue;
        }
    }
    return total;
}
";

    /// OCaml's imperative loop. Idiomatic OCaml would iterate with
    /// `List.iter` and a lambda, which the ontology correctly reads as a
    /// CALL rather than a loop — so the conformance case uses the form
    /// that means the same thing the other eight languages mean.
    const ML: &str = "
let classify items limit =
  let total = ref 0 in
  for i = 0 to Array.length items - 1 do
    let item = items.(i) in
    if item > 0 && item < limit then total := !total + item
    else if item < 0 then total := !total - item
    else ()
  done;
  !total
";

    #[test]
    fn same_function_scores_identically_in_every_language() {
        // for +1, if +2 (nested), && +1, elif +1, else +1 = cognitive 6;
        // decisions for/if/&&/elif = cyclomatic 5; depth for>if = 2; 2 params.
        let expected = (6, 5, 2, 2);
        assert_eq!(signature(Lang::Python, PY), expected, "python");
        assert_eq!(signature(Lang::Rust, RS), expected, "rust");
        assert_eq!(signature(Lang::TypeScript, TS), expected, "typescript");
        assert_eq!(signature(Lang::Go, GO), expected, "go");
        assert_eq!(signature(Lang::JavaScript, JS), expected, "javascript");
        assert_eq!(signature(Lang::Zig, ZIG), expected, "zig");
        assert_eq!(signature(Lang::C, C), expected, "c");
        assert_eq!(signature(Lang::Cpp, CPP), expected, "cpp");
        assert_eq!(signature(Lang::OCaml, ML), expected, "ocaml");
    }

    #[test]
    fn param_names_extracted_in_every_language() {
        for (lang, src) in [
            (Lang::Python, PY),
            (Lang::Rust, RS),
            (Lang::TypeScript, TS),
            (Lang::Go, GO),
            (Lang::JavaScript, JS),
            (Lang::Zig, ZIG),
            (Lang::C, C),
            (Lang::Cpp, CPP),
            (Lang::OCaml, ML),
        ] {
            let f = facts(lang, src);
            let names: Vec<&str> = f.units[1].params.iter().map(|p| &*p.name).collect();
            assert_eq!(names, ["items", "limit"], "{lang:?}");
        }
    }

    #[test]
    fn goto_costs_the_same_wherever_it_exists() {
        // Go mapped `goto` to Sem::Jump, which scores nothing — the same
        // construct cost 1 in C and 0 in Go. Sem::Goto exists for exactly
        // this: break/continue are free within their loop, but a goto
        // sends the reader hunting for a label.
        let c =
            "int f(int x) {\n    if (x < 0) goto fail;\n    return x;\nfail:\n    return -1;\n}\n";
        let go = "package main\n\nfunc f(x int) int {\n\tif x < 0 {\n\t\tgoto fail\n\t}\n\treturn x\nfail:\n\treturn -1\n}\n";
        assert_eq!(signature(Lang::C, c).0, 2, "c: if + goto");
        assert_eq!(signature(Lang::Go, go).0, 2, "go must agree with c");
    }

    #[test]
    fn receiver_declared_methods_are_methods_in_every_language() {
        // Go declares the receiver in its own field, so the method has no
        // enclosing type node. Feature Envy asks `is_method` first, which
        // made it structurally dead for Go: identical code scored 4 in
        // Python and 0 in Go.
        let envious = |lang: Lang, src: &str| {
            let f = facts(lang, src);
            let u = &f.units[1];
            (u.is_method, u.envy_count, u.qualname.to_string())
        };
        let py = envious(
            Lang::Python,
            "class Billing:\n    def total(self, order):\n        return order.base + order.tax + order.shipping + order.discount\n",
        );
        let go = envious(
            Lang::Go,
            "package main\n\nfunc (b *Billing) Total(order *Order) int {\n\treturn order.base + order.tax + order.shipping + order.discount\n}\n",
        );
        assert_eq!(py, (true, 4, "Billing.total".into()));
        assert_eq!(go, (true, 4, "Billing.Total".into()));

        // The receiver is the method's OWN object, not an envied one —
        // otherwise enabling this would fire on every Go method.
        let own = envious(
            Lang::Go,
            "package main\n\nfunc (b *Billing) Label() string {\n\treturn b.prefix + b.sep + b.suffix + b.tail\n}\n",
        );
        assert_eq!(own, (true, 0, "Billing.Label".into()));
    }

    #[test]
    fn escape_hatches_are_found_through_their_generics() {
        // A hatch hidden in a generic asserts exactly as little as the
        // bare hatch: `dict[str, Any]` and `Record<string, any>` are the
        // form real code actually takes.
        let loose = |lang: Lang, src: &str| {
            let f = facts(lang, src);
            f.units[1..]
                .iter()
                .map(|u| (u.untyped_params(), u.loose_params()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            loose(
                Lang::Python,
                "from typing import Any\n\ndef clean(name: str, n: int) -> str:\n    return name\n\ndef sloppy(payload, options):\n    return payload\n\ndef fake(payload: Any, options: dict[str, Any]) -> Any:\n    return payload\n",
            ),
            [(0, 0), (2, 0), (0, 2)]
        );
        assert_eq!(
            loose(
                Lang::TypeScript,
                "export function clean(name: string, n: number): string { return name; }\nexport function sloppy(payload, options) { return payload; }\nexport function fake(payload: any, o: Record<string, any>): any { return payload; }\nexport function safe(payload: unknown): string { return String(payload); }\n",
            ),
            [(0, 0), (2, 0), (0, 2), (0, 0)],
            "unknown is the SAFE alternative to any — flagging it would punish the fix"
        );
        // Statically-typed languages have no untyped surface at all, so
        // the metric must read zero rather than every parameter.
        assert_eq!(
            loose(
                Lang::Go,
                "package p\n\nfunc Clean(name string, n int) string { return name }\n\nfunc Fake(payload interface{}, opts []interface{}) any { return payload }\n",
            ),
            [(0, 0), (0, 2)]
        );
    }

    #[test]
    fn every_language_spells_its_own_cast_and_they_all_count() {
        // Each language overrules its type checker differently; the point
        // of Sem::Cast is that the metric never has to know which.
        let counted = |lang: Lang, src: &str| {
            let f = facts(lang, src);
            let casts: u16 = f.units.iter().map(|u| u.casts).sum();
            (casts, f.suppressions.len())
        };
        assert_eq!(
            counted(
                Lang::TypeScript,
                "export function loose(raw: unknown): string {\n  // @ts-ignore legacy shape\n  const a = raw as string;\n  const b = (raw as any).value!;\n  return a + b;\n}\n",
            ),
            (3, 1),
            "as, as any, and the non-null assertion"
        );
        assert_eq!(
            counted(
                Lang::Python,
                "from typing import cast\n\ndef convert(raw):\n    a = cast(str, raw)  # type: ignore[arg-type]\n    return a\n",
            ),
            (1, 1)
        );
        assert_eq!(
            counted(
                Lang::Go,
                "package p\n\nfunc Convert(raw interface{}) string {\n\ts := raw.(string)\n\treturn s\n}\n",
            ),
            (1, 0)
        );
        assert_eq!(
            counted(
                Lang::C,
                "void *dup(void *p, unsigned n) {\n    char *q = (char *)p;\n    return (void *)(q + n);\n}\n",
            ),
            (2, 0)
        );
        assert_eq!(
            counted(
                Lang::Rust,
                "fn narrow(n: u64) -> u32 {\n    let a = n as u32;\n    a as u32\n}\n",
            ),
            (2, 0),
            "numeric `as` is Rust's silent truncation"
        );
        assert_eq!(
            counted(
                Lang::Cpp,
                "void *dup(void *p, unsigned n) {\n    char *q = static_cast<char *>(p);\n    return reinterpret_cast<void *>(q + n);\n}\n",
            ),
            (2, 0),
            "the named casts have no node of their own: each parses as a call to a template function"
        );
    }

    #[test]
    fn a_framework_declared_test_is_a_unit_in_js_and_ts() {
        // vitest, jest, mocha and node:test all spell a test as an
        // anonymous arrow passed to a call. Without recognising that,
        // every test in the JS/TS ecosystem is invisible: green-screen
        // reported 0% test units while having tests, and hono's gold
        // corpus read 53% when the real figure is 85%.
        let src = "import test from 'node:test';\n\ntest('resolves regional aliases', () => {\n  assert.equal(resolve('en-GB'), 'en');\n});\n\nconst helper = () => 1;\n";
        for lang in [Lang::TypeScript, Lang::JavaScript] {
            let f = facts(lang, src);
            let named: Vec<(&str, bool)> = f.units[1..]
                .iter()
                .map(|u| (&*u.name, u.named_test))
                .collect();
            assert_eq!(
                named,
                [("resolves regional aliases", true), ("helper", false)],
                "{lang:?}: the test takes its name from its first argument"
            );
        }

        // A callback that is not a test stays attributed to its caller.
        let cb = facts(
            Lang::TypeScript,
            "items.forEach((x) => { seen.push(x); });\n",
        );
        assert!(
            cb.units.len() == 1,
            "an ordinary callback is not promoted to a unit"
        );
    }

    /// (lang, path, source, how many assertion CALLS the file holds).
    /// Every ASSERTING line below read zero before this table existed,
    /// and each file also carries a call the rule must NOT believe:
    /// Go's `fmt.Errorf` and `err.Error()` wear the same verbs as
    /// `t.Errorf`, TypeScript's `ok` is an ordinary function name unless
    /// the file took it from Node's assert module, and Lua's `assert`
    /// namespace has to survive being read whole.
    const ASSERTION_VOCABULARY: &[(Lang, &str, &str, u16)] = &[
        (
            Lang::Go,
            "e_test.go",
            "package p\n\nimport (\n\t\"testing\"\n\t\"fmt\"\n\t\"github.com/stretchr/testify/require\"\n)\n\nfunc TestReady(t *testing.T) {\n\tt.Errorf(\"no\")\n\trequire.Equal(t, 1, one())\n\tt.Run(\"inner\", func(t *testing.T) { t.Fatal(\"nope\") })\n\tfmt.Errorf(\"wrapped\")\n\terr := run()\n\t_ = err.Error()\n}\n",
            3,
        ),
        (
            Lang::Lua,
            "e_spec.lua",
            "it(\"keeps the size it was given\", function()\n  assert.same(3, size())\n  assert.is_true(ok())\n  local m = obj.same(1)\nend)\n",
            2,
        ),
        (
            Lang::Swift,
            "ETests.swift",
            "@Test func addsUp() {\n    #expect(1 + 1 == 2)\n    let v = try #require(maybe())\n    XCTAssertEqual(v, 3)\n    #available(macOS 13, *)\n}\n",
            3,
        ),
        (
            Lang::TypeScript,
            "e.test.ts",
            "import { strictEqual, ok } from 'assert';\n\ntest('holds', () => {\n  strictEqual(1, 1);\n  ok(true);\n});\n",
            2,
        ),
        (
            Lang::TypeScript,
            "f.test.ts",
            "function ok(v: boolean) { return v; }\n\ntest('holds', () => {\n  ok(true);\n});\n",
            0,
        ),
    ];

    #[test]
    fn an_assertion_is_recognised_in_the_vocabulary_its_ecosystem_writes() {
        for (lang, path, src, want) in ASSERTION_VOCABULARY {
            let f = facts_at(*lang, path, src);
            let asserts: u16 = f.units.iter().map(|u| u.assert_calls).sum();
            assert_eq!(asserts, *want, "{path}");
        }
    }

    #[test]
    fn a_test_is_declared_only_by_evidence_its_context_supports() {
        // One name used to do two jobs: a production `test_connection`
        // health check was judged as a lazy assertless test AND pardoned
        // from every production metric. Name conventions declare only
        // inside test files; attributes and structure declare anywhere;
        // cfg(test) membership exempts without declaring.
        type Case = (Lang, &'static str, &'static str, (bool, bool), &'static str);
        let cases: &[Case] = &[
            (
                Lang::Python,
                "prod/net.py",
                "def test_connection(h):\n    return ping(h, 9042)\n",
                (false, false),
                "a prod test_-name is neither judged nor pardoned",
            ),
            (
                Lang::Python,
                "tests/test_net.py",
                "def test_connection(h):\n    assert ping(h)\n",
                (true, true),
                "the same name in a test file is the convention at work",
            ),
            (
                Lang::Python,
                "tests/test_net.py",
                "def make_server():\n    return object()\n",
                (false, true),
                "a test-file helper is exempt but never judged",
            ),
            (
                Lang::Go,
                "svc/health.go",
                "package p\n\nfunc TestConnection() bool { return true }\n",
                (false, false),
                "a production TestConnection is a function",
            ),
            (
                Lang::Go,
                "svc/health_test.go",
                "package p\n\nfunc TestConnection(t *T) { assertOk(t) }\n",
                (true, true),
                "the same name in _test.go is a test",
            ),
            (
                Lang::Rust,
                "src/lib.rs",
                "#[test]\nfn rejects_bad_input() {\n    assert!(1 == 1);\n}\n",
                (true, true),
                "an attribute is context-free evidence",
            ),
            (
                Lang::Rust,
                "src/lib.rs",
                "#[cfg(test)]\nmod tests {\n    fn fixture() -> u32 {\n        1\n    }\n}\n",
                (false, true),
                "a cfg(test) fixture is exempt but never judged",
            ),
            (
                Lang::TypeScript,
                "src/anywhere.ts",
                "test('resolves aliases', () => { assert.ok(resolve('en')); });\n",
                (true, true),
                "structure is context-free evidence",
            ),
        ];
        for (lang, path, src, want, why) in cases {
            let f = facts_at(*lang, path, src);
            let u = &f.units[1];
            assert_eq!((u.named_test, u.is_test), *want, "{why}");
        }
    }

    /// The count-shaped detector metrics whose liveness is decided by
    /// pack hooks and tables — the exact surface where three confirmed
    /// bugs (secrets dead in Rust/Zig/C, Go's error family, four
    /// languages' wildcard arms) died in silence.
    const DETECTORS: &[&str] = &[
        "secrets",
        "swallowed",
        "broad catch",
        "lost context",
        "unwraps",
        "blocking async",
        "dropped tasks",
        "casts",
        "suppressions",
        "spooky",
        "wildcard match",
        "negations",
        "demeter",
        "kw opacity",
        "flag params",
        "loose types",
        "untyped params",
        "lying name",
        "vacuous asserts",
        "test asserts",
        "conditional hook",
        "repurposed",
        "unawaited coroutine",
        "sleepy test",
        "skipped tests",
        "bool traps",
        "built query",
        "shelled out",
    ];

    /// Pairs that can NEVER fire, each with its reason. Deliberate
    /// deadness is a design decision stated here; ACCIDENTAL deadness
    /// is how detectors die in silence. Fixing a pack must shrink this
    /// list — the parity test refuses a pair that is both seeded alive
    /// and declared dead.
    pub(super) const DECLARED_DEAD: &[(Lang, &str, &str)] = &[
        (
            Lang::Python,
            "unwraps",
            "exceptions are the error path; no panic-instead-of-error idiom",
        ),
        (
            Lang::Python,
            "dropped tasks",
            "create_task is the idiom; unrecognized — deferred, not denied",
        ),
        (
            Lang::Rust,
            "swallowed",
            "errors are values; misuse surfaces as unwraps, not empty handlers",
        ),
        (Lang::Rust, "broad catch", "no exceptions"),
        (
            Lang::Rust,
            "lost context",
            "no exceptions, no chain to lose",
        ),
        (
            Lang::Rust,
            "suppressions",
            "#[allow] is scoped lint config, not a checker-comment",
        ),
        (Lang::Rust, "kw opacity", "no kwargs"),
        (
            Lang::Rust,
            "untyped params",
            "the language types every parameter",
        ),
        (
            Lang::Rust,
            "loose types",
            "no Any; the hatch is unsafe, judged as spooky and casts",
        ),
        (Lang::TypeScript, "unwraps", "no panic idiom"),
        (
            Lang::TypeScript,
            "kw opacity",
            "an options object is a declared shape, not a splat",
        ),
        (
            Lang::TypeScript,
            "broad catch",
            "catch binds everything by language design; there is no narrow catch to prefer",
        ),
        (
            Lang::JavaScript,
            "broad catch",
            "catch binds everything by language design",
        ),
        (Lang::Go, "broad catch", "errors are values"),
        (
            Lang::Go,
            "lost context",
            "no exception chain; %w-wrapping judgment deferred",
        ),
        (
            Lang::Go,
            "suppressions",
            "nolint is linter config, not a checker-comment",
        ),
        (Lang::Go, "kw opacity", "no kwargs"),
        (Lang::Go, "untyped params", "every parameter typed"),
        (
            Lang::Go,
            "blocking async",
            "goroutines have no single-threaded executor to stall",
        ),
        (
            Lang::Go,
            "dropped tasks",
            "the go statement returns no handle to drop",
        ),
        (
            Lang::JavaScript,
            "casts",
            "no cast syntax; coercion is invisible to the tree",
        ),
        (Lang::JavaScript, "unwraps", "no panic idiom"),
        (Lang::JavaScript, "kw opacity", "no kwargs"),
        (
            Lang::JavaScript,
            "loose types",
            "no type syntax to be loose in",
        ),
        (
            Lang::JavaScript,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::JavaScript,
            "lying name",
            "no declared return types to lie against",
        ),
        (
            Lang::Zig,
            "swallowed",
            "catch is an expression; empty-body judgment deferred",
        ),
        (Lang::Zig, "broad catch", "no typed catch to be broad"),
        (Lang::Zig, "lost context", "errors are values"),
        (
            Lang::Zig,
            "suppressions",
            "no checker-comment suppression exists",
        ),
        (Lang::Zig, "kw opacity", "no kwargs"),
        (
            Lang::Zig,
            "untyped params",
            "every parameter typed; anytype is loose, not untyped",
        ),
        (Lang::Zig, "blocking async", "async is out of the language"),
        (
            Lang::Zig,
            "dropped tasks",
            "Thread.spawn returns !Thread; try-wrapping precludes the bare-statement shape",
        ),
        (
            Lang::Zig,
            "negations",
            "the grammar parses !expr as an error-union type, so a negation is visible only where the operand cannot BE a type — `!try f()` and nothing else. The hook answered 44 times in 2.3M questions across the gold corpus and produced no De Morgan candidate, no negative-polarity name and no negated `!=`: 0 violations in 17,956 measurements",
        ),
        (Lang::C, "swallowed", "no exceptions"),
        (Lang::C, "broad catch", "no exceptions"),
        (Lang::C, "lost context", "no exceptions"),
        (
            Lang::C,
            "suppressions",
            "no checker-comment suppression exists",
        ),
        (
            Lang::C,
            "demeter",
            "Lieberherr's rule constrains which OBJECTS a method may send a message to, and C has no methods: `s->layout.sparse.offsets` is a path into a nested RECORD, with no neighbour carrying behaviour to ask instead. All 219 gold findings were that shape",
        ),
        (Lang::C, "kw opacity", "no kwargs"),
        (Lang::C, "untyped params", "every parameter typed"),
        (Lang::C, "blocking async", "no async"),
        (Lang::C, "dropped tasks", "no spawn idiom"),
        (
            Lang::C,
            "vacuous asserts",
            "no test-declaration form; assert() is a production invariant",
        ),
        (
            Lang::C,
            "sleepy test",
            "no test-declaration form; nothing here declares itself a test",
        ),
        (
            Lang::C,
            "skipped tests",
            "no test-declaration form, so nothing to switch off",
        ),
        (
            Lang::OCaml,
            "skipped tests",
            "no test-declaration form, so nothing to switch off",
        ),
        (
            Lang::Zig,
            "skipped tests",
            "`test` blocks are compiled in or out by the build; no per-test off switch",
        ),
        (
            Lang::Shell,
            "skipped tests",
            "bats `skip` is a runner builtin indistinguishable from a command of that name",
        ),
        (
            Lang::OCaml,
            "sleepy test",
            "no test-declaration form; nothing here declares itself a test",
        ),
        (
            Lang::Shell,
            "sleepy test",
            "no test-declaration form: `[` is both assertion and control flow",
        ),
        (Lang::C, "test asserts", "no test-declaration form"),
        (
            Lang::OCaml,
            "secrets",
            "let_binding doubles as the unit definition; value anchoring deferred",
        ),
        (
            Lang::OCaml,
            "swallowed",
            "no Catch mapped; try is measured as structure",
        ),
        (Lang::OCaml, "broad catch", "no Catch mapped"),
        (
            Lang::OCaml,
            "lost context",
            "no chain-keeping idiom to check",
        ),
        (
            Lang::OCaml,
            "suppressions",
            "no checker-comment suppression exists",
        ),
        (
            Lang::OCaml,
            "unwraps",
            "the panicky list would be the language's own RAISE vocabulary: `failwith`, `invalid_arg`. There is no panic construct distinct from raising, so the metric would measure how thoroughly a function validates its arguments — 0 of 7 gold findings were a panic where an error belonged",
        ),
        (Lang::OCaml, "kw opacity", "no kwargs"),
        (
            Lang::OCaml,
            "blocking async",
            "async is a library, not syntax",
        ),
        (Lang::OCaml, "dropped tasks", "no spawn idiom recognized"),
        (
            Lang::OCaml,
            "casts",
            "no cast syntax at all: a coercion is `(e : t)`, which is an ASCRIPTION the checker still verifies. `Obj.magic` is the one thing that overrides it, and it is judged as spooky rather than as a cast, the way Rust's transmute is",
        ),
        (
            Lang::OCaml,
            "negations",
            "not is an ordinary function application",
        ),
        (
            Lang::OCaml,
            "demeter",
            "field access is module access; no attr node mapped",
        ),
        (
            Lang::OCaml,
            "flag params",
            "types inferred; boolishness invisible in the signature",
        ),
        (Lang::OCaml, "loose types", "no hatch vocabulary"),
        (Lang::OCaml, "lying name", "no return-type annotations"),
        (
            Lang::OCaml,
            "vacuous asserts",
            "applications carry no arguments field to inspect",
        ),
        (
            Lang::OCaml,
            "test asserts",
            "no test-declaration form recognized",
        ),
        // Shell declares no parameters (`$1` is read from the caller's
        // frame) and no types, so the whole interface family is silent
        // by the language's own design rather than by omission.
        (
            Lang::Shell,
            "kw opacity",
            "a function takes $@; no keyword surface exists to obscure",
        ),
        (Lang::Shell, "flag params", "no declared parameters at all"),
        (
            Lang::Shell,
            "bool traps",
            "no boolean literal: every argument is a string",
        ),
        (
            Lang::Zig,
            "built query",
            "no SQL in the corpus and no format call this recognizes — allocPrint is the idiom, unmapped",
        ),
        (
            Lang::OCaml,
            "built query",
            "the corpus is Base and Dune: no database access to build a statement for",
        ),
        (
            Lang::Shell,
            "built query",
            "a heredoc to psql is the idiom, and its body is text this pack never enters",
        ),
        (
            Lang::Rust,
            "shelled out",
            "Command is a BUILDER: the -c and the assembled argument are separate calls, and joining them needs data flow",
        ),
        (
            Lang::C,
            "shelled out",
            "sprintf fills a buffer and system() reads it two statements later — the same data flow",
        ),
        (
            Lang::Zig,
            "shelled out",
            "ChildProcess takes an argv slice; there is no shell-string form to assemble",
        ),
        (
            Lang::OCaml,
            "shelled out",
            "the corpus is Base and Dune: no process spawning to judge",
        ),
        (
            Lang::Shell,
            "shelled out",
            "the whole language IS the shell; `eval` is already spooky's",
        ),
        (Lang::Shell, "loose types", "no type syntax to be loose in"),
        (
            Lang::Shell,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::Shell,
            "lying name",
            "no declared return types to lie against",
        ),
        (
            Lang::Shell,
            "casts",
            "every value is a string; there is no cast to count",
        ),
        (
            Lang::Shell,
            "swallowed",
            "`cmd || true` is EXPLICIT silencing, which the Zen exempts",
        ),
        (Lang::Shell, "broad catch", "no exceptions"),
        (Lang::Shell, "lost context", "no exceptions"),
        (
            Lang::Shell,
            "unwraps",
            "`exit 1` is ordinary control flow in a script, not a panic",
        ),
        (
            Lang::Shell,
            "suppressions",
            "shellcheck directives are lint config, not a checker-comment",
        ),
        (Lang::Shell, "blocking async", "no async"),
        (
            Lang::Shell,
            "dropped tasks",
            "`cmd &` returns no handle to drop",
        ),
        (
            Lang::Shell,
            "demeter",
            "a value is a string, not an object; no member access exists",
        ),
        // bats and shunit2 assert with the `[` builtin, which is
        // indistinguishable from ordinary control flow, so every
        // test-named shell function reads as assertionless — the
        // corpus said so at 100%. Shell declares no tests at all.
        (
            Lang::Shell,
            "vacuous asserts",
            "no test-declaration form: `[` is both assertion and control flow",
        ),
        (
            Lang::Shell,
            "test asserts",
            "no test-declaration form: `[` is both assertion and control flow",
        ),
        // Hooks are React's idea, and the rule exists because React
        // identifies a hook by CALL ORDER. No other language here has
        // an order-identified construct to break.
        (Lang::Python, "conditional hook", "no call-order identity"),
        (Lang::Rust, "conditional hook", "no call-order identity"),
        (Lang::Go, "conditional hook", "no call-order identity"),
        (Lang::Zig, "conditional hook", "no call-order identity"),
        (Lang::C, "conditional hook", "no call-order identity"),
        (Lang::OCaml, "conditional hook", "no call-order identity"),
        (Lang::Shell, "conditional hook", "no call-order identity"),
        (
            Lang::OCaml,
            "repurposed",
            "a let is a fresh binding and `:=` writes through a ref, never rebinding the name",
        ),
        (
            Lang::Go,
            "unawaited coroutine",
            "a goroutine RUNS when spawned; there is no coroutine object to drop",
        ),
        (Lang::C, "unawaited coroutine", "no async"),
        (
            Lang::Zig,
            "unawaited coroutine",
            "async left the language; the grammar has no await to see",
        ),
        (
            Lang::OCaml,
            "unawaited coroutine",
            "no await syntax — concurrency is a library of ordinary functions",
        ),
        (Lang::Shell, "unawaited coroutine", "no async"),
        (
            Lang::TypeScript,
            "unawaited coroutine",
            "a promise is eagerly scheduled: the call RUNS and only its rejection goes unobserved — a weaker claim admired code violates deliberately (144 telemetry sends in gold), owned by no-floating-promises",
        ),
        (
            Lang::JavaScript,
            "unawaited coroutine",
            "a promise is eagerly scheduled: the call RUNS — same verdict as TypeScript",
        ),
        // C++ keeps C's verdicts wherever C++ kept C's semantics, and
        // differs exactly where the language does: exceptions revive
        // swallowed and broad catch, and gtest revives the test family.
        (
            Lang::Cpp,
            "lost context",
            "`throw;` rethrows and `throw X(e)` is a constructor call — telling the two apart needs the catch parameter's flow",
        ),
        (
            Lang::Cpp,
            "suppressions",
            "NOLINT is clang-tidy configuration, not a checker-comment",
        ),
        (Lang::Cpp, "kw opacity", "no kwargs"),
        (Lang::Cpp, "untyped params", "every parameter typed"),
        (
            Lang::Cpp,
            "blocking async",
            "`co_await` is a suspension operator, not a declaration: nothing marks a unit async, so nothing can block one",
        ),
        (
            Lang::Cpp,
            "unawaited coroutine",
            "same absence of a declaration form; a coroutine is known by its return type, which needs resolution",
        ),
        (
            Lang::Cpp,
            "dropped tasks",
            "a discarded `std::async` future BLOCKS in its destructor — the opposite failure, and unrecognized",
        ),
        (
            Lang::Cpp,
            "skipped tests",
            "gtest skips with a `DISABLED_` prefix inside the test NAME; there is no declaration to point at",
        ),
        (Lang::Cpp, "conditional hook", "no call-order identity"),
        (
            Lang::Cpp,
            "shelled out",
            "system() takes a `const char*`, so an assembled command reaches it through a separate string and `.c_str()` — C's data flow with one more hop",
        ),
        // Ruby. Concurrency, types and casts are all library or
        // convention here, so the families that read a DECLARATION have
        // nothing to read.
        (
            Lang::Ruby,
            "blocking async",
            "no async declaration: Thread and Fiber are objects, and nothing marks a method as running on an executor",
        ),
        (
            Lang::Ruby,
            "dropped tasks",
            "`Thread.new` returns a thread nobody is required to join; the language has no spawn that hands back a handle to lose",
        ),
        (
            Lang::Ruby,
            "casts",
            "no cast syntax: `Integer(x)` and `to_i` are conversions the receiver defines, not an override of a checker",
        ),
        (
            Lang::Ruby,
            "suppressions",
            "`rubocop:disable` configures a style linter; there is no type checker to silence",
        ),
        (Lang::Ruby, "loose types", "no type syntax to be loose in"),
        (
            Lang::Ruby,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::Ruby,
            "lying name",
            "no declared return types to lie against, and no receiver-mutability marker for a lying getter",
        ),
        (Lang::Ruby, "conditional hook", "no call-order identity"),
        (
            Lang::Ruby,
            "unawaited coroutine",
            "no coroutine object exists: a method call runs",
        ),
        // Lua. The smallest surface here: no classes, no types, no
        // exceptions and no match. Most of these are absences of syntax
        // rather than of a pack hook.
        (
            Lang::Lua,
            "swallowed",
            "`pcall` returns a status PAIR; an ignored error is an unused value, not an empty handler block",
        ),
        (
            Lang::Lua,
            "broad catch",
            "no catch construct at all, so no catch is wider than another",
        ),
        (
            Lang::Lua,
            "unwraps",
            "the panicky list would be the language's own RAISE vocabulary: `error`. There is no panic construct distinct from raising, so the metric would measure how thoroughly a function validates its arguments — 0 of 72 gold findings were a panic where an error belonged",
        ),
        (Lang::Lua, "lost context", "no exception chain to break"),
        (
            Lang::Lua,
            "blocking async",
            "coroutines are a library of ordinary functions; nothing declares a unit async",
        ),
        (Lang::Lua, "dropped tasks", "no spawn form"),
        (Lang::Lua, "casts", "no cast syntax"),
        (
            Lang::Lua,
            "suppressions",
            "luacheck directives configure a linter, not a type checker",
        ),
        (
            Lang::Lua,
            "wildcard match",
            "no match or switch construct: a dispatch table is an ordinary table lookup",
        ),
        (
            Lang::Lua,
            "kw opacity",
            "no keyword arguments; `...` is positional and the idiom is to pass a table",
        ),
        (
            Lang::Lua,
            "flag params",
            "a parameter carries neither a type nor a default, so nothing in a signature marks a switch",
        ),
        (Lang::Lua, "loose types", "no type syntax to be loose in"),
        (
            Lang::Lua,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::Lua,
            "lying name",
            "no declared return types to lie against",
        ),
        (Lang::Lua, "conditional hook", "no call-order identity"),
        (
            Lang::Lua,
            "unawaited coroutine",
            "`coroutine.create` returns an inert object by design and resuming it is the caller's job — the idiom, not the defect",
        ),
        // Perl.
        (
            Lang::Perl,
            "broad catch",
            "`catch ($e)` binds whatever died into a lexical; there is no class to name, so no catch is narrower than another",
        ),
        (
            Lang::Perl,
            "dropped tasks",
            "`fork` returns a pid to the parent and 0 to the child — a branch, not a handle",
        ),
        (
            Lang::Perl,
            "casts",
            "no cast syntax; a sigil decides how a value is read",
        ),
        (
            Lang::Perl,
            "suppressions",
            "`## no critic` configures Perl::Critic, a style linter, and there is no type checker to silence",
        ),
        (
            Lang::Perl,
            "wildcard match",
            "no switch: `given`/`when` was made experimental and then removed",
        ),
        (
            Lang::Perl,
            "unwraps",
            "the panicky list would be the language's own RAISE vocabulary: `die`, `croak`, `confess`, `exit`. There is no panic construct distinct from raising, so the metric would measure how thoroughly a function validates its arguments — 0 of 58 gold findings were a panic where an error belonged",
        ),
        (Lang::Perl, "loose types", "no type syntax to be loose in"),
        (
            Lang::Perl,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::Perl,
            "lying name",
            "no declared return types to lie against",
        ),
        (Lang::Perl, "conditional hook", "no call-order identity"),
        (
            Lang::Perl,
            "unawaited coroutine",
            "Future::AsyncAwait runs an async sub eagerly to its first await — the call RUNS, which is the TypeScript verdict",
        ),
        // PHP.
        (
            Lang::Php,
            "blocking async",
            "Fibers are objects a scheduler drives; nothing declares a function async",
        ),
        (Lang::Php, "dropped tasks", "no spawn form"),
        (
            Lang::Php,
            "kw opacity",
            "`...$args` is a positional variadic; named arguments are written at the CALL site and hide nothing",
        ),
        (Lang::Php, "conditional hook", "no call-order identity"),
        (
            Lang::Php,
            "unawaited coroutine",
            "no await syntax; a Fiber is started explicitly",
        ),
        // Java.
        (
            Lang::Java,
            "blocking async",
            "concurrency is Executor, Future and virtual threads — nothing in the language marks a method async",
        ),
        (
            Lang::Java,
            "dropped tasks",
            "a task goes to an Executor that owns it; the discarded value is a Future, which is the opposite failure and unrecognized",
        ),
        (
            Lang::Java,
            "suppressions",
            "`@SuppressWarnings` is a declaration the compiler reads, not a checker-comment",
        ),
        (Lang::Java, "kw opacity", "no kwargs"),
        (Lang::Java, "untyped params", "every parameter typed"),
        (Lang::Java, "conditional hook", "no call-order identity"),
        (Lang::Java, "unawaited coroutine", "no await syntax"),
        // C#.
        (
            Lang::CSharp,
            "dropped tasks",
            "`Task.Run` is the spawn and fire-and-forget is a documented pattern (`_ = ...`); the evidence this metric reads is a call named `spawn`, which the language has not got",
        ),
        (
            Lang::CSharp,
            "suppressions",
            "`#pragma warning disable` is a compiler directive, not a checker-comment",
        ),
        (
            Lang::CSharp,
            "kw opacity",
            "named arguments are written at the CALL site; there is no keyword-splat parameter",
        ),
        (Lang::CSharp, "untyped params", "every parameter typed"),
        (Lang::CSharp, "conditional hook", "no call-order identity"),
        (
            Lang::CSharp,
            "unawaited coroutine",
            "an async method RUNS synchronously to its first await — the TypeScript verdict, and CS4014 already owns it",
        ),
        // Swift.
        (
            Lang::Swift,
            "dropped tasks",
            "`Task { }` is an initializer, not a call named spawn, and an unstructured task is the documented way to leave structured concurrency",
        ),
        (
            Lang::Swift,
            "suppressions",
            "`swiftlint:disable` configures a style linter; there is no type checker to silence",
        ),
        (Lang::Swift, "untyped params", "every parameter typed"),
        (
            Lang::Swift,
            "kw opacity",
            "no keyword splat; an `_` argument label suppresses the WORD at the call site and still declares one named, typed parameter",
        ),
        (Lang::Swift, "conditional hook", "no call-order identity"),
        (
            Lang::Swift,
            "unawaited coroutine",
            "an async function RUNS to its first suspension; the compiler already refuses an unawaited call",
        ),
        // Scala.
        (
            Lang::Scala,
            "blocking async",
            "Future, ZIO and cats-effect are libraries; nothing in the language marks a def async",
        ),
        (Lang::Scala, "dropped tasks", "no spawn form"),
        (
            Lang::Scala,
            "casts",
            "`asInstanceOf` is a method call, judged as spooky; there is no cast syntax",
        ),
        (
            Lang::Scala,
            "suppressions",
            "`@nowarn` is an annotation the compiler reads, not a checker-comment",
        ),
        (
            Lang::Scala,
            "kw opacity",
            "named arguments are written at the CALL site; there is no keyword-splat parameter",
        ),
        (Lang::Scala, "untyped params", "every parameter typed"),
        (Lang::Scala, "conditional hook", "no call-order identity"),
        (
            Lang::Scala,
            "unawaited coroutine",
            "a Future is running the moment it is constructed — the TypeScript verdict",
        ),
        // Elixir. The homoiconic language: `def` is a call, so most of
        // what looks like syntax elsewhere is a name here.
        (
            Lang::Elixir,
            "blocking async",
            "concurrency is processes and Task; nothing marks a function async",
        ),
        (
            Lang::Elixir,
            "dropped tasks",
            "`spawn` returns a pid the caller is EXPECTED to drop: a process is owned by its supervisor, not by whoever started it",
        ),
        (
            Lang::Elixir,
            "unwraps",
            "the panicky list would be the language's own RAISE vocabulary: `raise`, `throw`, `exit`. There is no panic construct distinct from raising, so the metric would measure how thoroughly a function validates its arguments — 0 of 16 gold findings were a panic where an error belonged",
        ),
        (Lang::Elixir, "casts", "no cast syntax"),
        (
            Lang::Elixir,
            "suppressions",
            "`# credo:disable-for-next-line` configures a style linter; dialyzer is directed by `@dialyzer` attributes, not comments",
        ),
        (
            Lang::Elixir,
            "demeter",
            "`order.customer.address` is a chain of zero-arity CALLS — every field access is one — and a call ends a Demeter descent by definition",
        ),
        (
            Lang::Elixir,
            "kw opacity",
            "options travel as an ordinary keyword LIST argument; there is no splat parameter",
        ),
        (Lang::Elixir, "loose types", "no type syntax to be loose in"),
        (
            Lang::Elixir,
            "untyped params",
            "untyped is the language, not the code",
        ),
        (
            Lang::Elixir,
            "lying name",
            "no declared return types to lie against",
        ),
        (Lang::Elixir, "conditional hook", "no call-order identity"),
        (
            Lang::Elixir,
            "repurposed",
            "rebinding is the norm and carries none of the meaning it does elsewhere: `x = transform(x)` is a pipeline written without `|>`",
        ),
        (
            Lang::Elixir,
            "unawaited coroutine",
            "no await syntax; `Task.await` is an ordinary function on a struct",
        ),
        // Solidity. A transaction is atomic and single-threaded, and a
        // contract talks to no operating system, so three whole families
        // have nothing to describe.
        (
            Lang::Solidity,
            "lost context",
            "a revert carries a selector and its arguments; there is no cause to attach, so nothing can be dropped",
        ),
        (
            Lang::Solidity,
            "blocking async",
            "execution is single-threaded and atomic per transaction",
        ),
        (Lang::Solidity, "dropped tasks", "no concurrency at all"),
        (
            Lang::Solidity,
            "suppressions",
            "`solhint-disable` configures a style linter; the compiler has no comment that silences it",
        ),
        (
            Lang::Solidity,
            "wildcard match",
            "no switch in Solidity itself; Yul's `switch` is assembly, already judged as spooky",
        ),
        (Lang::Solidity, "kw opacity", "no kwargs"),
        (Lang::Solidity, "untyped params", "every parameter typed"),
        (Lang::Solidity, "conditional hook", "no call-order identity"),
        (Lang::Solidity, "unawaited coroutine", "no async"),
        (
            Lang::Solidity,
            "sleepy test",
            "there is no clock to sleep on: a transaction is atomic, and Forge moves time with `vm.warp`",
        ),
        (
            Lang::Solidity,
            "built query",
            "there is no database; a contract's only store is its own storage",
        ),
        (
            Lang::Solidity,
            "shelled out",
            "the EVM has no operating system to hand a command to",
        ),
    ];

    /// Languages whose detector matrix has NOT been audited yet.
    ///
    /// A blanket failure says only "somewhere, something is unproven",
    /// which is the same amount of information as silence. This list
    /// names the gap instead: every language here ships distribution
    /// metrics that are calibrated and trusted, and count-shaped
    /// detectors that may be dying quietly.
    ///
    /// It may only ever SHRINK. Adding a language to it to make a build
    /// pass would be the exact evasion the matrix exists to prevent, so
    /// the test below pins its length.
    const PENDING_PARITY: &[Lang] = &[];

    #[test]
    fn the_language_table_is_indexed_by_the_enum() {
        // DESCS is read with `self as usize`, so a row out of order
        // would give a language another's name, extensions and pack.
        for (i, d) in super::DESCS.iter().enumerate() {
            assert_eq!(d.lang as usize, i, "DESCS row {i} is {:?}", d.lang);
            assert_eq!(super::LANGS[i], d.lang, "LANGS and DESCS disagree at {i}");
        }
    }

    #[test]
    fn the_unaudited_language_list_only_shrinks() {
        assert!(
            PENDING_PARITY.is_empty(),
            "a language was ADDED to the unaudited list — audit it instead"
        );
    }

    #[test]
    fn every_detector_is_seeded_alive_or_declared_dead() {
        // Deadness must be a decision, never an accident: for each
        // (detector, language) pair, either the recall fixtures PROVE
        // it alive on every run, or this table states why it cannot
        // fire. Both at once means the table is stale; neither means a
        // detector may be dying in silence right now.
        for lang in super::LANGS {
            if PENDING_PARITY.contains(&lang) {
                continue;
            }
            // The TSX pack is the TS pack under a JSX-aware grammar
            // and the CUDA pack is the C++ pack under a launch-aware
            // one; each follows the evidence of the pack it IS. A
            // dialect that needed its own row would be a dialect with
            // its own hooks, which is the moment it stops being one.
            let evidence = match lang {
                Lang::Tsx => Lang::TypeScript,
                Lang::Cuda => Lang::Cpp,
                l => l,
            };
            for metric in DETECTORS {
                let seeded = crate::recall::SEEDS.iter().any(|(file, m, _)| {
                    m == metric
                        && crate::recall::FIXTURES
                            .iter()
                            .any(|(f, l, _)| f == file && *l == evidence)
                });
                let declared = DECLARED_DEAD
                    .iter()
                    .any(|(l, m, _)| *l == evidence && m == metric);
                assert!(
                    seeded || declared,
                    "{lang:?} x {metric}: neither seeded alive nor declared dead — a detector may be dying in silence"
                );
                assert!(
                    !(seeded && declared),
                    "{lang:?} x {metric}: seeded AND declared dead — the matrix is stale"
                );
            }
        }
    }

    /// Languages whose parameter list can bind by SHAPE, each with a
    /// signature taking one pattern and one plain name.
    ///
    /// Live evidence, not assertion: `destructured` exists so that a
    /// check comparing documented parameter names against declared ones
    /// can pass over a unit whose signature names nothing to compare
    /// with, and a flag that quietly stopped being set would let the
    /// check fire on every `@param options` in the corpus.
    const PATTERN_PARAMS: &[(Lang, &str, &[bool])] = &[
        (
            Lang::Python,
            "def f((a, b), c):\n    pass\n",
            &[true, false],
        ),
        (
            Lang::Rust,
            "fn f((a, b): (u8, u8), c: u8) {}\n",
            &[true, false],
        ),
        (
            Lang::TypeScript,
            "function f({ a, b }: O, c: number) {}\n",
            &[true, false],
        ),
        (
            Lang::JavaScript,
            "function f({ a, b } = {}, [x, y], c) {}\n",
            &[true, true, false],
        ),
        (Lang::Ruby, "def f((a, b), c)\nend\n", &[true, false]),
        (
            Lang::Elixir,
            "defmodule M do\n  def f(%User{id: id}, c) do\n    c\n  end\nend\n",
            &[true, false],
        ),
        (Lang::OCaml, "let f (a, b) c = c\n", &[true, false]),
    ];

    /// Languages whose parameter list can only bind NAMES, each with the
    /// reason. Pattern matching elsewhere in the language does not
    /// count: what matters is whether a DECLARATION can take a shape.
    const PATTERNLESS: &[(Lang, &str)] = &[
        (Lang::Go, "a parameter is a name and a type"),
        (Lang::C, "a declarator binds one name"),
        (
            Lang::Cpp,
            "structured bindings are for locals, not parameters",
        ),
        (Lang::Cuda, "the C++ pack under a launch-aware grammar"),
        (Lang::Tsx, "the TypeScript pack under a JSX-aware grammar"),
        (Lang::Zig, "a parameter is a name and a type"),
        (Lang::Java, "a formal parameter is a type and a name"),
        (
            Lang::CSharp,
            "deconstruction is a statement, not a parameter",
        ),
        (Lang::Swift, "a tuple parameter still binds one name"),
        (
            Lang::Scala,
            "a pattern belongs to `case`; `def` takes named parameters",
        ),
        (
            Lang::Php,
            "`list()` destructures an assignment, not a signature",
        ),
        (
            Lang::Perl,
            "a signature binds scalars; unpacking is @_ in the body",
        ),
        (Lang::Lua, "no destructuring anywhere in the language"),
        (
            Lang::Solidity,
            "a parameter is a type, a location and a name",
        ),
        (
            Lang::Shell,
            "no declared parameters at all — $1 is positional",
        ),
    ];

    #[test]
    fn a_pattern_parameter_is_marked_as_one() {
        for (lang, src, want) in PATTERN_PARAMS {
            let f = facts_at(*lang, &format!("a.{}", lang.name()), src);
            let u = f
                .units
                .iter()
                .find(|u| &*u.name == "f")
                .unwrap_or_else(|| panic!("{lang:?}: no unit `f` in {src:?}"));
            let got: Vec<bool> = u.params.iter().map(|p| p.destructured).collect();
            assert_eq!(&got[..], *want, "{lang:?} {src:?}");
        }
    }

    /// The width of a catch, in the three languages whose packs asked
    /// with a substring. Each source declares one unit `f`.
    ///
    /// `text.contains("Exception ")` is true of `IOException e`, and
    /// 2,089 of Java's 3,417 gold `broad catch` findings were a type the
    /// author chose deliberately — IOException 520, AssertionFailedError
    /// 316, MismatchedInputException 212. The root name is matched
    /// exactly now, as the last dotted segment of a token, so a
    /// qualified `java.lang.Throwable` still counts and a specific type
    /// that merely ENDS in a root name does not.
    const CATCH_WIDTH: &[(Lang, &str, &str, u16)] = &[
        (
            Lang::Java,
            "A.java",
            "class A {\n void f() {\n try { g(); } catch (Exception e) { h(e); }\n }\n}\n",
            1,
        ),
        (
            Lang::Java,
            "A.java",
            "class A {\n void f() {\n try { g(); } catch (java.lang.Throwable t) { h(t); }\n }\n}\n",
            1,
        ),
        (
            Lang::Java,
            "A.java",
            "class A {\n void f() {\n try { g(); } catch (IOException e) { h(e); }\n }\n}\n",
            0,
        ),
        (
            Lang::Java,
            "A.java",
            "class A {\n void f() {\n try { g(); } catch (AssertionFailedError e) { h(e); }\n }\n}\n",
            0,
        ),
        (
            Lang::CSharp,
            "A.cs",
            "class A {\n void f() {\n try { g(); } catch (Exception ex) { h(ex); }\n }\n}\n",
            1,
        ),
        (
            Lang::CSharp,
            "A.cs",
            "class A {\n void f() {\n try { g(); } catch (ArgumentException) { h(); }\n }\n}\n",
            0,
        ),
        (
            Lang::Scala,
            "A.scala",
            "class A {\n def f(): Unit = {\n try { g() } catch { case e: Exception => h(e) }\n }\n}\n",
            1,
        ),
        (
            Lang::Scala,
            "A.scala",
            "class A {\n def f(): Unit = {\n try { g() } catch { case _: Throwable => h() }\n }\n}\n",
            1,
        ),
        (
            Lang::Scala,
            "A.scala",
            "class A {\n def f(): Unit = {\n try { g() } catch { case e: ClassCastException => h(e) }\n }\n}\n",
            0,
        ),
    ];

    /// An empty handler silences the failure whatever it caught, so
    /// `swallowed` must not follow `broad catch` down. Scala asked
    /// breadth FIRST and only judged emptiness on an arm that reached
    /// everything — so narrowing breadth took two real gold findings
    /// (zio's `case _: SecurityException =>` and `case _:
    /// InterruptedException => ()`) with it until the order was fixed.
    const SILENT_HANDLER: &[(Lang, &str, &str)] = &[
        (
            Lang::Scala,
            "A.scala",
            "class A {\n def f(): Unit = {\n try { g() } catch { case _: SecurityException => }\n }\n}\n",
        ),
        (
            Lang::Scala,
            "A.scala",
            "class A {\n def f(): Unit = {\n try { g() } catch { case _: InterruptedException => () }\n }\n}\n",
        ),
        (
            Lang::Java,
            "A.java",
            "class A {\n void f() {\n try { g(); } catch (IOException e) { }\n }\n}\n",
        ),
    ];

    #[test]
    fn a_root_type_is_broad_and_any_empty_arm_is_silent() {
        for (lang, name, src, want) in CATCH_WIDTH {
            let f = facts_at(*lang, name, src);
            let u = f
                .units
                .iter()
                .find(|u| &*u.name == "f")
                .unwrap_or_else(|| panic!("{lang:?}: no unit `f` in {src:?}"));
            assert_eq!(u.broad_catch, *want, "{lang:?} {src:?}");
        }
        for (lang, name, src) in SILENT_HANDLER {
            let f = facts_at(*lang, name, src);
            let u = f
                .units
                .iter()
                .find(|u| &*u.name == "f")
                .unwrap_or_else(|| panic!("{lang:?}: no unit `f` in {src:?}"));
            assert_eq!((u.swallowed, u.broad_catch), (1, 0), "{lang:?} {src:?}");
        }
    }

    #[test]
    fn an_operandless_reraise_carries_the_cause_it_never_names() {
        for (lang, name, src, want) in RERAISE {
            let f = facts_at(*lang, name, src);
            let u = f
                .units
                .iter()
                .find(|u| &*u.name == "f")
                .unwrap_or_else(|| panic!("{lang:?}: no unit `f` in {src:?}"));
            assert_eq!(u.lost_context, *want, "{lang:?} {src:?}");
        }
    }

    /// Whether a handler drops the cause. Each source declares one unit
    /// `f`; the flag is whether `lost context` should fire.
    ///
    /// The OPERANDLESS rows are what this table was built for. A bare
    /// `throw;` / `raise` hands the caught error onward with its stack
    /// intact — it is the remedy, not the defect — and it mentions the
    /// binding nowhere only because it mentions nothing at all. The
    /// text test read that as a fresh error, so the rule ran exactly
    /// backwards on 34 of C#'s 36 gold findings and 11 of Python's 20.
    const RERAISE: &[(Lang, &str, &str, u16)] = &[
        (
            Lang::CSharp,
            "A.cs",
            "class A {\n void f() {\n try { g(); } catch (Exception x) { Log(x); throw; }\n }\n}\n",
            0,
        ),
        (
            Lang::CSharp,
            "A.cs",
            "class A {\n void f() {\n try { g(); } catch (Exception x) { Log(x); throw new Bad(\"no\"); }\n }\n}\n",
            1,
        ),
        (
            Lang::CSharp,
            "A.cs",
            "class A {\n void f() {\n try { g(); } catch (Exception x) { throw new Bad(\"no\", x); }\n }\n}\n",
            0,
        ),
        (
            Lang::Python,
            "a.py",
            "def f():\n    try:\n        g()\n    except ValueError as e:\n        log(e)\n        raise\n",
            0,
        ),
        (
            Lang::Python,
            "a.py",
            "def f():\n    try:\n        g()\n    except ValueError as e:\n        log(e)\n        raise Bad(\"no\")\n",
            1,
        ),
        (
            Lang::Python,
            "a.py",
            "def f():\n    try:\n        g()\n    except ValueError as e:\n        raise Bad(\"no\") from e\n",
            0,
        ),
        // A comment is a named child in every tree-sitter grammar, and
        // one written INSIDE the statement — before the semicolon — is
        // a child of the throw itself, so "raises nothing" has to be
        // judged on the children that are code.
        (
            Lang::CSharp,
            "A.cs",
            "class A {\n void f() {\n try { g(); } catch (Exception x) { throw /* keep the stack */; }\n }\n}\n",
            0,
        ),
        (
            Lang::Java,
            "A.java",
            "class A {\n void f() {\n try { g(); } catch (Exception e) { throw new Bad(\"no\"); }\n }\n}\n",
            1,
        ),
        (
            Lang::TypeScript,
            "a.ts",
            "function f() {\n try { g(); } catch (e) { throw new Error(\"no\"); }\n}\n",
            1,
        ),
    ];

    /// What a single literal argument to an assertion means. Each
    /// source declares one unit `f`, and the flag is whether it counts
    /// as vacuous — an assertion green whatever the code did.
    ///
    /// The three FALSE rows are the metric's three measured false
    /// positives: a fluent assertion whose subject is the receiver and
    /// whose literal is the expected answer (2,116 of 3,116 gold
    /// findings, and the whole reason C# read 24% of its test units), a
    /// project helper whose literal is DATA, and a curried assertion
    /// whose first application is only its subject.
    const LITERAL_ASSERT: &[(Lang, &str, &str, bool)] = &[
        (
            Lang::CSharp,
            "T.cs",
            "class T {\n void f() {\n Assert.True(true);\n }\n}\n",
            true,
        ),
        (
            Lang::CSharp,
            "T.cs",
            "class T {\n void f() {\n resultA.Name.ShouldBe(\"name1\");\n }\n}\n",
            false,
        ),
        (
            Lang::CSharp,
            "T.cs",
            "class T {\n void f() {\n order.Total.ShouldBe(3);\n }\n}\n",
            false,
        ),
        (
            Lang::Php,
            "T.php",
            "<?php\nclass T {\n function f() {\n $this->assertTrue(true);\n }\n}\n",
            true,
        ),
        (
            Lang::Elixir,
            "t.exs",
            "defmodule T do\n def f do\n assert_file(\"lib/accounts.ex\")\n end\nend\n",
            false,
        ),
        (
            Lang::Scala,
            "T.scala",
            "class T {\n def f(): Unit = assert(11)(equalTo(12))\n}\n",
            false,
        ),
        (Lang::Perl, "t.pl", "sub f {\n ok(1);\n}\n1;\n", true),
    ];

    #[test]
    fn a_literal_argument_is_the_subject_only_when_the_assertion_owns_it() {
        for (lang, name, src, want) in LITERAL_ASSERT {
            let f = facts_at(*lang, name, src);
            let u = f
                .units
                .iter()
                .find(|u| &*u.name == "f")
                .unwrap_or_else(|| panic!("{lang:?}: no unit `f` in {src:?}"));
            assert_eq!(u.vacuous_asserts > 0, *want, "{lang:?} {src:?}");
        }
    }

    /// A property fuzzer's harness is test code wherever it is filed.
    ///
    /// Echidna reads a contract whose `assert` IS the property under
    /// test: that assert failing is the finding the fuzzer exists to
    /// produce. All five gold Solidity `unwraps` findings sat in one
    /// crytic/echidna directory and every one was an invariant, so the
    /// cell's whole reading was a path the test predicate did not know.
    #[test]
    fn a_fuzz_harness_is_test_code_wherever_it_is_filed() {
        let harness = "contract E {\n function check_invariant() public {\n assert(a < b);\n assert(c < d);\n assert(e < f);\n }\n}\n";
        let fuzzed = facts_at(
            Lang::Solidity,
            "audits/tob/contracts/crytic/echidna/E2E_swap.sol",
            harness,
        );
        assert!(fuzzed.is_test_file, "a crytic/echidna path is a harness");
        // `unwraps` is emitted only for a unit that is not test code,
        // so this flag is what silences the invariant.
        assert!(
            fuzzed
                .units
                .iter()
                .filter(|u| !u.is_module)
                .all(|u| u.is_test),
            "a fuzzed invariant is the subject, not a panic where an error belonged"
        );
        // Restraint: the same three asserts in ordinary contract code
        // are still counted, so this exempts a directory and not a rule.
        let shipped = facts_at(Lang::Solidity, "contracts/Pool.sol", harness);
        assert!(!shipped.is_test_file);
        assert!(shipped.units.iter().all(|u| !u.is_test));
        assert_eq!(shipped.units.iter().map(|u| u.unwraps).sum::<u16>(), 3);
    }

    #[test]
    fn a_member_kind_is_a_method_wherever_the_parse_left_it() {
        // The ancestor walk is only as good as the parse. All three
        // grammars below accept a member declaration sitting directly
        // in a namespace, a package or a file — without an error node,
        // so nothing downstream can see that the type went missing —
        // and that is precisely what a class body the parser gave up
        // inside leaves behind.
        let orphans: &[(Lang, &str, &str)] = &[
            (
                Lang::CSharp,
                "a.cs",
                "namespace N\n{\n    public bool Ready()\n    {\n        return true;\n    }\n}\n",
            ),
            (
                Lang::Java,
                "A.java",
                "package n;\n\npublic boolean ready()\n{\n    return true;\n}\n",
            ),
        ];
        for (lang, path, src) in orphans {
            let f = facts_at(*lang, path, src);
            let u = f
                .units
                .iter()
                .find(|u| &*u.name == "ready" || &*u.name == "Ready")
                .unwrap_or_else(|| panic!("{lang:?}: no unit in {src:?}"));
            assert!(
                u.is_method,
                "{lang:?}: a member kind outside its type is still a member"
            );
        }
        // And the free function C# does have keeps its own spelling, so
        // the rule cannot swallow it.
        let f = facts_at(
            Lang::CSharp,
            "a.cs",
            "class C\n{\n    void Outer()\n    {\n        bool ready()\n        {\n            return true;\n        }\n    }\n}\n",
        );
        let u = f
            .units
            .iter()
            .find(|u| &*u.name == "ready")
            .expect("local function is a unit");
        assert!(!u.is_method, "a local function is not a member");
    }

    #[test]
    fn every_language_either_binds_by_shape_or_says_why_not() {
        // The same discipline as DECLARED_DEAD: a pack that sets the
        // flag nowhere is either a language that cannot express a
        // pattern parameter, or a pack with a hole in it, and only a
        // stated reason tells the two apart.
        for lang in super::LANGS {
            let shaped = PATTERN_PARAMS.iter().any(|(l, ..)| *l == lang);
            let flat = PATTERNLESS.iter().any(|(l, _)| *l == lang);
            assert!(
                shaped ^ flat,
                "{lang:?}: pattern parameters unaccounted for"
            );
        }
    }

    #[test]
    fn a_re_export_is_an_import() {
        // A barrel file states its dependencies with `export ... from`
        // and nothing else. Reading only `import` left immer's
        // internal.ts with eleven re-exports and no edges at all, so
        // the eight modules it fronts read as orphans and the whole
        // repository as 91% deletable.
        const SRC: &str = concat!(
            "export * from \"./leaf\";\n",
            "export { two as second } from \"./leaf2\";\n",
            "export * as ns from \"./leaf3\";\n",
            "export function own() {}\n",
            "export default own;\n",
        );
        for (lang, path) in [(Lang::TypeScript, "barrel.ts"), (Lang::Tsx, "barrel.tsx")]
            .into_iter()
            .chain([(Lang::JavaScript, "barrel.js")])
        {
            let f = facts_at(lang, path, SRC);
            let targets: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
            assert_eq!(
                targets,
                ["./leaf", "./leaf2", "./leaf3"],
                "{path}: an export without a source depends on nothing"
            );
            let bound: Vec<&str> = f
                .imports
                .iter()
                .flat_map(|i| &i.names)
                .map(|n| &**n)
                .collect();
            assert_eq!(bound, ["second", "ns"], "{path}");
        }
    }

    #[test]
    fn a_swift_import_names_a_module_and_nothing_else() {
        // The target was the declaration's raw text with `import`
        // trimmed off the front, so an attribute or a declaration kind
        // rode along into it: `@testable import NIOPosix` 306 times
        // and `import struct Foundation.Data` 217 across gold Swift,
        // neither of which can ever name anything.
        let f = facts_at(
            Lang::Swift,
            "Sources/App/main.swift",
            concat!(
                "import Foundation\n",
                "@testable import NIOPosix\n",
                "import struct Foundation.Data\n",
                "import NIOCore\n",
            ),
        );
        let targets: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(targets, ["Foundation", "NIOPosix", "Foundation", "NIOCore"]);
    }

    #[test]
    fn an_autoload_is_a_deferred_require() {
        // `autoload :Base, 'rack/protection/base'` loads the file when
        // the constant is first touched, and rack-protection states 18
        // of its dependencies exactly that way. The pack's own doc
        // comment claimed the form; `requires` never matched it.
        let f = facts_at(
            Lang::Ruby,
            "protection.rb",
            concat!(
                "autoload :Base, 'rack/protection/base'\n",
                "require 'rack'\n",
                "config.autoload :Nope, 'not/mine'\n",
            ),
        );
        let targets: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(targets, ["rack/protection/base", "rack"]);
    }

    #[test]
    fn a_guarded_require_is_still_a_require() {
        // A module that tolerates a missing dependency writes
        // `pcall(require, "x")`. The callee is pcall, so the pack read
        // an ordinary call and 132 of these across gold Lua emitted no
        // edge — while `pcall` around anything else stays a call.
        let f = facts_at(
            Lang::Lua,
            "m.lua",
            concat!(
                "local ok, x = pcall(require, \"dkjson\")\n",
                "local y = require \"cjson\"\n",
                "local z = pcall(tostring, \"nope\")\n",
            ),
        );
        let targets: Vec<&str> = f.imports.iter().map(|i| &*i.target).collect();
        assert_eq!(targets, ["dkjson", "cjson"]);
    }

    #[test]
    fn a_dot_h_is_read_as_the_dialect_it_is_written_in() {
        // Reading every `.h` as C dropped a third of every C++ repository
        // as unparseable — headers are where C++ keeps its classes.
        // Reading every `.h` as C++ parses fine and then files musl's 655
        // headers under `cpp`, which calibrates one language on another.
        // So the text decides, on four line-anchored spellings.
        let h = std::path::Path::new("port/port.h");
        let cases: &[(&str, Option<Lang>, &str)] = &[
            (
                "#ifndef PORT_H_\nnamespace leveldb {\nclass Slice;\n}\n",
                Some(Lang::Cpp),
                "a namespace is not C",
            ),
            (
                "template <typename T>\nT max(T a, T b) { return a > b ? a : b; }\n",
                Some(Lang::Cpp),
                "nor is a template",
            ),
            (
                "struct Cfg {\npublic:\n  int n;\n};\n",
                Some(Lang::Cpp),
                "nor an access specifier",
            ),
            (
                "#define LEVELDB_EXPORT __attribute__((visibility(\"default\")))\n",
                Some(Lang::C),
                "a macro-only header holds no C++ and reads as C",
            ),
            (
                "/* the C API: see leveldb::DB and the class template */\nvoid leveldb_open(void);\n",
                Some(Lang::C),
                "a MENTION inside a comment must not vote — hence line anchors",
            ),
        ];
        for (src, want, why) in cases {
            assert_eq!(Lang::of_source(h, src), *want, "{why}");
        }
        // The extension still decides everywhere it can.
        let unambiguous = [("a.c", Lang::C), ("a.cc", Lang::Cpp), ("a.hpp", Lang::Cpp)];
        for (name, want) in unambiguous {
            let path = std::path::Path::new(name);
            assert_eq!(Lang::of_source(path, "namespace x {}\n"), Some(want));
            assert_eq!(Lang::from_path(path), Some(want));
        }
        // CUDA goes through the same reader, and a comment mention must
        // not vote: "never call __device__ code from here" is a sentence
        // a HOST file writes about the boundary it sits on. The first
        // version of the sniff read it as a kernel.
        assert_eq!(
            Lang::of_source(
                std::path::Path::new("disp.cpp"),
                "// never call __device__ functions from this file\nnamespace d { void route(); }\n",
            ),
            Some(Lang::Cpp),
            "a comment about CUDA is not CUDA"
        );
        assert_eq!(
            Lang::of_source(
                std::path::Path::new("step.h"),
                "__global__ void step(float* xs, int n);\n",
            ),
            Some(Lang::Cuda),
            "a kernel declaration in a header is — flash-attention's launch templates live in .h"
        );
    }

    #[test]
    fn every_pack_name_resolves_in_its_grammar() {
        // The Zig @import lesson: an unmapped kind name silently zeroes a
        // metric, calibration then pins that zero, and the zero-guard
        // skips it — invisible decay. A grammar bump must fail HERE.
        let mut broken = Vec::new();
        for lang in super::LANGS {
            for name in lang.pack().unresolved() {
                broken.push(format!("{lang:?}: {name}"));
            }
        }
        assert!(broken.is_empty(), "unresolved grammar names:\n{broken:#?}");
    }

    #[test]
    fn grammar_abi_versions_are_pinned() {
        // Bumping a tree-sitter grammar crate must be a conscious act:
        // update this table and re-run the conformance suite.
        let pinned: Vec<String> = super::LANGS
            .iter()
            .map(|l| format!("{} {}", l.name(), l.pack().ts.abi_version()))
            .collect();
        assert_eq!(
            pinned.join(", "),
            "py 15, rs 15, ts 14, tsx 14, go 15, js 15, zig 14, lua 15, rb 14, pl 15, php 15, java 14, cs 15, swift 15, scala 15, ex 14, sol 15, c 15, ml 14, sh 15, cpp 14, cu 15"
        );
    }

    #[test]
    fn lang_discriminants_match_langs_order() {
        // Every per-language array indexes by `Lang as usize`; LANGS order
        // and enum declaration order must never drift apart.
        for (i, l) in super::LANGS.iter().enumerate() {
            assert_eq!(*l as usize, i, "{l:?}");
        }
    }

    #[test]
    fn identical_logic_produces_identical_clone_hashes_within_language() {
        // Type-2: renamed identifiers and changed literals must not matter.
        let f = facts(
            Lang::Rust,
            "
fn alpha(xs: Vec<i64>) -> i64 {
    let mut acc = 0;
    for x in xs {
        if x > 10 {
            acc += x;
        }
    }
    acc
}

fn beta(items: Vec<i64>) -> i64 {
    let mut total = 0;
    for value in items {
        if value > 999 {
            total += value;
        }
    }
    total
}
",
        );
        let dup: Vec<_> = f
            .clone_sites
            .iter()
            .filter(|s| f.clone_sites.iter().filter(|t| t.hash == s.hash).count() >= 2)
            .collect();
        assert!(!dup.is_empty(), "renamed twin functions must collide");
    }
}
