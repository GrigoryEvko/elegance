//! C++ syntax the bundled grammar cannot read, rewritten into syntax it
//! can, with every byte offset left where it was.
//!
//! A parse error is not local. The node that fails swallows the rest of
//! its scope, so one `= delete("reason")` near the top of a header
//! re-parents every function below it. The loss is silent. Units go
//! missing from the listing, and the units that survive carry the
//! complexity of whatever the error folded into them. That reads as a
//! real measurement, which is worse than no measurement.
//!
//! So the source is normalized before the parser reads it. Every
//! rewrite here replaces a span with the SAME NUMBER OF BYTES, because
//! every offset the report prints is an offset into this text. Spaces
//! and underscores keep all of them true. A splice becomes underscores
//! and not spaces because it stands where a name stands: `obj.[:m:]`
//! has to stay a member access, and `obj.` and then blanks is not one.
//!
//! None of this is an opinion about the code. It is the smallest edit
//! that lets the grammar read the shape the compiler reads.
//!
//! One rule is one function, and `RULES` lists them with the paper that
//! adds the syntax. A rule states the byte or the word that can start
//! it, so a file pays for the rules its text can reach and not for all
//! of them.
//!
//! The grammar moves, and the language moves faster. A construct that
//! no rule here covers stays a parse error, and `--errors` names the
//! file and the line. That is the signal to add a rule. The failure
//! mode is loud on purpose.

use std::sync::OnceLock;

use crate::clangfmt::{Kind, Macros};

// ---------------------------------------------------------------------
// The text under rewrite
// ---------------------------------------------------------------------

/// The bytes, and which of them are code.
struct Text {
    out: Vec<u8>,
    code: Vec<bool>,
}

impl Text {
    fn new(src: &[u8]) -> Text {
        Text {
            code: code_mask(src),
            out: src.to_vec(),
        }
    }

    fn len(&self) -> usize {
        self.out.len()
    }

    fn at(&self, i: usize) -> u8 {
        self.out[i]
    }

    fn get(&self, i: usize) -> Option<u8> {
        self.out.get(i).copied()
    }

    fn is_code(&self, i: usize) -> bool {
        self.code.get(i).copied().unwrap_or(false)
    }

    fn starts(&self, i: usize, want: &[u8]) -> bool {
        self.out[i..].starts_with(want)
    }

    /// True when `at` starts the word `want` and no word byte runs into
    /// it.
    fn word_at(&self, at: usize, want: &[u8]) -> bool {
        word_at(&self.out, at, want)
    }

    /// True when a word starts at `at`.
    fn starts_word(&self, at: usize) -> bool {
        is_word(self.out[at]) && (at == 0 || !is_word(self.out[at - 1]))
    }

    /// The end of the word that starts at `at`.
    fn word_end(&self, at: usize) -> usize {
        word_end(&self.out, at)
    }

    /// The word that starts at `at`.
    fn word(&self, at: usize) -> &[u8] {
        &self.out[at..self.word_end(at)]
    }

    /// The word that ends at `end`, when one does.
    fn word_before(&self, end: usize) -> &[u8] {
        word_before(&self.out, end)
    }

    /// The last code byte before `at` that is not whitespace.
    fn prev(&self, at: usize) -> Option<usize> {
        (0..at)
            .rev()
            .find(|&i| self.is_code(i) && !self.out[i].is_ascii_whitespace())
    }

    /// The first code byte at or after `at` that is not whitespace.
    fn next(&self, at: usize) -> Option<usize> {
        (at..self.len()).find(|&i| self.is_code(i) && !self.out[i].is_ascii_whitespace())
    }

    /// The index of the bracket that closes the `open` bracket at `at`,
    /// counted over code bytes only.
    fn match_close(&self, at: usize, open: u8, shut: u8) -> Option<usize> {
        if !self.is_code(at) || self.out[at] != open {
            return None;
        }
        let mut depth = 0u32;
        for (i, &byte) in self.out.iter().enumerate().skip(at) {
            if !self.is_code(i) {
                continue;
            }
            if byte == open {
                depth += 1;
            } else if byte == shut {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        None
    }

    fn match_paren(&self, at: usize) -> Option<usize> {
        self.match_close(at, b'(', b')')
    }

    /// Overwrite a span with `fill`, and leave the line breaks where
    /// they are. A `delete("...")` reason or a contract predicate can
    /// run across several lines. A rewrite that removes those breaks
    /// moves every finding below it onto the wrong line.
    fn blank(&mut self, range: std::ops::Range<usize>, fill: u8) {
        for byte in &mut self.out[range] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = fill;
            }
        }
    }

    /// Write `text` at the start of a span and blank the rest of it.
    ///
    /// False when the span cannot hold the text, or when a line break
    /// runs through where the text would go. Nothing is written then,
    /// because a rewrite that grows the source or eats a line break
    /// moves every finding below it. Each caller has an answer for
    /// false that is safe.
    fn overwrite(&mut self, range: std::ops::Range<usize>, text: &[u8]) -> bool {
        let start = range.start;
        if range.len() < text.len() || self.out[start..start + text.len()].contains(&b'\n') {
            return false;
        }
        self.blank(range, b' ');
        self.out[start..start + text.len()].copy_from_slice(text);
        true
    }
}

// ---------------------------------------------------------------------
// The lexical layer
// ---------------------------------------------------------------------

/// Which bytes are code, and not comment or literal text. Every rewrite
/// reads this first. A `^^` inside a string is a string, and a `pre(`
/// in a doc comment is prose.
///
/// Three lexical rules keep the mask true, and each one of them is a
/// defect if it is missing:
///
/// - A line comment continues past the line break when the line ends
///   with a backslash. Translation phase 2 joins the two lines before
///   the compiler reads a single token.
/// - A quote does not open a literal when no quote closes it on the
///   same line. Neither a string nor a character literal holds a raw
///   line break, so an apostrophe in prose stays prose.
/// - An apostrophe between two digits is a digit separator. `1'000'000`
///   is one number, and the C++14 separator in it opens nothing.
///
/// Without the last two rules, one apostrophe in `#error can't` or in
/// `1'000` marks the rest of the file as text. Every rewrite below it
/// then stops, and the file goes to the parser unchanged.
fn code_mask(src: &[u8]) -> Vec<bool> {
    let mut code = vec![true; src.len()];
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            b'/' if src.get(i + 1) == Some(&b'/') => {
                let end = line_comment_end(src, i);
                code[i..end].fill(false);
                i = end;
            }
            b'/' if src.get(i + 1) == Some(&b'*') => {
                let end = find(src, i + 2, b"*/").map_or(src.len(), |e| e + 2);
                code[i..end].fill(false);
                i = end;
            }
            // A raw string has no escapes, so its own delimiter is the
            // only way out: R"tag( ... )tag". The `L`, `u8`, `u` and `U`
            // prefixes sit before the `R`, and the scan meets the `R`
            // itself, so each prefix needs no rule of its own.
            b'R' if src.get(i + 1) == Some(&b'"') => {
                let open = i + 2;
                let Some(paren) = src[open..]
                    .iter()
                    .position(|&c| c == b'(')
                    .map(|p| open + p)
                else {
                    i += 1;
                    continue;
                };
                let mut close = Vec::with_capacity(paren - open + 2);
                close.push(b')');
                close.extend_from_slice(&src[open..paren]);
                close.push(b'"');
                let end = find(src, paren + 1, &close).map_or(src.len(), |e| e + close.len());
                code[i..end].fill(false);
                i = end;
            }
            // `#error` and `#warning` carry prose, and prose carries
            // apostrophes. The text after them is not a token sequence.
            b'#' if directive_is_prose(src, i) => {
                let end = line_comment_end(src, i);
                code[i..end].fill(false);
                i = end;
            }
            b'\'' if digit_separator(src, i) => i += 1,
            b'"' | b'\'' => match literal_end(src, i) {
                Some(end) => {
                    code[i..end].fill(false);
                    i = end;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    code
}

/// The end of the line comment that opens at `at`, past every line that
/// a backslash joins to it.
fn line_comment_end(src: &[u8], at: usize) -> usize {
    let mut i = at;
    loop {
        while i < src.len() && src[i] != b'\n' {
            i += 1;
        }
        if i >= src.len() || !spliced(src, i) {
            return i;
        }
        i += 1;
    }
}

/// True when the line break at `nl` joins its line to the next one. A
/// line that ends with a backslash is one logical line with the line
/// that follows it.
fn spliced(src: &[u8], nl: usize) -> bool {
    let mut k = nl;
    if k > 0 && src[k - 1] == b'\r' {
        k -= 1;
    }
    k > 0 && src[k - 1] == b'\\'
}

/// True when the `#` at `at` starts a directive whose argument is prose.
fn directive_is_prose(src: &[u8], at: usize) -> bool {
    let Some(word) = (at + 1..src.len()).find(|&k| !matches!(src[k], b' ' | b'\t')) else {
        return false;
    };
    word_at(src, word, b"error") || word_at(src, word, b"warning")
}

/// True when the apostrophe at `at` separates the digits of a number.
/// C++14 writes `1'000'000` and `0xDEAD'BEEF`, and neither apostrophe
/// opens a character literal. The run that ends at the apostrophe tells
/// the two apart: a number starts with a digit, and the `L`, `u8`, `u`
/// and `U` prefixes of a character literal do not.
fn digit_separator(src: &[u8], at: usize) -> bool {
    if at == 0 || !src[at - 1].is_ascii_alphanumeric() {
        return false;
    }
    let mut start = at;
    while start > 0 && (is_word(src[start - 1]) || src[start - 1] == b'\'') {
        start -= 1;
    }
    src[start].is_ascii_digit()
}

/// The byte after the literal that opens at `at`, or `None` when the
/// quote does not close on the same logical line. A string and a
/// character literal both end on the line that starts them, unless a
/// backslash joins that line to the next.
fn literal_end(src: &[u8], at: usize) -> Option<usize> {
    let quote = src[at];
    let mut i = at + 1;
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'\n' if !spliced(src, i) => return None,
            byte if byte == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn find(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() || needle.len() > hay.len() - from {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The word that ends at `end`, when one does.
fn word_before(src: &[u8], end: usize) -> &[u8] {
    let mut start = end;
    while start > 0 && is_word(src[start - 1]) {
        start -= 1;
    }
    &src[start..end]
}

/// The end of the word that starts at `at`.
fn word_end(src: &[u8], at: usize) -> usize {
    let mut end = at;
    while end < src.len() && is_word(src[end]) {
        end += 1;
    }
    end
}

/// True when `at` starts the word `want` and nothing runs into it.
fn word_at(src: &[u8], at: usize, want: &[u8]) -> bool {
    src[at..].starts_with(want)
        && !src.get(at + want.len()).is_some_and(|&c| is_word(c))
        && (at == 0 || !is_word(src[at - 1]))
}

// ---------------------------------------------------------------------
// What a position means
// ---------------------------------------------------------------------

/// The type keywords that cannot stand alone as an expression. `^^int`
/// reflects on one of these, and the operand has to become a name
/// before the grammar reads the line.
const BUILTIN_TYPES: [&[u8]; 16] = [
    b"void",
    b"bool",
    b"char",
    b"char8_t",
    b"char16_t",
    b"char32_t",
    b"wchar_t",
    b"short",
    b"int",
    b"long",
    b"signed",
    b"unsigned",
    b"float",
    b"double",
    b"auto",
    b"nullptr_t",
];

/// The specifiers a lambda can carry. The grammar reads each one after
/// a parameter list and none of them without one.
const LAMBDA_SPECIFIERS: [&[u8]; 5] = [
    b"consteval",
    b"constexpr",
    b"static",
    b"mutable",
    b"noexcept",
];

/// The keywords that take a parenthesized condition. A `)` that closes
/// one of these is the end of a condition, and never the end of the
/// parameter list of a function.
const CONDITIONS: [&[u8]; 6] = [
    b"if",
    b"while",
    b"for",
    b"switch",
    b"catch",
    b"synchronized",
];

/// The words that can stand between the parameter list of a function
/// and a contract clause.
const QUALIFIERS: [&[u8]; 6] = [
    b"const",
    b"volatile",
    b"noexcept",
    b"override",
    b"final",
    b"requires",
];

/// The keywords that an expression can follow. A declarator holds none
/// of them, so one of them before a `(` makes that `(` the start of an
/// argument list.
const EXPRESSIONS: [&[u8]; 12] = [
    b"return",
    b"co_return",
    b"co_yield",
    b"co_await",
    b"throw",
    b"case",
    b"new",
    b"delete",
    b"sizeof",
    b"alignof",
    b"typeid",
    b"goto",
];

/// True when `at` sits in a `[...]` that opens in the same statement.
/// A structured binding pack is the one ellipsis that stands in
/// brackets and before a name.
fn inside_brackets(t: &Text, at: usize) -> bool {
    let mut depth = 0u32;
    for i in (0..at).rev() {
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b']' => depth += 1,
            b'[' if depth == 0 => return true,
            b'[' => depth -= 1,
            b';' | b'{' | b'}' => return false,
            _ => {}
        }
    }
    false
}

/// True when the `)` at `at` closes the condition of an `if`, a `while`
/// or another statement that takes one. A contract clause follows the
/// parameter list of a function, and `if (ready) pre(x);` is a call.
fn closes_a_condition(t: &Text, at: usize) -> bool {
    let mut depth = 0u32;
    for i in (0..=at).rev() {
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return t
                        .prev(i)
                        .is_some_and(|p| CONDITIONS.contains(&t.word_before(p + 1)));
                }
            }
            _ => {}
        }
    }
    false
}

/// The `(` that opens the parameter list that `close` ends.
fn open_paren_of(t: &Text, close: usize) -> Option<usize> {
    let mut depth = 0u32;
    for i in (0..=close).rev() {
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `<` that opens the template header closed by the `>` at `close`.
fn angle_open(t: &Text, close: usize) -> Option<usize> {
    let mut depth = 0u32;
    for i in (0..=close).rev() {
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b'>' => depth += 1,
            b'<' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            // A header holds no statement, so anything that ends one
            // says the `>` was a comparison.
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
    }
    None
}

/// True when the parameter list that ends at `close` belongs to a
/// declaration, and not to a call in an expression.
///
/// The scan steps over the parameter list and reads back to the start
/// of the statement. Four things end it. A `]` directly before the list
/// is the introducer of a lambda, and a lambda takes a contract clause
/// of its own. An unmatched `(` or `[` means an argument list or a
/// subscript holds the call, as in `f(g() & pre(h))`. An `=` or one of
/// the expression keywords before it means an expression, as in
/// `return g() & pre(2)`. A statement boundary means a declaration.
fn declares_a_function(t: &Text, close: usize) -> bool {
    let Some(open) = open_paren_of(t, close) else {
        return false;
    };
    if t.prev(open).is_some_and(|p| t.at(p) == b']') {
        return true;
    }
    let mut depth = 0u32;
    let mut i = open;
    while i > 0 {
        i -= 1;
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b')' | b']' => depth += 1,
            b'(' | b'[' if depth == 0 => return false,
            b'(' | b'[' => depth -= 1,
            // A template header ends in `>`, and a default template
            // argument spells an `=` inside it. Reading that `=` as an
            // assignment rejects every contract clause on a template.
            // The scan steps over the whole header instead.
            b'>' if depth == 0 => match angle_open(t, i) {
                Some(open) => i = open,
                None => return true,
            },
            b'=' | b'?' if depth == 0 => return false,
            b';' | b'{' | b'}' if depth == 0 => return true,
            byte if depth == 0 && is_word(byte) => {
                let word = t.word_before(i + 1);
                if EXPRESSIONS.contains(&word) {
                    return false;
                }
                i -= word.len() - 1;
            }
            _ => {}
        }
    }
    true
}

/// The position after a contract clause that starts at `at`, or `None`
/// when `at` starts a call instead.
///
/// P2900 gives `pre` and `post` no keyword of their own, so the two
/// read as ordinary names. Only the position tells a clause from a
/// call, and the tool answers the question from three sides. A wrong
/// answer in one direction removes real work and under-reports
/// complexity, and that failure is silent. A wrong answer in the other
/// direction leaves a parse error, which `--errors` reports.
///
/// A clause needs all three of these:
///
/// 1. The bytes before it belong to a declarator. A clause follows the
///    parameter list, a cv-qualifier or ref-qualifier, `noexcept`, a
///    virt-specifier, or a trailing return type.
/// 2. The `)` it reads back to closes a parameter list. It does not
///    close the condition of an `if`, and the declarator it ends is not
///    a call in an expression.
/// 3. A body or another clause follows it. A call is followed by an
///    operator or an argument.
fn contract_clause(t: &Text, at: usize) -> Option<usize> {
    let open = t.next(t.word_end(at)).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;

    // 3. What follows a clause is a body, a declaration end, another
    //    clause, a trailing return type, or the initializer list of a
    //    constructor. A call is followed by an operator or an argument.
    let follows = t.next(close + 1)?;
    let tail_ok = matches!(t.at(follows), b'{' | b';' | b'-' | b'=' | b':')
        || t.word_at(follows, b"pre")
        || t.word_at(follows, b"post")
        || QUALIFIERS.contains(&t.word(follows));
    if !tail_ok {
        return None;
    }

    // 1. Read back over the tokens a declarator can end with. A word is
    //    a qualifier, or part of a trailing return type, and a trailing
    //    return type has to hold a type name before its `->`.
    let mut saw_word = false;
    let mut i = t.prev(at)?;
    loop {
        let byte = t.at(i);
        if byte == b')' {
            // 2. The parameter list of a declaration, not a condition
            //    and not a call.
            return (!closes_a_condition(t, i) && declares_a_function(t, i)).then_some(close + 1);
        }
        if is_word(byte) {
            // A qualifier, or the type in a trailing return type. The
            // arrow below is what tells the second one from a member
            // access, and it needs a type name to have come first.
            let start = i + 1 - t.word_before(i + 1).len();
            saw_word = true;
            i = t.prev(start)?;
            continue;
        }
        match byte {
            // A ref-qualifier, or a pointer or reference in a trailing
            // return type.
            b'&' => {}
            // `->` opens a trailing return type. Without a type name
            // between the arrow and the clause, the arrow is a member
            // access and `pre` is the member.
            b'>' if t.prev(i).is_some_and(|p| t.at(p) == b'-') => {
                if !saw_word {
                    return None;
                }
                i = t.prev(i)?;
            }
            // The rest of a trailing return type.
            b'*' | b':' | b',' | b'<' | b'>' | b'~' if saw_word => {}
            _ => return None,
        }
        i = t.prev(i)?;
    }
}

/// The end of the operand of a `^^` that cannot stand as an expression.
/// `^^int` reflects on a type keyword, and `^^::` on the global
/// namespace. Neither is a name, so both become one.
fn reflect_operand_end(t: &Text, at: usize) -> Option<usize> {
    let start = t.next(at)?;
    if t.starts(start, b"::") {
        return Some(start + 2);
    }
    let mut end = None;
    let mut i = start;
    while let Some(word) = t.next(i) {
        if word > i && end.is_none() {
            break;
        }
        let stop = t.word_end(word);
        if stop == word || !BUILTIN_TYPES.contains(&t.word(word)) {
            break;
        }
        end = Some(stop);
        i = stop;
    }
    end
}

/// True when a qualified name follows `at`. `typename` before one is a
/// disambiguator the grammar reads without, and `typename` in a
/// template parameter list is followed by a plain name instead.
fn qualified_after(t: &Text, at: usize) -> bool {
    let Some(start) = t.next(at) else {
        return false;
    };
    if t.starts(start, b"::") {
        return true;
    }
    let name_end = t.word_end(start);
    name_end > start && t.next(name_end).is_some_and(|k| t.starts(k, b"::"))
}

/// True when the `=` at `at` follows the designator of a braced
/// initializer, as `.pad = {}` does.
///
/// The `,` after such a value opens the next designator, and not the
/// next parameter, so the value is not a default argument. Blanking it
/// leaves `.pad ,` and the list around it stops parsing.
fn designator_before(t: &Text, at: usize) -> bool {
    let Some(prev) = t.prev(at) else {
        return false;
    };
    let word = t.word_before(prev + 1);
    let start = prev + 1 - word.len();
    !word.is_empty() && start > 0 && t.at(start - 1) == b'.'
}

/// True when `at` sits on a preprocessor directive line.
///
/// The name a directive spells is not a declarator. Blanking
/// `CRUCIBLE_INLINE` in the `#define` that writes it leaves `#define`
/// with nothing to define, and every declaration under it stops
/// parsing. `#undef` and `#ifdef` name it the same way.
fn on_a_directive(t: &Text, at: usize) -> bool {
    (0..at)
        .rev()
        .take_while(|&k| t.at(k) != b'\n')
        .filter(|&k| !t.at(k).is_ascii_whitespace())
        .last()
        .is_some_and(|k| t.at(k) == b'#')
}

/// True when a string literal ends earlier on the same line as `at`,
/// with whitespace between the two.
///
/// A name in that position can only be a macro that expands to a
/// string: `"%016" PRIx64` is one literal to the compiler, and C++ puts
/// nothing else after a literal. Two details carry the rule.
///
/// The space is one. A user-defined literal writes its suffix against
/// the quote, so `""_km` names an operator and keeps its name.
///
/// The line is the other, and without it the rule is a defect. An
/// include line ends with a quote, and the declaration under it opens
/// with a name: `#include "config.h"` over `namespace crucible {`
/// blanks the namespace and every file that holds one stops parsing.
fn follows_a_string(t: &Text, at: usize) -> bool {
    if at == 0 || !t.at(at - 1).is_ascii_whitespace() {
        return false;
    }
    let Some(quote) = (0..at)
        .rev()
        .take_while(|&k| t.at(k) != b'\n')
        .find(|&k| !t.at(k).is_ascii_whitespace())
        .filter(|&k| t.at(k) == b'"' && !t.is_code(k))
    else {
        return false;
    };
    // Two declarations put a name after a string of their own.
    // `extern "C" int f()` states a linkage, and `operator "" _km`
    // names a literal operator. The word before the string tells them
    // from a macro, so the scan reads back over the literal to find it.
    let opens = (0..=quote)
        .rev()
        .take_while(|&k| !t.is_code(k))
        .last()
        .unwrap_or(quote);
    !t.prev(opens)
        .is_some_and(|p| matches!(t.word_before(p + 1), b"extern" | b"operator"))
}

/// The end of a module declaration that starts at `at`, or `None` when
/// `at` starts something else. `module` and `import` are context
/// keywords, so a declaration has to start a statement, and a call such
/// as `import(path)` keeps its parentheses and stays.
fn module_declaration(t: &Text, at: usize) -> Option<usize> {
    let starts_statement = t
        .prev(at)
        .is_none_or(|p| matches!(t.at(p), b';' | b'{' | b'}'));
    if !starts_statement {
        return None;
    }
    let mut i = at;
    if t.word_at(i, b"export") {
        let after = t.next(i + b"export".len())?;
        if !t.word_at(after, b"module") && !t.word_at(after, b"import") {
            return None;
        }
        i = after;
    }
    if !t.word_at(i, b"module") && !t.word_at(i, b"import") {
        return None;
    }
    // `import(x)` is a call, and `module = 1` is an assignment. Neither
    // word is reserved, so both stay where a declaration does not follow.
    let after = t.next(t.word_end(i));
    if after.is_none_or(|k| matches!(t.at(k), b'(' | b'=')) {
        return None;
    }
    let end = (i..t.len()).find(|&k| t.is_code(k) && t.at(k) == b';')?;
    Some(end + 1)
}

// ---------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------

/// What a rule needs before the driver calls it. A rule that states a
/// byte or a word is asked only where that byte or word stands.
enum Trigger {
    Byte(u8),
    Words(&'static [&'static [u8]]),
    /// Any word, because the rule keys on the position and not on the
    /// spelling.
    AnyWord,
}

/// One rewrite: what it is called, the paper that adds the syntax, and
/// where it can fire.
struct Rule {
    name: &'static str,
    paper: &'static str,
    trigger: Trigger,
    apply: fn(&mut Text, usize, &Cx) -> Option<usize>,
}

/// What a rule may consult besides the text.
struct Cx<'a> {
    macros: &'a Macros,
}

/// P2996 reflect. The operand is an ordinary expression once the
/// operator is gone, unless the operand is a type keyword or the global
/// namespace, and neither of those is an expression.
fn reflect(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'^') {
        return None;
    }
    match reflect_operand_end(t, i + 2) {
        Some(end) => {
            t.blank(i..end, b'_');
            Some(end)
        }
        None => {
            t.blank(i..i + 2, b' ');
            Some(i + 2)
        }
    }
}

/// P2996 splice. Splices nest, so the depth answers where one ends, and
/// the first `:]` does not.
fn splice(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b':') {
        return None;
    }
    let mut depth = 0u32;
    let mut j = i;
    let end = loop {
        if j + 1 >= t.len() {
            return None;
        }
        if t.is_code(j) && t.at(j) == b'[' && t.at(j + 1) == b':' {
            depth += 1;
            j += 2;
        } else if t.is_code(j) && t.at(j) == b':' && t.at(j + 1) == b']' {
            depth -= 1;
            if depth == 0 {
                break j + 2;
            }
            j += 2;
        } else {
            j += 1;
        }
    };
    t.blank(i..end, b'_');
    Some(end)
}

/// An attribute before `friend`. The grammar reads `friend` and it
/// reads an attribute, and not the two together. An attribute carries
/// no complexity, so the declaration keeps everything it had.
fn attribute_before_friend(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'[') {
        return None;
    }
    let close = t.match_close(i, b'[', b']')?;
    if close <= i + 2 || t.at(close - 1) != b']' {
        return None;
    }
    if !t.next(close + 1).is_some_and(|k| t.word_at(k, b"friend")) {
        return None;
    }
    t.blank(i..close + 1, b' ');
    Some(close + 1)
}

/// P3394 annotation. An attribute that starts with `=` carries an
/// expression, and an attribute is not a unit of complexity.
fn annotation(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'[') {
        return None;
    }
    if !t.next(i + 2).is_some_and(|k| t.at(k) == b'=') {
        return None;
    }
    let close = t.match_close(i, b'[', b']')?;
    if close <= i + 2 || t.at(close - 1) != b']' {
        return None;
    }
    t.blank(i..close + 1, b' ');
    Some(close + 1)
}

/// P2662 pack indexing. An index into a pack carries no complexity.
fn pack_index(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !t.starts(i, b"...") {
        return None;
    }
    let open = t.next(i + 3).filter(|&k| t.at(k) == b'[')?;
    let close = t.match_close(open, b'[', b']')?;
    t.blank(i..close + 1, b' ');
    Some(close + 1)
}

/// P1061 structured binding pack. An ellipsis before a name in brackets
/// binds the rest of the pack, and the names that remain read as an
/// ordinary binding.
fn binding_pack(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !t.starts(i, b"...") {
        return None;
    }
    let names = t
        .next(i + 3)
        .is_some_and(|k| is_word(t.at(k)) && !t.at(k).is_ascii_digit());
    if !names
        || !t.prev(i).is_some_and(|p| matches!(t.at(p), b'[' | b','))
        || !inside_brackets(t, i)
    {
        return None;
    }
    t.blank(i..i + 3, b' ');
    Some(i + 3)
}

/// P1306 expansion statement. Drop `template` and keep the `for`.
fn expansion_statement(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let after = i + b"template".len();
    if !t.next(after).is_some_and(|k| t.word_at(k, b"for")) {
        return None;
    }
    t.blank(i..after, b' ');
    Some(after)
}

/// P2573 `= delete("reason")`. The whole initializer goes, and not only
/// the reason. The grammar has no deleted function outside a class
/// body, so `= delete` alone fails where the reason string made the
/// line parse — and it fails wrongly, as a variable with a call for an
/// initializer. What remains is the declaration, which is what a
/// deleted function is.
fn delete_reason(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let eq = t.prev(i).filter(|&p| t.at(p) == b'=')?;
    let open = t.next(i + b"delete".len()).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    t.blank(eq..close + 1, b' ');
    Some(close + 1)
}

/// P2900 contract clause.
fn contract(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = contract_clause(t, i)?;
    t.blank(i..end, b' ');
    Some(end)
}

/// P0847 explicit object parameter. `this` names the parameter, and a
/// type follows it. A `this` that a call passes is followed by `)` or
/// `,` and stays.
fn explicit_object_parameter(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = i + b"this".len();
    if !t.prev(i).is_some_and(|p| t.at(p) == b'(') || !t.next(end).is_some_and(|k| is_word(t.at(k)))
    {
        return None;
    }
    t.blank(i..end, b' ');
    Some(end)
}

/// A lambda specifier with no parameter list. `[] consteval {}` is
/// C++23, and the grammar reads the word only after a `()` that this
/// lambda does not write. A specifier states how the body may be called
/// and adds nothing to measure.
///
/// The introducer is what identifies one. A `]]` before the word closes
/// an attribute instead, and `[[nodiscard]] constexpr` is an ordinary
/// declaration that already reads.
fn lambda_specifier(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !LAMBDA_SPECIFIERS.contains(&t.word(i)) {
        return None;
    }
    t.prev(i)
        .filter(|&p| t.at(p) == b']' && (p == 0 || t.at(p - 1) != b']'))?;
    let end = t.word_end(i);
    t.blank(i..end, b' ');
    Some(end)
}

/// P1938 `if consteval`. The branch is an ordinary branch, and the
/// condition it takes is a constant.
fn if_consteval(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = i + b"consteval".len();
    let bang = t.prev(i).filter(|&p| t.at(p) == b'!');
    let start = bang.unwrap_or(i);
    if !t.prev(start).is_some_and(|p| t.word_before(p + 1) == b"if") {
        return None;
    }
    t.overwrite(start..end, b"(true)").then_some(end)
}

/// P1103 modules. The grammar reads no module declaration, and `export`
/// before an ordinary declaration is the whole of what it adds to one.
fn module(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if let Some(end) = module_declaration(t, i) {
        t.blank(i..end, b' ');
        return Some(end);
    }
    if t.word_at(i, b"export") {
        let end = i + b"export".len();
        t.blank(i..end, b' ');
        return Some(end);
    }
    None
}

/// A braced default argument. The grammar reads `= Cfg{}` and `= 0`,
/// and no `= {}`, which is ordinary C++11. What follows the closing
/// brace is what says the value is one: a default argument ends at the
/// next parameter or at the parameter list.
///
/// An empty pair states value-initialization and carries no complexity,
/// so it goes. A pair with something in it becomes parentheses instead
/// of blanks, so that a call written inside a default is still a call
/// that this tool counts.
fn braced_default_argument(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if designator_before(t, i) {
        return None;
    }
    let open = t.next(i + 1).filter(|&k| t.at(k) == b'{')?;
    let close = t.match_close(open, b'{', b'}')?;
    if !t
        .next(close + 1)
        .is_some_and(|k| matches!(t.at(k), b',' | b')'))
    {
        return None;
    }
    if t.next(open + 1) == Some(close) {
        t.blank(i..close + 1, b' ');
        return Some(close + 1);
    }
    t.out[open] = b'(';
    t.out[close] = b')';
    Some(close + 1)
}

/// A qualified name after `typename`. The keyword disambiguates for the
/// compiler and the grammar has no rule for it here.
fn typename_qualified(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = i + b"typename".len();
    if !qualified_after(t, end) {
        return None;
    }
    t.blank(i..end, b' ');
    Some(end)
}

/// A name that follows a string literal expands to one.
fn string_macro(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !follows_a_string(t, i) {
        return None;
    }
    let end = t.word_end(i);
    t.blank(i..end, b' ');
    Some(end)
}

/// A macro the project declared to clang-format. The grammar has no
/// rule for a bare token in a declarator, and the name is the only
/// thing that says one is there.
fn declared_macro(t: &mut Text, i: usize, cx: &Cx) -> Option<usize> {
    if cx.macros.is_empty() || on_a_directive(t, i) {
        return None;
    }
    let kind = cx.macros.kind(t.word(i))?;
    let name_end = t.word_end(i);
    // The argument list, when the macro takes one. It goes with the
    // name, because what remains has to stand on its own and `(a, b);`
    // is not a declaration at namespace scope.
    let args = t
        .next(name_end)
        .filter(|&k| t.at(k) == b'(')
        .and_then(|open| t.match_paren(open).map(|close| (open, close)));
    let call_end = args.map_or(name_end, |(_, close)| close + 1);
    match kind {
        // It expands to an attribute, which is not complexity.
        Kind::Attribute => {
            t.blank(i..name_end, b' ');
            Some(name_end)
        }
        Kind::Statement => {
            t.blank(i..call_end, b' ');
            Some(call_end)
        }
        // The body stays a loop, because the macro writes one. A name
        // too short to hold `for (;;)` leaves a plain block, which
        // parses and counts one loop less.
        Kind::ForEach => {
            if !t.overwrite(i..call_end, b"for (;;)") {
                t.blank(i..call_end, b' ');
            }
            Some(call_end)
        }
        // The branch stays a branch, and it keeps its condition.
        Kind::Branch => {
            if t.overwrite(i..name_end, b"if") {
                return Some(name_end);
            }
            t.blank(i..call_end, b' ');
            Some(call_end)
        }
        // The argument IS the type, so only the name and the two
        // parentheses around it go, and the type stays where the
        // declaration put it.
        Kind::Typename => {
            if let Some((open, close)) = args {
                t.blank(i..open + 1, b' ');
                t.blank(close..close + 1, b' ');
                return Some(close + 1);
            }
            t.blank(i..name_end, b' ');
            Some(name_end)
        }
        Kind::Namespace => {
            if !t.overwrite(i..call_end, b"namespace") {
                t.blank(i..call_end, b' ');
            }
            Some(call_end)
        }
    }
}

/// Every rewrite, in the order the driver tries them at one position.
const RULES: &[Rule] = &[
    Rule {
        name: "reflect",
        paper: "P2996",
        trigger: Trigger::Byte(b'^'),
        apply: reflect,
    },
    Rule {
        name: "splice",
        paper: "P2996",
        trigger: Trigger::Byte(b'['),
        apply: splice,
    },
    Rule {
        name: "attribute before friend",
        paper: "P2893",
        trigger: Trigger::Byte(b'['),
        apply: attribute_before_friend,
    },
    Rule {
        name: "annotation",
        paper: "P3394",
        trigger: Trigger::Byte(b'['),
        apply: annotation,
    },
    Rule {
        name: "pack index",
        paper: "P2662",
        trigger: Trigger::Byte(b'.'),
        apply: pack_index,
    },
    Rule {
        name: "binding pack",
        paper: "P1061",
        trigger: Trigger::Byte(b'.'),
        apply: binding_pack,
    },
    Rule {
        name: "expansion statement",
        paper: "P1306",
        trigger: Trigger::Words(&[b"template"]),
        apply: expansion_statement,
    },
    Rule {
        name: "delete with a reason",
        paper: "P2573",
        trigger: Trigger::Words(&[b"delete"]),
        apply: delete_reason,
    },
    Rule {
        name: "contract clause",
        paper: "P2900",
        trigger: Trigger::Words(&[b"pre", b"post"]),
        apply: contract,
    },
    Rule {
        name: "explicit object parameter",
        paper: "P0847",
        trigger: Trigger::Words(&[b"this"]),
        apply: explicit_object_parameter,
    },
    Rule {
        name: "lambda specifier",
        paper: "P1102",
        trigger: Trigger::Words(&[
            b"consteval",
            b"constexpr",
            b"static",
            b"mutable",
            b"noexcept",
        ]),
        apply: lambda_specifier,
    },
    Rule {
        name: "if consteval",
        paper: "P1938",
        trigger: Trigger::Words(&[b"consteval"]),
        apply: if_consteval,
    },
    Rule {
        name: "module declaration",
        paper: "P1103",
        trigger: Trigger::Words(&[b"export", b"module", b"import"]),
        apply: module,
    },
    Rule {
        name: "braced default argument",
        paper: "N2672",
        trigger: Trigger::Byte(b'='),
        apply: braced_default_argument,
    },
    Rule {
        name: "typename before a qualified name",
        paper: "N1478",
        trigger: Trigger::Words(&[b"typename"]),
        apply: typename_qualified,
    },
    Rule {
        name: "macro that expands to a string",
        paper: "N1653",
        trigger: Trigger::AnyWord,
        apply: string_macro,
    },
    Rule {
        name: "macro the project declared",
        paper: "clang-format",
        trigger: Trigger::AnyWord,
        apply: declared_macro,
    },
];

/// The rules that can fire at a byte, in the order `RULES` gives.
fn dispatch(byte: u8) -> &'static [&'static Rule] {
    static BY_BYTE: OnceLock<Vec<Vec<&'static Rule>>> = OnceLock::new();
    let table = BY_BYTE.get_or_init(|| {
        let mut table: Vec<Vec<&'static Rule>> = vec![Vec::new(); 256];
        for rule in RULES {
            match rule.trigger {
                Trigger::Byte(byte) => table[usize::from(byte)].push(rule),
                Trigger::Words(words) => {
                    for word in words {
                        table[usize::from(word[0])].push(rule);
                    }
                }
                Trigger::AnyWord => {
                    for byte in 0..=u8::MAX {
                        if is_word(byte) && !byte.is_ascii_digit() {
                            table[usize::from(byte)].push(rule);
                        }
                    }
                }
            }
        }
        table
    });
    &table[usize::from(byte)]
}

/// The rewritten source, and the rules that wrote it.
pub struct Rewritten {
    pub text: String,
    /// Which rules fired, by name and paper, in order.
    pub rules: Vec<(&'static str, &'static str)>,
}

/// Rewrite the C++ the grammar cannot read. `None` when the source
/// needs none of it, so the common file is not copied.
pub fn rewrite(src: &str, macros: &Macros) -> Option<Rewritten> {
    let mut text = Text::new(src.as_bytes());
    let cx = Cx { macros };
    let mut fired: Vec<(&'static str, &'static str)> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if !text.is_code(i) {
            i += 1;
            continue;
        }
        let mut next = i + 1;
        for rule in dispatch(text.at(i)) {
            let matched = match rule.trigger {
                Trigger::Byte(_) => true,
                Trigger::Words(words) => words.iter().any(|word| text.word_at(i, word)),
                Trigger::AnyWord => text.starts_word(i),
            };
            if !matched {
                continue;
            }
            if let Some(end) = (rule.apply)(&mut text, i, &cx) {
                fired.push((rule.name, rule.paper));
                // A rule that returns where it started would spin, and
                // one byte of progress is enough to prevent that.
                next = end.max(i + 1);
                break;
            }
        }
        i = next;
    }
    if fired.is_empty() {
        return None;
    }
    // A span is bounded by ASCII tokens, so a rewrite covers every byte
    // of any character inside it. If some file proves otherwise, the
    // original text is still measurable, and a panic would not be.
    String::from_utf8(text.out)
        .ok()
        .map(|text| Rewritten { text, rules: fired })
}

/// The rewritten source alone, for the one caller that measures.
pub fn normalize(src: &str, macros: &Macros) -> Option<String> {
    rewrite(src, macros).map(|done| done.text)
}

/// How many times each rule fired on this source, by name and paper, in
/// the order each one first fired. `--errors` prints this beside the
/// parse errors that remain, because a rule that fired near one of them
/// is the first thing to read.
pub fn rewrites(src: &str, macros: &Macros) -> Vec<(&'static str, &'static str, usize)> {
    let mut counts: Vec<(&'static str, &'static str, usize)> = Vec::new();
    for (name, paper) in rewrite(src, macros).map_or_else(Vec::new, |done| done.rules) {
        match counts.iter_mut().find(|(seen, _, _)| *seen == name) {
            Some(entry) => entry.2 += 1,
            None => counts.push((name, paper, 1)),
        }
    }
    counts
}

/// Every rule, by name and paper, for the test that holds each one to a
/// case that reaches it.
#[cfg(test)]
pub fn rules() -> Vec<(&'static str, &'static str)> {
    RULES.iter().map(|rule| (rule.name, rule.paper)).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use crate::clangfmt::Macros;
    use crate::facts::extract;
    use crate::lang::Lang;

    /// A project that declares nothing, which is what every test about
    /// the standard syntax measures against.
    fn normalize(src: &str) -> Option<String> {
        super::normalize(src, &Macros::default())
    }

    /// The same rewrite, against a project that declared these names.
    fn with_macros(src: &str, yaml: &str) -> Option<String> {
        super::normalize(src, &Macros::read(yaml))
    }

    fn normalized(src: &str) -> crate::facts::FileFacts {
        let pack = Lang::Cpp.pack();
        let text = normalize(src).unwrap_or_else(|| src.to_string());
        extract(pack, &mut pack.make_parser(), Path::new("t.cpp"), &text)
    }

    /// The two invariants every rewrite owes the report. A finding
    /// prints a byte offset and a line number, and both of them are
    /// offsets into the text the parser read.
    fn same_length(src: &str) {
        if let Some(out) = normalize(src) {
            assert_eq!(out.len(), src.len(), "a rewrite moved a byte offset");
            assert_eq!(
                out.bytes().filter(|&b| b == b'\n').count(),
                src.bytes().filter(|&b| b == b'\n').count(),
                "a rewrite moved a line"
            );
        }
    }

    /// Source that the grammar cannot read as written. Each one of
    /// these re-parents every declaration below it while it fails.
    const DIALECT: &[(&str, &str)] = &[
        (
            "delete with a reason",
            "struct S { S(const S&) = delete(\"no copy\"); };",
        ),
        ("reflect", "constexpr auto r = ^^Widget;"),
        ("reflect a type keyword", "constexpr auto r = ^^int;"),
        (
            "reflect unsigned long",
            "constexpr auto r = ^^unsigned long;",
        ),
        ("reflect the global namespace", "constexpr auto r = ^^::;"),
        ("splice a type", "using T = [:r:];"),
        ("splice a member", "int y = obj.[:m:];"),
        ("splice a nested splice", "auto q = [:members[:i:]:];"),
        ("annotation", "struct [[=1]] S { int f() { return 0; } };"),
        (
            "expansion statement",
            "void f() { template for (constexpr auto m : ms) { use(m); } }",
        ),
        (
            "contract on a free function",
            "int f(int n) pre(n > 0) { return n; }",
        ),
        (
            "contract with a result binding",
            "int f(int n) post(r: r > 0) { return n; }",
        ),
        (
            "contract after noexcept",
            "int f(int n) noexcept pre(n > 0) { return n; }",
        ),
        (
            "contract after const",
            "struct S { int f() const pre(ok()) { return 1; } };",
        ),
        (
            "contract after a trailing return",
            "auto f(int n) -> int pre(n > 0) { return n; }",
        ),
        (
            "contract after a ref qualifier",
            "struct S { int f() & pre(x) { return 1; } };",
        ),
        (
            "contract after an rvalue ref",
            "struct S { int f() && pre(x) { return 1; } };",
        ),
        (
            "contract after const volatile",
            "struct S { int f() const volatile pre(x) { return 1; } };",
        ),
        (
            "contract on a lambda",
            "auto g = [](int n) pre(n > 0) { return n; };",
        ),
        (
            "pack indexing",
            "template <class... T> using First = T...[0];",
        ),
        (
            "structured binding pack",
            "void f() { auto [x, ...rest] = t; }",
        ),
        (
            "explicit object parameter",
            "struct S { int f(this S& self) { return 1; } };",
        ),
        (
            "if consteval",
            "int f() { if consteval { return 1; } else { return 2; } }",
        ),
        (
            "module declaration",
            "export module widget;\nexport int f() { return 1; }\n",
        ),
    ];

    /// Ordinary C++ that happens to spell one of the words a rewrite
    /// looks for. None of it may change. A rewrite here would delete
    /// real work, and the complexity of that work would go unreported
    /// without any sign that it had.
    const ORDINARY: &[(&str, &str)] = &[
        (
            "pre and post as names",
            "int f(int x) { int y = pre(x); return post(y); }",
        ),
        (
            "a call on a member",
            "int f(A a) { return a.pre(1) + a.post(2); }",
        ),
        (
            "a call through a pointer",
            "int f(A* a) { return a->pre(1) + a->post(2); }",
        ),
        (
            "a call after a condition",
            "int f() { if (ready()) pre(1); return 0; }",
        ),
        (
            "a call after a loop head",
            "int f() { while (go()) post(1); return 0; }",
        ),
        (
            "a call behind an operator",
            "int f() { return g() * pre(2); }",
        ),
        (
            "a call behind a bitwise and",
            "int f() { return g() & pre(2); }",
        ),
        (
            "a call inside an argument",
            "int f() { return h(g() & pre(2)); }",
        ),
        (
            "a qualified call",
            "int f() { return N::pre(1) + N::post(2); }",
        ),
        (
            "a call in a ternary",
            "int f(int h) { return h ? pre(1) : post(2); }",
        ),
        ("a function named pre", "int pre(int x) { return x; }"),
        (
            "a method named pre",
            "struct S { int pre(int x) { return x; } };",
        ),
        (
            "this as an argument",
            "struct S { void f() { g(this); h(this, 1); } };",
        ),
        (
            "this through an arrow",
            "struct S { int x; int f() { return this->x; } };",
        ),
        ("a returned this", "struct S { S* f() { return this; } };"),
        (
            "an ordinary delete",
            "void f(int* p) { delete p; delete[] p; }",
        ),
        ("varargs", "void f(int, ...);"),
        (
            "a pack expansion",
            "template <class... T> void f(T... a) { g(a...); }",
        ),
        (
            "a catch-all",
            "void f() { try { g(); } catch (...) { h(); } }",
        ),
        (
            "a pack of base classes",
            "template <class... T> struct S : T... { };",
        ),
        (
            "module as an identifier",
            "int f() { int module = 1; return module; }",
        ),
        ("import as a call", "void f() { import(3); }"),
        ("exclusive or", "int f(int a, int b) { return a ^ b; }"),
        ("an attribute", "[[nodiscard]] int f() { return 1; }"),
        (
            "a capture by value",
            "void f() { auto l = [=]() { return 1; }; l(); }",
        ),
        ("a subscript", "int f(A a) { return a[1] + a[2]; }"),
        (
            "consteval as a specifier",
            "consteval int f() { return 1; }",
        ),
        (
            "a template member call",
            "void f() { obj.template get<int>(); }",
        ),
    ];

    #[test]
    fn the_dialect_the_grammar_cannot_read_parses_after_a_rewrite() {
        for (what, src) in DIALECT {
            same_length(src);
            assert!(
                normalize(src).is_some(),
                "{what}: nothing was rewritten, so the grammar still cannot read it"
            );
            assert!(
                !normalized(src).low_confidence(),
                "{what}: still fails to parse"
            );
        }
    }

    #[test]
    fn ordinary_code_is_left_exactly_as_it_was() {
        for (what, src) in ORDINARY {
            assert_eq!(
                normalize(src),
                None,
                "{what}: a rewrite fired on code the grammar already reads"
            );
        }
    }

    #[test]
    fn ordinary_code_survives_a_file_that_needs_a_rewrite() {
        // A header carries both. The rewrite fires for the dialect, and
        // every byte of the ordinary code below it has to come through
        // unchanged. A test on the ordinary code alone cannot see this,
        // because there the rewrite never starts.
        for (dialect, head) in DIALECT {
            for (plain, tail) in ORDINARY {
                let src = format!("{head}\n{tail}\n");
                let Some(out) = normalize(&src) else {
                    panic!("{dialect}: the rewrite stopped firing");
                };
                assert_eq!(
                    &out[src.len() - tail.len() - 1..],
                    &src[src.len() - tail.len() - 1..],
                    "{dialect} + {plain}: the rewrite reached the ordinary code"
                );
            }
        }
    }

    #[test]
    fn a_deleted_copy_with_a_reason_still_parses() {
        // The whole point: without this the class closes early and every
        // member below it is re-parented.
        let src = "struct Arena {\n\
                   \x20 Arena(const Arena&) = delete(\"interior pointers would dangle\");\n\
                   \x20 int take(int n) { return n + 1; }\n\
                   };\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "delete(\"reason\") must parse");
        assert!(
            f.units.iter().any(|u| &*u.name == "take"),
            "the member after the deleted copy survives"
        );
    }

    #[test]
    fn a_reason_containing_parens_and_quotes_does_not_end_the_span_early() {
        let src = "struct S { S(S&&) = delete(\"use move(x) \\\"not\\\" copy\"); int f() { return 1; } };\n";
        same_length(src);
        assert!(!normalized(src).low_confidence());
    }

    #[test]
    fn reflection_and_splices_leave_a_readable_body() {
        let src = "template <typename T>\n\
                   consteval unsigned long hash(const T& obj) {\n\
                   \x20 unsigned long h = 0;\n\
                   \x20 h ^= mix(obj.[:member_of<T, 0>():]);\n\
                   \x20 return h + sizeof(^^T);\n\
                   }\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "reflect and splice must parse");
        assert!(f.units.iter().any(|u| &*u.name == "hash"));
    }

    #[test]
    fn an_expansion_statement_is_an_ordinary_loop() {
        let src = "void walk() {\n\
                   \x20 template for (constexpr auto m : members) {\n\
                   \x20   use(m);\n\
                   \x20 }\n\
                   }\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "template for must parse");
        assert_eq!(
            f.units.iter().filter(|u| &*u.name == "walk").count(),
            1,
            "the loop stays inside its function"
        );
    }

    #[test]
    fn contract_clauses_vanish_and_the_body_remains() {
        let src = "int take(int n) noexcept pre(n > 0) post(r: r > n) {\n\
                   \x20 return n + 1;\n\
                   }\n";
        same_length(src);
        let f = normalized(src);
        assert!(!f.low_confidence(), "contract clauses must parse");
        let take = f.units.iter().find(|u| &*u.name == "take").expect("take");
        assert_eq!(take.params.len(), 1, "a clause is not a parameter");
    }

    #[test]
    fn a_call_to_a_function_named_pre_is_left_alone() {
        // `pre` is only a clause where a clause can stand. Blanking a
        // call would delete real work and under-report complexity.
        let src = "int f(int x) {\n  int y = pre(x);\n  return post(y);\n}\n";
        assert_eq!(normalize(src), None, "no clause here, so no rewrite");
    }

    #[test]
    fn constructs_inside_comments_and_strings_are_not_code() {
        let src = "// x = delete(\"not real\") and ^^T and [:s:]\n\
                   const char* s = \"= delete(\\\"still not\\\") ^^T\";\n\
                   int f() { return 0; }\n";
        assert_eq!(normalize(src), None, "prose is not syntax");
    }

    #[test]
    fn a_raw_string_holding_the_syntax_is_left_alone() {
        let src = "const char* t = R\"sql(= delete(\"x\") ^^T [:y:])sql\";\n";
        assert_eq!(normalize(src), None, "a raw string is one literal");
    }

    #[test]
    fn a_file_without_any_of_it_is_not_copied() {
        assert_eq!(normalize("int main() { return 0; }\n"), None);
    }
    /// What a project declares, and what each declaration has to do to
    /// the one line under it.
    const DECLARED: &str = "AttributeMacros: [KEEP_ALIVE]\n\
                            StatementMacros: [LAYOUT_INVARIANT]\n\
                            ForEachMacros: [for_each_slot]\n\
                            IfMacros: [IF_SOME]\n\
                            TypenameMacros: [STACK_OF]\n\
                            NamespaceMacros: [TESTSUITE]\n";

    #[test]
    fn a_declared_macro_stops_being_a_parse_error() {
        let cases: &[(&str, &str)] = &[
            (
                "an attribute in a declarator",
                "int* keep(int& a KEEP_ALIVE) KEEP_ALIVE { return &a; }\n",
            ),
            (
                "a whole declaration at namespace scope",
                "LAYOUT_INVARIANT(Alias, int);\nint f() { return 1; }\n",
            ),
            (
                "a loop over a range",
                "void f(Table t) { for_each_slot(s, t) { use(s); } }\n",
            ),
            (
                "a branch",
                "void f(Maybe m) { IF_SOME(x, m) { use(x); } }\n",
            ),
            ("a type", "STACK_OF(Frame)* frames() { return 0; }\n"),
            ("a namespace", "TESTSUITE(Pool) { int f() { return 1; } }\n"),
        ];
        for (what, src) in cases {
            let out = with_macros(src, DECLARED);
            let text = out.clone().unwrap_or_else(|| src.to_string());
            assert_eq!(text.len(), src.len(), "{what}: a rewrite moved an offset");
            assert!(out.is_some(), "{what}: the declaration was not read");
            let pack = Lang::Cpp.pack();
            let f = extract(pack, &mut pack.make_parser(), Path::new("t.cpp"), &text);
            assert!(!f.low_confidence(), "{what}: still fails to parse");
        }
    }

    #[test]
    fn a_loop_macro_stays_a_loop_and_a_branch_macro_stays_a_branch() {
        // Blanking either one would leave a bare block, which parses
        // and reports one control structure less than the code has.
        let loops = with_macros("void f(T t) { for_each_slot(s, t) { g(s); } }\n", DECLARED);
        assert!(loops.is_some_and(|t| t.contains("for (;;)")), "not a loop");
        let branch = with_macros("void f(T m) { IF_SOME(x, m) { g(x); } }\n", DECLARED).unwrap();
        let head = branch.split_once('{').unwrap().1.trim_start();
        assert!(head.starts_with("if "), "not a branch: {branch}");
        assert!(branch.contains("(x, m)"), "the condition went: {branch}");
    }

    #[test]
    fn a_type_macro_keeps_the_type_it_wraps() {
        // The argument IS the type. Blanking it with the name would
        // leave a declaration with nothing to declare.
        let out = with_macros("STACK_OF(Frame)* frames() { return 0; }\n", DECLARED).unwrap();
        assert!(out.contains("Frame"), "the type went with the macro: {out}");
        assert!(out.trim_start().starts_with("Frame"), "{out}");
    }

    #[test]
    fn a_macro_the_project_did_not_declare_is_left_alone() {
        // The name is the whole of what identifies one. A tool that
        // guessed would blank real work and report less than is there.
        assert_eq!(
            with_macros("int f(int a UNDECLARED) { return a; }\n", DECLARED),
            None
        );
    }

    #[test]
    fn a_declaration_does_not_reach_a_word_that_merely_contains_it() {
        // `KEEP_ALIVE` is declared; `KEEP_ALIVE_2` is another name.
        let src = "int KEEP_ALIVE_2 = 1;\nint f() { return KEEP_ALIVE_2; }\n";
        assert_eq!(with_macros(src, DECLARED), None);
    }
    #[test]
    fn typename_before_a_qualified_name_goes_and_a_template_parameter_keeps_it() {
        let dependent = "void f() { g(typename sriov::VfIndex::Trusted{}); }\n";
        same_length(dependent);
        assert!(normalize(dependent).is_some(), "typename must go here");
        assert!(!normalized(dependent).low_confidence());
        // A template parameter list spells a plain name after the word,
        // and blanking it there would turn the parameter into a value.
        for kept in [
            "template <typename T> void f(T x) { }\n",
            "template <typename... Ts> void f(Ts... a) { }\n",
            "template <template <typename> class C> void f() { }\n",
        ] {
            assert_eq!(normalize(kept), None, "a template parameter kept its word");
        }
    }

    #[test]
    fn a_name_after_a_string_literal_is_a_macro_that_expands_to_one() {
        let src = "void f(long v) { printf(\"%016\" PRIx64 \"\\n\", v); }\n";
        same_length(src);
        let out = normalize(src).expect("the macro must go");
        assert!(!out.contains("PRIx64"), "{out}");
        assert!(!normalized(src).low_confidence());
    }

    #[test]
    fn an_include_does_not_reach_the_declaration_under_it() {
        // The rule above reads back to a quote. Without a line to stop
        // it, `#include "config.h"` blanks the `namespace` below it and
        // every file that holds one stops parsing.
        let src = "#include \"config.h\"\nnamespace crucible {\nint f() { return 1; }\n}\n";
        assert_eq!(normalize(src), None, "the include reached past its line");
        assert!(!normalized(src).low_confidence());
    }

    #[test]
    fn a_user_defined_literal_keeps_its_suffix() {
        // The suffix is written against the quote, and it names the
        // operator. Only a space makes the name a separate token.
        let src = "constexpr int operator\"\"_km(unsigned long long v) { return (int)v; }\n";
        assert_eq!(normalize(src), None, "the suffix went with the literal");
    }
    #[test]
    fn a_declaration_that_states_a_string_of_its_own_keeps_its_name() {
        // A name after a string is a macro, EXCEPT in the two
        // declarations that put a string of their own in front of one.
        // Blanking either leaves a declaration with no declarator.
        for kept in [
            "extern \"C\" int fuzz_one(const char* data, int size);\n",
            "extern \"C++\" void g();\n",
            "extern \"C\" {\nint h(int a);\n}\n",
            "constexpr int operator \"\" _km(unsigned long long v) { return (int)v; }\n",
        ] {
            assert_eq!(normalize(kept), None, "a rewrite ate a declarator: {kept}");
            assert!(!normalized(kept).low_confidence(), "{kept}");
        }
    }

    #[test]
    fn a_macro_is_not_rewritten_in_the_directive_that_defines_it() {
        // `#define KEEP_ALIVE [[gnu::always_inline]]` names the macro on
        // a line that is not a declarator. Blanking it there leaves
        // `#define` with nothing to define, and the file stops parsing.
        let src = "#define KEEP_ALIVE [[gnu::always_inline]]\n\
                   #ifdef KEEP_ALIVE\n\
                   int f(int a KEEP_ALIVE) { return a; }\n\
                   #endif\n";
        let out = with_macros(src, DECLARED).expect("the declarator use is rewritten");
        assert!(
            out.contains("#define KEEP_ALIVE"),
            "the definition went: {out}"
        );
        assert!(out.contains("#ifdef KEEP_ALIVE"), "the guard went: {out}");
        assert!(!out.contains("int a KEEP_ALIVE"), "the use stayed: {out}");
    }
    #[test]
    fn a_braced_default_argument_parses_and_keeps_the_calls_in_it() {
        for src in [
            "void f(int x = {}) { }\n",
            "void f(Cfg c = {}) { }\n",
            "void f(Cfg = {}) { }\n",
            "void f(std::type_identity<Row> = {}) { }\n",
            "struct S { S(Cfg c = {}) noexcept : c_{c} {} Cfg c_; };\n",
            "void f(Cfg c = {1, 2}) { }\n",
        ] {
            same_length(src);
            assert!(normalize(src).is_some(), "not rewritten: {src}");
            assert!(!normalized(src).low_confidence(), "still fails: {src}");
        }
        // A value becomes parentheses and not blanks, so a call written
        // in a default is a call this tool still counts.
        let out = normalize("void f(Cfg c = {compute(), 2}) { }\n").unwrap();
        assert!(
            out.contains("compute()"),
            "the call went with the braces: {out}"
        );
        // A braced initializer that is not a default argument already
        // parses, and nothing here may touch it.
        for kept in ["int x = {};\n", "struct S { int x = {}; };\n"] {
            assert_eq!(normalize(kept), None, "rewrote an initializer: {kept}");
        }
    }
    #[test]
    fn a_designated_initializer_is_not_a_default_argument() {
        // `.pad = {},` sits in a braced list, where the comma opens the
        // next designator. Blanking the value leaves `.pad ,` and the
        // list stops parsing, which is worse than what it replaced.
        let src = "void f() {\n\
                   \x20 Meta m = {.layout = Strided,\n\
                   \x20            .pad = {},\n\
                   \x20            .slot = SlotId{SL_X},\n\
                   \x20            .pad2 = {}};\n\
                   \x20 use(m);\n\
                   }\n";
        assert_eq!(normalize(src), None, "a designator was read as a default");
        assert!(!normalized(src).low_confidence());
    }
    #[test]
    fn an_attribute_before_friend_goes_and_the_declaration_stays() {
        // The grammar reads `friend`, and it reads an attribute, and
        // not the two together.
        for src in [
            "struct S { [[nodiscard]] friend bool ok(S const& s) { return true; } };\n",
            "struct S { [[nodiscard]] friend constexpr bool ok(S const& s); };\n",
            "template <class T> struct S { [[nodiscard]] friend constexpr bool ok(S const& s) { return true; } };\n",
        ] {
            same_length(src);
            let out = normalize(src).expect("the attribute must go");
            assert!(out.contains("friend"), "the declaration went: {out}");
            assert!(!out.contains("nodiscard"), "the attribute stayed: {out}");
            assert!(!normalized(src).low_confidence(), "still fails: {src}");
        }
        // An attribute that stands anywhere else already parses.
        assert_eq!(normalize("[[nodiscard]] int f() { return 1; }\n"), None);
    }

    #[test]
    fn a_contract_clause_reads_under_a_defaulted_template_parameter() {
        // `template <class R = Row>` spells an `=` that is a default
        // template argument, not an assignment. A scan that reads it as
        // one rejects every contract clause on a template.
        let src = "template <class R = Row>\n\
                   struct Pool {\n\
                   \x20 explicit Pool(unsigned n)\n\
                   \x20     pre(n >= 8)\n\
                   \x20     : n_{n} {}\n\
                   \x20 unsigned n_;\n\
                   };\n";
        same_length(src);
        assert!(normalize(src).is_some(), "the clause was read as a call");
        assert!(!normalized(src).low_confidence());
    }
    #[test]
    fn a_lambda_specifier_with_no_parameter_list_goes() {
        // `[] consteval {}` is C++23. The grammar reads each specifier
        // after a `()` and none of them without one.
        for src in [
            "auto f = [] consteval { return 1; };\n",
            "auto f = [] constexpr { return 1; };\n",
            "auto f = [] static { return 1; };\n",
            "auto f = [] mutable { return 1; };\n",
            "auto f = [] noexcept { return 1; };\n",
            "auto f = [x] consteval { return x; };\n",
        ] {
            same_length(src);
            assert!(normalize(src).is_some(), "not rewritten: {src}");
            assert!(!normalized(src).low_confidence(), "still fails: {src}");
        }
        // A `]]` before the word closes an attribute, and the
        // declaration under it keeps its specifier.
        for kept in [
            "[[nodiscard]] constexpr int f() { return 1; }\n",
            "struct S { [[nodiscard]] static constexpr int f() { return 1; } };\n",
            "auto f = []() consteval { return 1; };\n",
        ] {
            assert_eq!(normalize(kept), None, "rewrote a declaration: {kept}");
        }
    }

    /// One short source for each construct of the language that carries
    /// syntax, from C++98 to C++2d, with the rows of the Clang
    /// conformance table as the list. A row that states a rule of
    /// meaning and no syntax has no probe here, because a parser cannot
    /// see one.
    ///
    /// `### name` opens a probe, and a header that says `(not read
    /// yet)` marks a construct that no rule covers. The test holds both
    /// directions: a probe without the mark has to read, and a probe
    /// with it has to fail, so that a rule which starts to cover one
    /// forces the mark off.
    const PROBES: &str = include_str!("dialect_probes.txt");

    struct Probe {
        name: &'static str,
        pending: bool,
        source: &'static str,
    }

    fn probes() -> Vec<Probe> {
        let mut probes = Vec::new();
        let mut rest = PROBES;
        while let Some(head_end) = rest.find('\n') {
            let head = rest[..head_end]
                .strip_prefix("### ")
                .expect("a probe opens with ### and its name");
            let body = &rest[head_end + 1..];
            // The body keeps its last line break. A directive on the
            // last line of a probe needs that break to end.
            let len = body.find("\n### ").map_or(body.len(), |k| k + 1);
            probes.push(Probe {
                name: head.split_whitespace().next().expect("a probe has a name"),
                pending: head.contains("(not read yet)"),
                source: &body[..len],
            });
            rest = &body[len..];
        }
        probes
    }

    /// The ERROR and MISSING nodes in the tree, counted the way
    /// `--errors` counts them. `FileFacts::parse_errors` counts fewer,
    /// because the walk that fills it does not reach every MISSING node.
    fn parse_errors(lang: Lang, text: &str) -> usize {
        let tree = lang
            .pack()
            .make_parser()
            .parse(text, None)
            .expect("the parser returns a tree");
        let mut stack = vec![tree.root_node()];
        let mut count = 0;
        while let Some(node) = stack.pop() {
            count += usize::from(node.is_error() || node.is_missing());
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        count
    }

    #[test]
    fn every_probe_reads_after_a_rewrite() {
        let mut wrong = Vec::new();
        for probe in probes() {
            let text = normalize(probe.source).unwrap_or_else(|| probe.source.to_string());
            assert_eq!(
                text.len(),
                probe.source.len(),
                "{}: a rewrite moved a byte offset",
                probe.name
            );
            assert_eq!(
                text.bytes().filter(|&b| b == b'\n').count(),
                probe.source.bytes().filter(|&b| b == b'\n').count(),
                "{}: a rewrite moved a line",
                probe.name
            );
            // The language the CLI would read the file as: a `.cpp` that
            // spells `__device__` is CUDA.
            let lang = Lang::of_source(Path::new("probe.cpp"), probe.source)
                .expect("a .cpp path names a language");
            let errors = parse_errors(lang, &text);
            match (probe.pending, errors) {
                (false, 0) | (true, 1..) => {}
                (false, n) => wrong.push(format!("{}: {n} parse errors", probe.name)),
                (true, 0) => wrong.push(format!(
                    "{}: reads now, so drop the (not read yet) mark",
                    probe.name
                )),
            }
        }
        assert!(
            wrong.is_empty(),
            "{} probes disagree with the file:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }

    #[test]
    fn every_rule_has_a_probe_that_reaches_it() {
        // A rule no probe reaches is a rule no test holds to its
        // meaning, and the next edit to it is unmeasured.
        let mut reached = BTreeSet::new();
        let mut reach = |src: &str, macros: &Macros| {
            if let Some(done) = super::rewrite(src, macros) {
                reached.extend(done.rules.iter().map(|&(name, _)| name));
            }
        };
        for probe in probes() {
            reach(probe.source, &Macros::default());
        }
        // The macro rules answer to a declaration, and the probe file
        // states none, because a probe is one file with no project.
        let declared = Macros::read(DECLARED);
        for src in [
            "int f(int a KEEP_ALIVE) { return a; }\n",
            "LAYOUT_INVARIANT(Alias, int);\n",
            "void f(T t) { for_each_slot(s, t) { g(s); } }\n",
            "void f(T m) { IF_SOME(x, m) { g(x); } }\n",
            "STACK_OF(Frame)* frames() { return 0; }\n",
            "TESTSUITE(Pool) { int f() { return 1; } }\n",
        ] {
            reach(src, &declared);
        }
        let missing: Vec<String> = super::rules()
            .into_iter()
            .filter(|(name, _)| !reached.contains(name))
            .map(|(name, paper)| format!("{name} ({paper})"))
            .collect();
        assert!(
            missing.is_empty(),
            "no probe reaches: {}",
            missing.join(", ")
        );
    }
}
