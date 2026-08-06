//! Hook reachability: proof that the core ASKS what a pack answers.
//!
//! Nine detectors died at once and all nine died the same way — a pack
//! answered a question the core never put to it. `swallows_error` was
//! written for catch clauses and consulted on `if`; `loses_context` was
//! written against `throw`, a jump the core never hands to a pack;
//! `unguarded_resource` was written against a declaration where the core
//! asks about calls; `spooky` was asked of calls and typedefs only, so
//! Solidity's `assembly` and Perl's `eval "..."` went unseen. Every one
//! compiled, passed and shipped silent, because a hook that is never
//! consulted and a hook that correctly finds nothing produce the same
//! zero.
//!
//! So the two are separated here. Every hook the core owns is asked
//! through a method on [`Pack`], and each method records that the
//! question was PUT and whether it was ANSWERED — a `true`, a `Some`, a
//! non-empty list, a non-zero width. The pack fields themselves are
//! private, so there is no route to a hook that skips the ledger: a
//! `called` of zero is proof of unreachability rather than evidence of
//! it, whatever else the process did.
//!
//! Counting is live under `debug_assertions` and compiled out otherwise,
//! the same bargain [`crate::report::Gated`] makes. A release binary
//! built with `-C debug-assertions=on` therefore measures a whole corpus
//! at release speed.

use std::sync::atomic::{AtomicU64, Ordering};

use tree_sitter::Node;

use super::{CatchSin, ImportInfo, Lang, Pack, ParamInfo};
use crate::facts::InterfaceFact;
use crate::sem::Sem;

/// What counts as an ANSWER. A hook says something when it returns a
/// `true`, a `Some`, a list with anything in it, or a non-zero width;
/// everything else is the silence that has to be explained.
trait Answered {
    fn answered(&self) -> bool;
}

impl Answered for bool {
    fn answered(&self) -> bool {
        *self
    }
}

impl<T> Answered for Option<T> {
    fn answered(&self) -> bool {
        self.is_some()
    }
}

impl<T> Answered for Vec<T> {
    fn answered(&self) -> bool {
        !self.is_empty()
    }
}

impl Answered for u16 {
    fn answered(&self) -> bool {
        *self != 0
    }
}

/// Declare the hooks once: the variant, the name, and the asking method
/// all come from this list, so none of the three can drift against
/// another. `refine` is spelled out below rather than generated — its
/// answer is a CHANGE to the table's verdict, which no return value
/// alone can report.
macro_rules! hooks {
    ($( $field:ident $(<$lt:lifetime>)? ( $($arg:ident : $ty:ty),* ) -> $ret:ty ; )*) => {
        /// Every question the core puts to a pack, named as the field is
        /// named, because that is what a reader greps for.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[allow(non_camel_case_types)]
        pub enum Hook {
            refine,
            $($field),*
        }

        /// Declaration order, which is also the order the table prints.
        pub const HOOKS: &[Hook] = &[Hook::refine, $(Hook::$field),*];

        impl Hook {
            pub fn name(self) -> &'static str {
                match self {
                    Hook::refine => "refine",
                    $(Hook::$field => stringify!($field)),*
                }
            }
        }

        impl Pack {
            $(
                #[doc = concat!(
                    "Put the `", stringify!($field), "` question to this pack, \
                     recording that it was asked. See the field of the same name."
                )]
                pub fn $field $(<$lt>)? (&self, $($arg: $ty),*) -> $ret {
                    let out = (self.$field)($($arg),*);
                    note(self.lang, Hook::$field, Answered::answered(&out));
                    out
                }
            )*
        }
    };
}

hooks! {
    name_node<'t>(node: Node<'t>) -> Option<Node<'t>>;
    composed_name(node: Node, src: &[u8]) -> Option<String>;
    imports(node: Node, src: &[u8]) -> Vec<ImportInfo>;
    param_info(node: Node, src: &[u8]) -> Option<ParamInfo>;
    is_self_call(node: Node, src: &[u8], name: &str) -> bool;
    is_doc(node: Node) -> bool;
    is_public(node: Node, src: &[u8]) -> bool;
    doc_span(node: Node, src: &[u8]) -> Option<(u32, u32)>;
    is_override(node: Node, src: &[u8]) -> bool;
    spooky(node: Node, sem: Sem, src: &[u8]) -> bool;
    negation_operand<'t>(node: Node<'t>, src: &[u8]) -> Option<Node<'t>>;
    catch_sin(node: Node, src: &[u8]) -> Option<CatchSin>;
    swallows_error(node: Node, src: &[u8]) -> bool;
    loses_context(node: Node, src: &[u8]) -> bool;
    panicky(node: Node, src: &[u8]) -> bool;
    record_keys(node: Node, src: &[u8]) -> Option<Vec<Box<str>>>;
    unguarded_resource(node: Node, src: &[u8]) -> bool;
    is_async(node: Node, src: &[u8]) -> bool;
    declares_test(node: Node, src: &[u8]) -> bool;
    names_test(node: Node, src: &[u8]) -> bool;
    is_test_code(node: Node, src: &[u8]) -> bool;
    test_path(path: &str) -> bool;
    asserty(node: Node, src: &[u8]) -> bool;
    is_hook(node: Node, src: &[u8]) -> bool;
    return_arity(node: Node, src: &[u8]) -> u16;
    skips_test(node: Node, src: &[u8]) -> bool;
    interfaces(node: Node, src: &[u8]) -> Vec<InterfaceFact>;
}

const N_HOOKS: usize = HOOKS.len();

/// (asked, answered) per (language, hook). Relaxed adds from every
/// worker: the numbers are evidence of reachability, and no reader cares
/// which file got there first.
static TALLY: [[(AtomicU64, AtomicU64); N_HOOKS]; super::LANGS.len()] =
    [const { [const { (AtomicU64::new(0), AtomicU64::new(0)) }; N_HOOKS] }; super::LANGS.len()];

/// One question asked, and whether it was answered.
pub(super) fn note(lang: Lang, hook: Hook, answered: bool) {
    if !cfg!(debug_assertions) {
        return;
    }
    let (asked, said) = &TALLY[lang as usize][hook as usize];
    asked.fetch_add(1, Ordering::Relaxed);
    if answered {
        said.fetch_add(1, Ordering::Relaxed);
    }
}

/// How often this pack was asked this question, and how often it said
/// something. Zero and zero in a release build means only that nobody
/// was counting.
pub fn tally(lang: Lang, hook: Hook) -> (u64, u64) {
    let (asked, said) = &TALLY[lang as usize][hook as usize];
    (asked.load(Ordering::Relaxed), said.load(Ordering::Relaxed))
}

/// The whole ledger, one row per (language, hook) that was ever asked.
pub fn table() -> String {
    let mut out = String::from("lang\thook\tasked\tanswered\n");
    for lang in super::LANGS {
        for hook in HOOKS {
            let (asked, said) = tally(lang, *hook);
            if asked > 0 {
                out.push_str(&format!(
                    "{}\t{}\t{asked}\t{said}\n",
                    lang.name(),
                    hook.name()
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod reachable {
    //! The parity matrix, one level down. `every_detector_is_seeded_alive_
    //! or_declared_dead` proves a METRIC fires; these prove the core
    //! reaches the HOOK the metric's liveness rests on, which is the
    //! layer the nine silent deaths happened at.
    //!
    //! Every assertion here is `> 0`, and that is deliberate: tests share
    //! a process and a counter, so another test's extraction can only
    //! ever ADD to a tally. A "must be reached" claim is safe under that;
    //! a "must stay silent" claim would be decided by the scheduler.

    use super::{HOOKS, Hook, tally};
    use crate::facts::extract;
    use crate::lang::{LANGS, Lang};
    use std::path::Path;

    /// Which hooks carry a count-shaped detector's liveness ENTIRELY: if
    /// none of them ever answers for a language, the detector reads zero
    /// there whatever the code says. Reconciled below against the same
    /// `DECLARED_DEAD` table the metric-level matrix uses.
    ///
    /// `swallowed` has two, because a language without a handler node
    /// swallows in an `if` instead and only one of the pair can apply.
    ///
    /// Detectors are absent from this list when a hook is not their only
    /// evidence: `test asserts` and `vacuous asserts` also come from
    /// `Sem::Assert`, so a language whose `assert` is a STATEMENT counts
    /// them with a silent `asserty`; `spooky` also counts a mutable
    /// parameter default. Nothing is claimed about those here.
    const DECIDED_BY: &[(&str, &[Hook])] = &[
        ("spooky", &[Hook::spooky]),
        ("swallowed", &[Hook::swallows_error, Hook::catch_sin]),
        ("broad catch", &[Hook::catch_sin]),
        ("lost context", &[Hook::loses_context]),
        ("unwraps", &[Hook::panicky]),
        ("unmanaged", &[Hook::unguarded_resource]),
        ("negations", &[Hook::negation_operand]),
        ("blocking async", &[Hook::is_async]),
        ("conditional hook", &[Hook::is_hook]),
        ("skipped tests", &[Hook::skips_test]),
    ];

    /// Hooks every one of the 22 packs implements for real, against a
    /// construct every one of the 22 languages has — so per-PACK
    /// reachability is a claim worth making rather than a fixture
    /// accident. Everywhere else the honest statement is the corpus-wide
    /// one above, because most hooks are a deliberate stub in most packs
    /// and most fixtures are too small to carry every node kind.
    ///
    /// The list is short and earned it. Lua's and Ruby's `imports` were
    /// written against `require`, which is a CALL in both languages,
    /// while the core asks about imports at `Sem::Import` nodes: over
    /// the whole gold corpus the two hooks were asked exactly zero
    /// times, and neither language had a module graph at all. Nothing
    /// else in the suite could see that, because a pack that answers
    /// nothing and a pack nobody asks look identical from outside.
    const ASKED_OF_EVERY_PACK: &[Hook] = &[Hook::imports];

    /// Run every recall fixture through the core, which is the only
    /// thing that puts a question to a pack.
    ///
    /// The two dialect packs get the base language's fixtures: TSX is
    /// the TypeScript pack under a JSX-aware grammar and CUDA is the C++
    /// pack under a launch-aware one, so they share hooks but not
    /// tables, and only running them proves the tables still reach.
    fn ask_everything() {
        for (file, lang, src) in crate::recall::FIXTURES {
            let dialect = match lang {
                Lang::TypeScript => Some((Lang::Tsx, "tsx")),
                Lang::Cpp => Some((Lang::Cuda, "cu")),
                _ => None,
            };
            for (lang, path) in [(*lang, (*file).to_string())]
                .into_iter()
                .chain(dialect.map(|(l, ext)| (l, format!("{file}.{ext}"))))
            {
                let pack = lang.pack();
                let mut parser = pack.make_parser();
                extract(pack, &mut parser, Path::new(&path), src);
            }
        }
    }

    #[test]
    fn every_hook_is_asked_and_answered_somewhere() {
        // A hook nothing ever asks is written against a node the core
        // does not consult — the whole bug class, in one line. A hook
        // asked everywhere and answered nowhere is the same bug seen
        // from the other side: the question reaches the pack, but never
        // carrying the node the pack was written for.
        ask_everything();
        let (mut unasked, mut mute) = (Vec::new(), Vec::new());
        for hook in HOOKS {
            let (asked, said) = LANGS
                .iter()
                .map(|l| tally(*l, *hook))
                .fold((0, 0), |(a, s), (a2, s2)| (a + a2, s + s2));
            if asked == 0 {
                unasked.push(hook.name());
            } else if said == 0 {
                mute.push(hook.name());
            }
        }
        assert!(
            unasked.is_empty(),
            "the core never consults these hooks: {unasked:?}"
        );
        assert!(
            mute.is_empty(),
            "these hooks are consulted and never answer, in any language: {mute:?}"
        );
    }

    #[test]
    fn a_universal_hook_reaches_every_pack() {
        ask_everything();
        let mut broken = Vec::new();
        for hook in ASKED_OF_EVERY_PACK {
            for lang in LANGS {
                let (asked, said) = tally(lang, *hook);
                if asked == 0 || said == 0 {
                    broken.push(format!(
                        "{lang:?} x {}: asked {asked}, answered {said} — every \
                         language has this construct and every pack claims to \
                         read it",
                        hook.name()
                    ));
                }
            }
        }
        assert!(broken.is_empty(), "{}", broken.join("\n"));
    }

    #[test]
    fn a_live_detector_has_a_hook_that_answers_for_its_language() {
        // The per-language half. A detector the parity matrix does not
        // declare dead must have evidence to read, and for these it is
        // the hook alone: `spooky` was asked of Solidity's calls and
        // never of its `assembly` block, so the pack answered and the
        // core never heard it.
        ask_everything();
        let mut broken = Vec::new();
        for lang in LANGS {
            // Same rule the metric-level matrix uses: a dialect follows
            // the evidence of the pack it IS.
            let evidence = match lang {
                Lang::Tsx => Lang::TypeScript,
                Lang::Cuda => Lang::Cpp,
                l => l,
            };
            for (metric, hooks) in DECIDED_BY {
                let dead = crate::lang::conformance::DECLARED_DEAD
                    .iter()
                    .any(|(l, m, _)| *l == evidence && m == metric);
                if dead || hooks.iter().any(|h| tally(lang, *h).1 > 0) {
                    continue;
                }
                let asked: u64 = hooks.iter().map(|h| tally(lang, *h).0).sum();
                let names: Vec<&str> = hooks.iter().map(|h| h.name()).collect();
                broken.push(format!(
                    "{lang:?} x {metric}: alive in the matrix, but {names:?} \
                     answered nothing in {asked} questions"
                ));
            }
        }
        assert!(broken.is_empty(), "{}", broken.join("\n"));
    }
}
