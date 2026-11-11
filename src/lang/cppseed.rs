//! The seed of a C++ parse: the names that a project declares as a type
//! or as a template, and the names that it defines as a macro.
//!
//! The external scanner of the C++ fork records the names that ONE FILE
//! declares. That record cannot hold a name from a header, because no
//! header is parsed. So `class Foo` in a header and `Foo bar(x);` in a
//! source file are two files, and at the second one the scanner knows
//! nothing about `Foo`. A seed gives the scanner the names of the whole
//! project, read one time from a file on disk.
//!
//! A FACT OF THE PROJECT IS THE ONLY THING THAT DECIDES SOME POSITIONS.
//! `A(x)` at statement position is a call or a declaration of `x` with
//! the type `A`, and the text of the file answers neither. The project
//! answers it. That is what a seed is for, and a parser with no seed
//! reads such a position by a default that is wrong part of the time.
//!
//! WHERE THE FILE COMES FROM. `cargo xtask seed collect ROOT LIST
//! --out-dir DIRECTORY` of the tree-sitter-cpp fork writes one
//! `<project>.seed` for each project. Put the file of this project at
//! `.elegance/cpp.seed`, or name it with `--cpp-seed PATH`.
//!
//! ONE PROJECT, ONE SEED. A seed of a project unioned with the seeds of
//! its dependencies is wrong 16 times in 20, measured over 87
//! cross-project pairs, because a name that two projects declare
//! differently then takes the reading of the wrong one.
//!
//! THE FORMAT LIVES IN THE FORK, AND THIS FILE READS IT. `src/seed.h` of
//! the fork declares the structs and the bits, and `xtask/src/seed.rs`
//! holds the reader that the collector itself uses. This module is a
//! second reader in a second repository, so the two can drift. EACH WAY
//! THEY CAN DRIFT FAILS LOUDLY:
//!
//! - The version comes from the linked scanner through
//!   `tree_sitter_cpp::seed_version()`, so it is never a constant here.
//! - A kind word that this reader does not know is an error, never a
//!   name with fewer bits.
//! - A name longer than [`MAX_NAME`] is an error for the whole file,
//!   never a dropped row.
//!
//! A seed that silently loses names gives a parse that looks correct and
//! is not, and no ERROR node and no metric shows it.

use std::ffi::c_void;
use std::path::Path;
use std::sync::OnceLock;

/// The longest name of a seed, in bytes.
///
/// The scanner reads a name into a buffer of `TS_CPP_SEED_WORD_SIZE`
/// bytes, 65, which `src/seed.h` of the fork declares, and the last byte
/// holds the end of the string. A longer name can never match, and the
/// collector drops it. A file that holds one comes from a tool that does
/// not agree with this reader, so the reader refuses the whole file.
///
/// THIS NUMBER IS THE ONE VALUE OF THE FORMAT THAT THIS FILE REPEATS.
/// The crate exports the version and no size, so a change of the size in
/// the fork must change this line too. The failure is loud: the fork
/// then writes longer names, and [`Seed::read`] refuses the file and
/// names this constant.
const MAX_NAME: usize = 64;

/// The first four bytes of the seed struct, `TSSD` in the order of the
/// bytes of the machine.
const MAGIC: u32 = u32::from_le_bytes(*b"TSSD");

/// The words of the kinds cell of a row, in the order that a row writes
/// them, with their bits. `TS_CPP_SEED_*` of `src/seed.h`.
///
/// The collector writes the words in this order, and this reader refuses
/// a cell in a different order, so one set of kinds has one text.
/// `template` holds the bit of `type`, because a template is a type.
const KIND_WORDS: [(&str, u16); 13] = [
    ("type", 1),
    ("template", 1 | 2),
    ("object-macro", 4),
    ("function-macro", 8),
    ("body-empty", 16),
    ("body-name", 32),
    ("body-specifier", 64),
    ("body-type", 128),
    ("body-scope", 256),
    ("body-member", 512),
    ("body-initializer", 1024),
    ("body-call", 2048),
    ("body-other", 4096),
];

/// One name of a seed, the layout of `TSCppSeedEntry` of `src/seed.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Entry {
    /// The start of the name in the text block.
    offset: u32,
    /// The length of the name in bytes, with no end of the string.
    length: u16,
    /// The kinds of the name, as the bits of [`KIND_WORDS`].
    kinds: u16,
}

/// The head of a seed, the layout of `TSCppSeed` of `src/seed.h`. The
/// scanner reads it through the context of the parser.
#[repr(C)]
struct Head {
    magic: u32,
    version: u32,
    count: u32,
    text_size: u32,
    entries: *const Entry,
    text: *const u8,
    id: [u8; 32],
}

/// A seed that a parser can take.
///
/// The head points into the two buffers of this struct, and neither
/// buffer moves while the struct lives.
pub struct Seed {
    /// The entries, sorted by the bytes of the name. The head points at
    /// them.
    _entries: Vec<Entry>,
    /// The names, one after the other, with no separator. The head
    /// points at them.
    _text: Vec<u8>,
    /// The head. The box keeps its address when the struct moves.
    head: Box<Head>,
    id: [u8; 32],
    names: usize,
}

// SAFETY: the struct holds no interior mutability, and nothing writes it
// after `read` gives it. The two pointers of the head refer to the heap
// buffers of this same struct, which live as long as it does. So a
// shared reference is safe to read from more than one thread, which the
// rayon walk needs.
unsafe impl Send for Seed {}
unsafe impl Sync for Seed {}

impl Seed {
    /// Read a seed file. O(n) in the bytes of the file.
    ///
    /// The reader refuses every file that a later reader cannot binary
    /// search: a first row that is not the format row of the version the
    /// linked scanner reads, a row that is not `name<TAB>kinds`, a kinds
    /// cell that is not the words of [`KIND_WORDS`] in their order, a
    /// name of more than [`MAX_NAME`] bytes, and an order that does not
    /// ascend by the bytes of the name.
    ///
    /// THE VERSION CHECK IS THE REASON THIS FUNCTION CAN FAIL AT ALL.
    /// The runtime gives the context to the scanner through a function
    /// that returns nothing, so a scanner that meets a seed of a version
    /// it does not read gives NO NAME, with no message and no ERROR
    /// node. A run of that kind reads exactly like a run with no seed.
    /// So this reader asks the scanner for its version first and refuses
    /// the file by name.
    pub fn read(path: &Path) -> Result<Self, String> {
        let version = tree_sitter_cpp::seed_version();
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read the C++ seed {}: {e}", path.display()))?;
        let text =
            std::str::from_utf8(&bytes).map_err(|e| format!("the C++ seed {} is not UTF-8: {e}", path.display()))?;
        let first = text.lines().next().unwrap_or("");
        let found = first.strip_prefix("# seed format ");
        if found != Some(&version.to_string()) {
            return Err(format!(
                "{}:1: the first row is `{first}`, and the C++ parser that this build links reads a seed of \
                 version {version}, whose first row is `# seed format {version}`. A scanner reads a seed of \
                 another version as NO NAME, with no message, so the run stops here rather than measure a \
                 tree that looks seeded and is not. Write the file again with `cargo xtask seed collect` of \
                 the tree-sitter-cpp commit that Cargo.toml pins.",
                path.display()
            ));
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut block: Vec<u8> = Vec::new();
        let mut previous = "";
        for (number, line) in text.lines().enumerate() {
            let row = number + 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (name, cell) = line
                .split_once('\t')
                .ok_or_else(|| format!("{}:{row}: the row is `{line}`, and a row of a seed is `name<TAB>kinds`", path.display()))?;
            if name.is_empty() {
                return Err(format!("{}:{row}: the row has no name", path.display()));
            }
            if name.len() > MAX_NAME {
                return Err(format!(
                    "{}:{row}: the name `{name}` has {} bytes, and the scanner compares a maximum of \
                     {MAX_NAME}, which is TS_CPP_SEED_WORD_SIZE of src/seed.h less the end of the string. \
                     The collector drops such a name, so a file that holds one comes from a tool that does \
                     not agree with this reader.",
                    path.display(),
                    name.len()
                ));
            }
            let kinds = kinds_of(cell).ok_or_else(|| {
                format!(
                    "{}:{row}: the kinds of `{name}` are `{cell}`. The kinds are a comma list of the words \
                     {}, each word one time and in that order, and `type` and `template` do not come \
                     together.",
                    path.display(),
                    KIND_WORDS.map(|(word, _)| word).join(", ")
                )
            })?;
            if !previous.is_empty() && name.as_bytes() <= previous.as_bytes() {
                return Err(format!(
                    "{}:{row}: `{name}` comes after `{previous}`, and the rows of a seed ascend by the \
                     bytes of the name, with one row for each name. A reader binary searches the file.",
                    path.display()
                ));
            }
            previous = name;
            entries.push(Entry {
                offset: u32::try_from(block.len())
                    .map_err(|_| format!("the C++ seed {} is too large", path.display()))?,
                length: u16::try_from(name.len()).expect("a name has a maximum of MAX_NAME bytes"),
                kinds,
            });
            block.extend_from_slice(name.as_bytes());
        }
        let id = sha256(&bytes);
        let names = entries.len();
        // The head takes the addresses of the two buffers. A move of this
        // struct moves no heap buffer, so the addresses stay correct.
        let head = Box::new(Head {
            magic: MAGIC,
            version,
            count: u32::try_from(names).map_err(|_| format!("the C++ seed {} holds too many names", path.display()))?,
            text_size: u32::try_from(block.len())
                .map_err(|_| format!("the C++ seed {} is too large", path.display()))?,
            entries: entries.as_ptr(),
            text: block.as_ptr(),
            id,
        });
        Ok(Self {
            _entries: entries,
            _text: block,
            head,
            id,
            names,
        })
    }

    /// The SHA-256 of the file, comments included, as 64 lowercase
    /// hexadecimal digits.
    ///
    /// THE TREE OF A C++ FILE IS A FUNCTION OF THE FILE AND THE SEED, so
    /// every metric of a seeded run is a function of this id too. An
    /// output that names no seed cannot be read again.
    pub fn id(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.id {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// The number of names.
    pub fn names(&self) -> usize {
        self.names
    }

    /// The pointer that `Parser::set_scanner_context` takes. It stays
    /// valid for as long as this seed lives.
    fn as_context(&self) -> *const c_void {
        std::ptr::from_ref::<Head>(&*self.head).cast::<c_void>()
    }
}

impl std::fmt::Debug for Seed {
    /// The number of names and the id. The two buffers are large, and no
    /// message holds them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Seed({} names, {})", self.names, self.id())
    }
}

/// The bits of a kinds cell, or None for a cell that [`KIND_WORDS`] does
/// not give. O(n) in the words of the cell.
fn kinds_of(cell: &str) -> Option<u16> {
    let mut bits = 0;
    let mut next = 0;
    for word in cell.split(',') {
        let index = KIND_WORDS.iter().position(|(known, _)| *known == word)?;
        if index < next {
            return None;
        }
        // `template` comes after `type` in the list, and a cell holds one
        // of the two.
        next = if index == 0 { 2 } else { index + 1 };
        bits |= KIND_WORDS[index].1;
    }
    (bits != 0).then_some(bits)
}

/// The seed of this run, or None for a run with no seed.
///
/// THE SEED IS A PROPERTY OF THE RUN AND NOT OF A FILE, because one
/// project has one seed. It is read one time before any file is read,
/// and it lives for the rest of the process, which is what the pointer
/// the parser holds requires.
///
/// A global carries it for the reason `clangfmt::for_file` gives: the
/// walk builds a parser in twenty places, and a value that is None for
/// every language but one says less threaded through all of them than it
/// does here.
static RUN: OnceLock<Option<Seed>> = OnceLock::new();

/// Record the seed of this run, one time, before any file is read.
///
/// A second call changes nothing, because a parser built after the first
/// call already holds a pointer into the first seed.
pub fn install(seed: Option<Seed>) {
    let _ = RUN.set(seed);
}

/// The seed of this run, for a report that names it.
pub fn of_run() -> Option<&'static Seed> {
    RUN.get().and_then(Option::as_ref)
}

/// Give the seed of this run to a parser of the C++ fork.
///
/// A parser of any other grammar must not take it. The runtime gives the
/// context only to a language of the ABI of the fork, so an upstream
/// grammar never reads it, and this test makes the intent plain as well.
pub fn install_in(parser: &mut tree_sitter::Parser, lang: super::Lang) {
    if lang != super::Lang::Cpp {
        return;
    }
    let Some(seed) = of_run() else {
        return;
    };
    // SAFETY: the pointer refers to the head of a seed in `RUN`, which is
    // a `OnceLock` that nothing writes again, so the target outlives every
    // parser of this process. The scanner reads the target and writes
    // none of it.
    unsafe { parser.set_scanner_context(seed.as_context()) };
}

/// The round constants of SHA-256, the first 32 bits of the fractional
/// parts of the cube roots of the first 64 prime numbers. Refer to
/// FIPS 180-4, section 4.2.2.
const ROUND: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

/// The SHA-256 of bytes, as 32 bytes. O(n) in the bytes. Refer to
/// FIPS 180-4.
///
/// The function is written here, as the fork writes it in
/// `xtask/src/seed.rs`, so that the identity of a seed needs no
/// dependency. The id must equal the id the fork prints for the same
/// file, or a number of this tool and a number of the gate name two
/// seeds that are one.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19,
    ];
    let mut message = bytes.to_vec();
    let length = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&length.to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut schedule = [0u32; 64];
        for (word, part) in schedule.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(part.try_into().expect("a part of four bytes"));
        }
        for index in 16..64 {
            let (before15, before2) = (schedule[index - 15], schedule[index - 2]);
            let s0 = before15.rotate_right(7) ^ before15.rotate_right(18) ^ (before15 >> 3);
            let s1 = before2.rotate_right(17) ^ before2.rotate_right(19) ^ (before2 >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (round, word) in ROUND.iter().zip(&schedule) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let first = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(*round)
                .wrapping_add(*word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (value, added) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *value = value.wrapping_add(added);
        }
    }
    let mut out = [0u8; 32];
    for (part, value) in out.chunks_exact_mut(4).zip(state) {
        part.copy_from_slice(&value.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Entry, Head, Seed, kinds_of, sha256};
    use std::io::Write as _;
    use std::path::PathBuf;

    /// A seed file in a directory of its own, removed at the end of the
    /// test. Two tests run at the same time, so a shared directory lets
    /// the end of one test remove the file of another.
    struct File(PathBuf);

    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    impl File {
        /// A seed file whose first row is the format row of the linked
        /// scanner, and then `text`.
        fn new(text: &str) -> Self {
            Self::raw(&format!("# seed format {}\n{text}", tree_sitter_cpp::seed_version()))
        }

        /// A seed file of exactly these bytes.
        fn raw(text: &str) -> Self {
            let number = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("elegance-cpp-seed-{}-{number}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("the directory of the test");
            let path = dir.join("cpp.seed");
            let mut file = std::fs::File::create(&path).expect("the file of the test");
            file.write_all(text.as_bytes()).expect("the write of the test");
            Self(path)
        }
    }

    impl Drop for File {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().expect("the file has a directory"));
        }
    }

    /// The two structs have the layout that `src/seed.h` of the fork
    /// declares. A build whose layout differs gives the scanner a head
    /// whose fields are at other offsets, and the scanner then reads a
    /// count or a pointer out of the wrong bytes with no message.
    #[test]
    fn the_structs_have_the_layout_of_the_header_of_the_fork() {
        assert_eq!(size_of::<Entry>(), 8);
        assert_eq!(align_of::<Entry>(), 4);
        assert_eq!(std::mem::offset_of!(Entry, offset), 0);
        assert_eq!(std::mem::offset_of!(Entry, length), 4);
        assert_eq!(std::mem::offset_of!(Entry, kinds), 6);
        assert_eq!(size_of::<Head>(), 64);
        assert_eq!(std::mem::offset_of!(Head, magic), 0);
        assert_eq!(std::mem::offset_of!(Head, version), 4);
        assert_eq!(std::mem::offset_of!(Head, count), 8);
        assert_eq!(std::mem::offset_of!(Head, text_size), 12);
        assert_eq!(std::mem::offset_of!(Head, entries), 16);
        assert_eq!(std::mem::offset_of!(Head, text), 24);
        assert_eq!(std::mem::offset_of!(Head, id), 32);
    }

    /// The published vectors of FIPS 180-4. A wrong digest gives a seed
    /// an id that no other tool can reproduce.
    #[test]
    fn the_digest_gives_the_published_vectors() {
        let hex = |bytes: &[u8]| -> String {
            sha256(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        };
        assert_eq!(hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// A well-formed file gives its names, and the id is the digest of
    /// the whole file, comments included.
    #[test]
    fn a_well_formed_file_gives_its_names() {
        let file = File::new("Alpha\ttype\nBeta\ttemplate\nGAMMA\tobject-macro,body-name\n");
        let seed = Seed::read(&file.0).expect("the file reads");
        assert_eq!(seed.names(), 3);
        let bytes = std::fs::read(&file.0).expect("the file of the test");
        let expected: String = sha256(&bytes).iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(seed.id(), expected);
    }

    /// A file of another version stops the run and names the two
    /// versions. The scanner would read it as no name, with no message.
    #[test]
    fn a_file_of_another_version_is_refused_by_name() {
        let version = tree_sitter_cpp::seed_version();
        let file = File::raw(&format!("# seed format {}\nAlpha\ttype\n", version + 1));
        let message = Seed::read(&file.0).expect_err("the version differs");
        assert!(message.contains(&format!("# seed format {version}")), "{message}");
        assert!(message.contains(&format!("version {}", version + 1)) || message.contains("the first row is"), "{message}");
        assert!(message.contains("xtask seed collect"), "{message}");
    }

    /// A file with no format row is refused. The collector of version 1
    /// wrote no such row.
    #[test]
    fn a_file_with_no_format_row_is_refused() {
        let file = File::raw("Alpha\ttype\n");
        let message = Seed::read(&file.0).expect_err("the first row is not the format row");
        assert!(message.contains("the first row is `Alpha"), "{message}");
    }

    /// The reader refuses a file that a binary search cannot read, and
    /// never a row of it.
    #[test]
    fn a_file_that_a_binary_search_cannot_read_is_refused() {
        for (text, part) in [
            ("Alpha type\n", "name<TAB>kinds"),
            ("Beta\ttype\nAlpha\ttype\n", "ascend by the"),
            ("Alpha\tAlpha\ttype\n", "the kinds of"),
            ("Alpha\tobject-macro,type\n", "the kinds of"),
            ("Alpha\t\n", "the kinds of"),
        ] {
            let file = File::new(text);
            let Err(message) = Seed::read(&file.0) else {
                panic!("the file `{text}` is refused")
            };
            assert!(message.contains(part), "the message of `{text}` is `{message}`");
        }
        let long = "A".repeat(super::MAX_NAME + 1);
        let file = File::new(&format!("{long}\ttype\n"));
        let message = Seed::read(&file.0).expect_err("the name is too long");
        assert!(message.contains("TS_CPP_SEED_WORD_SIZE"), "{message}");
    }

    /// The words of a kinds cell come in the order of the file, each one
    /// time, and `template` holds the bit of `type`.
    #[test]
    fn the_kinds_cell_reads_the_words_of_the_fork() {
        assert_eq!(kinds_of("type"), Some(1));
        assert_eq!(kinds_of("template"), Some(3));
        assert_eq!(kinds_of("type,object-macro"), Some(1 | 4));
        assert_eq!(kinds_of("object-macro,body-empty,body-name"), Some(4 | 16 | 32));
        assert_eq!(kinds_of("function-macro,object-macro"), None);
        assert_eq!(kinds_of("type,type"), None);
        assert_eq!(kinds_of("macro"), None);
        assert_eq!(kinds_of(""), None);
    }

    /// THE SEED REACHES THE SCANNER, AND IT GIVES THE RIGHT TREE.
    ///
    /// `Alpha(x)` after `return` is a call when nothing says that
    /// `Alpha` is a type, and a functional cast when something does. The
    /// file itself can say it, with `class Alpha {}`, and a seed says it
    /// for a project whose header this file does not hold. A forward
    /// declaration alone does not say it: the record of the scanner
    /// holds a class that has a body.
    ///
    /// THE ASSERTION IS THE STRONG ONE. "The tree changed" passes for
    /// any change at all. This says the seeded tree EQUALS the tree of
    /// the same text in a file that declares the name, which is what a
    /// seed is for. A statement position would prove nothing: a
    /// declaration can start there, so `Alpha(x);` stays a call even
    /// when the file declares the class.
    #[test]
    fn the_seeded_tree_is_the_tree_of_a_file_that_declares_the_name() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("the C++ grammar loads");
        let sexp = |parser: &mut tree_sitter::Parser, source: &str, child: usize| {
            let tree = parser.parse(source, None).expect("the parse ends");
            let root = tree.root_node();
            assert!(!root.has_error(), "{source}");
            root.named_child(child as u32).expect("the function").to_sexp()
        };
        let text = "int f() { return Alpha(x); }";
        let plain = sexp(&mut parser, text, 0);
        let declared = sexp(&mut parser, &format!("class Alpha {{}};\n{text}"), 1);
        assert_ne!(plain, declared, "the declaration decides this position");

        let file = File::new("Alpha\ttype\n");
        let seed = Seed::read(&file.0).expect("the file reads");
        // SAFETY: the context is the head of a seed that lives to the end
        // of this test, and the parse below happens inside that life.
        unsafe { parser.set_scanner_context(seed.as_context()) };
        assert_eq!(sexp(&mut parser, text, 0), declared);

        // And the seed decides nothing that it does not name.
        assert_eq!(sexp(&mut parser, "int f() { return Beta(x); }", 0), plain.replace("Alpha", "Beta"));
    }

    /// THE SCAN PATH: `parse_with_options` with a progress callback, the
    /// call that `parse_bounded` makes, must read the seed too.
    #[test]
    fn a_parse_with_options_reads_the_seed() {
        use std::ops::ControlFlow;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("the C++ grammar loads");
        let file = File::new("Alpha\ttype\n");
        let seed = Seed::read(&file.0).expect("the file reads");
        unsafe { parser.set_scanner_context(seed.as_context()) };
        let text = "int f() { return Alpha(x); }";
        let bytes = text.as_bytes();
        let mut stop = |_: &tree_sitter::ParseState| ControlFlow::Continue(());
        let tree = parser
            .parse_with_options(
                &mut |at, _| bytes.get(at..).unwrap_or_default(),
                None,
                Some(tree_sitter::ParseOptions::new().progress_callback(&mut stop)),
            )
            .expect("the parse ends");
        assert!(tree.root_node().to_sexp().contains("type_identifier"), "{}", tree.root_node().to_sexp());
    }

    /// A seed that moves keeps the addresses in its head, because the
    /// two buffers live on the heap and the head is a box.
    #[test]
    fn a_seed_that_moves_keeps_the_addresses_of_its_head() {
        let file = File::new("Alpha\ttype\nBeta\ttype\n");
        let seed = Seed::read(&file.0).expect("the file reads");
        let context = seed.as_context();
        let moved = Box::new(seed);
        assert_eq!(moved.as_context(), context);
        // SAFETY: the context is the head of a seed that is alive here.
        let head = unsafe { &*context.cast::<Head>() };
        assert_eq!(head.magic, super::MAGIC);
        assert_eq!(head.version, tree_sitter_cpp::seed_version());
        assert_eq!(head.count, 2);
        assert_eq!(head.text_size, 9);
    }
}
