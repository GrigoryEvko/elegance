//! The macro names a C++ project already declares to clang-format.
//!
//! A macro in a declarator is the largest cause of a parse error that
//! is left in C++ once the standard syntax reads. `Arena& a
//! CRUCIBLE_LIFETIMEBOUND` holds a token the grammar has no rule for,
//! and the error swallows the rest of its scope the way every other
//! parse error does.
//!
//! The name is the only thing that identifies such a token, and no
//! heuristic reads it safely. A rule about an uppercase word in a
//! declarator is wrong before it is written, because `SEC("license")`
//! stands between `]` and `=` and not after `)`. A rule that is broad
//! enough to catch every shape also blanks real work, and that failure
//! is silent.
//!
//! A project that has this problem has already written the names down.
//! clang-format cannot format one of these macros without them either,
//! so `.clang-format` carries the list, and this module reads it. The
//! result is no new configuration key and no guess.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use yaml_rust2::{Yaml, YamlLoader};

/// What a macro stands for, which decides what it becomes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Expands to an attribute. It carries no complexity, so it goes.
    Attribute,
    /// Stands before the expression of a statement and expands to nothing
    /// that counts, as Qt's `emit` does. Anywhere else the word is a name.
    StatementAttribute,
    /// Stands where a statement or a declaration stands.
    Statement,
    /// Opens a loop over a range.
    ForEach,
    /// Opens a branch.
    Branch,
    /// Stands where a type stands, and its argument is that type.
    Typename,
    /// Opens a namespace.
    Namespace,
}

/// The names one `.clang-format` declares, by what they stand for.
pub struct Macros {
    names: Vec<(Box<str>, Kind)>,
    /// The first byte of every name. Most words in a file start with a
    /// byte that no macro starts with, and this answers for those
    /// without a comparison.
    first: [bool; 256],
}

impl Default for Macros {
    fn default() -> Macros {
        Macros {
            names: Vec::new(),
            first: [false; 256],
        }
    }
}

impl Macros {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// What this word stands for, when a declaration names it.
    pub fn kind(&self, word: &[u8]) -> Option<Kind> {
        if !self.first[*word.first()? as usize] {
            return None;
        }
        self.names
            .iter()
            .find(|(name, _)| name.as_bytes() == word)
            .map(|(_, kind)| *kind)
    }

    /// The names in one `.clang-format`. A key this tool has no rewrite
    /// for is left alone, and so is a name that is not an identifier: a
    /// rewrite keys on the word a declaration spells, and nothing else
    /// can be one.
    pub fn read(text: &str) -> Macros {
        let mut macros = Macros::default();
        let Ok(docs) = YamlLoader::load_from_str(text) else {
            return macros;
        };
        for doc in &docs {
            let Yaml::Hash(map) = doc else { continue };
            for (key, value) in map {
                let kind = match key.as_str() {
                    Some("AttributeMacros") => Kind::Attribute,
                    Some("StatementAttributeLikeMacros") => Kind::StatementAttribute,
                    Some("StatementMacros") => Kind::Statement,
                    Some("ForEachMacros") => Kind::ForEach,
                    Some("IfMacros") => Kind::Branch,
                    Some("TypenameMacros") => Kind::Typename,
                    Some("NamespaceMacros") => Kind::Namespace,
                    _ => continue,
                };
                let Yaml::Array(items) = value else { continue };
                for item in items {
                    if let Some(name) = item.as_str().filter(|n| is_identifier(n)) {
                        macros.first[name.as_bytes()[0] as usize] = true;
                        macros.names.push((name.into(), kind));
                    }
                }
            }
        }
        macros
    }
}

/// True when every byte of the name can stand in an identifier. A
/// rewrite matches a whole word, so nothing else can ever match.
fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Every `.clang-format` under the scan root, nearest-wins.
///
/// clang-format reads the nearest one above the file it formats, and a
/// repository that keeps one per component means that. The lookup is
/// the longest matching prefix, which is what the budget layers do.
#[derive(Default)]
pub struct Registry {
    dirs: Vec<(PathBuf, Arc<Macros>)>,
}

impl Registry {
    pub fn discover(root: &Path) -> Registry {
        let mut dirs: Vec<(PathBuf, Arc<Macros>)> = Vec::new();
        let read = |dir: &Path| {
            let text = std::fs::read_to_string(dir.join(".clang-format")).ok()?;
            let macros = Macros::read(&text);
            (!macros.is_empty()).then(|| Arc::new(macros))
        };

        // At or above the root, nearest wins, the way clang-format
        // reads the nearest file at or above the one it formats.
        // `--errors one/file.h` names a FILE as the root, and
        // `elegance src/` names a directory below the one that holds
        // the declaration. Neither finds it by a walk downward.
        //
        // A declaration at or above the root governs every file in the
        // scan, so it is recorded under the empty path, which is a
        // prefix of every path. Recording it under the root instead
        // would rest on how the root is spelled: `.` is not a prefix of
        // `include/x.h`, only of `./include/x.h`. A canonical path does
        // not serve either, because the scan yields the paths it was
        // given. The empty key also sorts below every real directory,
        // so a nested declaration still wins.
        let base = match root.is_file() {
            true => root.parent().unwrap_or(Path::new("")),
            false => root,
        };
        let mut taken = None;
        if let Some(abs) = std::fs::canonicalize(base).ok()
            && let Some((dir, here)) = abs
                .ancestors()
                .find_map(|dir| read(dir).map(|macros| (dir.to_path_buf(), macros)))
        {
            taken = Some(dir);
            dirs.push((PathBuf::new(), here));
        }

        // And below it, for a repository that keeps one per component.
        // `.clang-format` is a dotfile and the walker hides those by
        // default, so without this the scan would never find one.
        for entry in ignore::WalkBuilder::new(root)
            .hidden(false)
            .build()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            // The file the upward pass already took is skipped, so that
            // one declaration is never counted or stored twice.
            if path.file_name().is_some_and(|n| n == ".clang-format")
                && let Some(dir) = path.parent()
                && std::fs::canonicalize(dir).ok().as_ref() != taken.as_ref()
                && let Some(macros) = read(dir)
            {
                dirs.push((dir.to_path_buf(), macros));
            }
        }
        Registry { dirs }
    }

    pub fn for_file(&self, path: &Path) -> Option<&Macros> {
        self.dirs
            .iter()
            .filter(|(dir, _)| path.starts_with(dir))
            .max_by_key(|(dir, _)| dir.components().count())
            .map(|(_, macros)| &**macros)
    }

    /// How many declarations the scan found, for the reader of a report.
    pub fn count(&self) -> usize {
        self.dirs.iter().map(|(_, m)| m.names.len()).sum()
    }
}

static INSTALLED: OnceLock<Registry> = OnceLock::new();
static NONE: OnceLock<Macros> = OnceLock::new();

/// Record what the scan found, one time, before any file is read.
pub fn install(registry: Registry) {
    let _ = INSTALLED.set(registry);
}

/// The macros in force where this file lives. A project that declares
/// none gets an empty set, and an empty set costs the rewrite one test.
///
/// This is read through a global because `measurable` is the one seam
/// every read passes through, and six entry points call it. Threading a
/// list that is empty for most repositories through all of them would
/// say less than this does.
pub fn for_file(path: &Path) -> &'static Macros {
    INSTALLED
        .get()
        .and_then(|registry| registry.for_file(path))
        .unwrap_or_else(|| NONE.get_or_init(Macros::default))
}

#[cfg(test)]
mod tests {
    use super::{Kind, Macros};

    #[test]
    fn every_key_this_tool_rewrites_is_read() {
        let macros = Macros::read(
            "---\n\
             AttributeMacros: [KEEP_ALIVE, HOT]\n\
             StatementAttributeLikeMacros: [EMIT]\n\
             StatementMacros: [LAYOUT_INVARIANT]\n\
             ForEachMacros: [for_each_program]\n\
             IfMacros: [IF_MAYBE]\n\
             TypenameMacros: [STACK_OF]\n\
             NamespaceMacros: [TESTSUITE]\n\
             ColumnLimit: 120\n",
        );
        assert_eq!(macros.kind(b"KEEP_ALIVE"), Some(Kind::Attribute));
        assert_eq!(macros.kind(b"HOT"), Some(Kind::Attribute));
        assert_eq!(macros.kind(b"EMIT"), Some(Kind::StatementAttribute));
        assert_eq!(macros.kind(b"LAYOUT_INVARIANT"), Some(Kind::Statement));
        assert_eq!(macros.kind(b"for_each_program"), Some(Kind::ForEach));
        assert_eq!(macros.kind(b"IF_MAYBE"), Some(Kind::Branch));
        assert_eq!(macros.kind(b"STACK_OF"), Some(Kind::Typename));
        assert_eq!(macros.kind(b"TESTSUITE"), Some(Kind::Namespace));
        assert_eq!(macros.kind(b"ColumnLimit"), None, "a key is not a name");
        assert_eq!(macros.kind(b"120"), None);
    }

    #[test]
    fn a_file_that_declares_nothing_reads_as_empty() {
        assert!(Macros::read("ColumnLimit: 100\nIndentWidth: 4\n").is_empty());
        assert!(Macros::read("").is_empty());
        assert!(Macros::read("[ this is not yaml").is_empty());
    }

    #[test]
    fn a_name_that_cannot_be_a_word_is_not_read() {
        // A rewrite matches a whole word. Anything else would never
        // match, and a `*` in the list must not become one that does.
        let macros = Macros::read("AttributeMacros: ['*', '1UP', 'a b', 'OK_']\n");
        assert_eq!(macros.kind(b"OK_"), Some(Kind::Attribute));
        assert_eq!(macros.kind(b"*"), None);
        assert_eq!(macros.kind(b"1UP"), None);
        assert_eq!(macros.kind(b"a b"), None);
    }

    #[test]
    fn the_language_sections_of_one_file_are_read_together() {
        // clang-format writes one document for each language, and a
        // name is distinctive enough that the union of them serves.
        let macros = Macros::read(
            "---\nLanguage: Cpp\nAttributeMacros: [CPP_ONLY]\n\
             ---\nLanguage: ObjC\nAttributeMacros: [OBJC_ONLY]\n",
        );
        assert_eq!(macros.kind(b"CPP_ONLY"), Some(Kind::Attribute));
        assert_eq!(macros.kind(b"OBJC_ONLY"), Some(Kind::Attribute));
    }
}
