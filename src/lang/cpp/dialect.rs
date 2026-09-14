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
//! and underscores keep all of them true. A span that stood where a
//! name stands becomes a name, two underscores and then spaces, because
//! `obj.[:m:]` has to stay a member access and `obj.` and then blanks is
//! not one.
//!
//! None of this is an opinion about the code. It is the smallest edit
//! that lets the grammar read the shape the compiler reads. What goes is
//! what carries no work: an attribute, a contract, a specifier, a
//! disambiguator. What stays is every call, branch and loop.
//!
//! One rewrite is one function. `PASSES` lists the rewrites that read
//! the whole file at one time, and `RULES` lists the rewrites that fire
//! at one position, each with the paper that adds the syntax. A rule
//! states the byte or the word that can start it, so a file pays for the
//! rules its text can reach and not for all of them.
//!
//! The grammar moves, and the language moves faster. A construct that
//! no rule here covers stays a parse error, and `--errors` names the
//! file and the line. That is the signal to add a rule. The failure
//! mode is loud on purpose.

use std::ops::Range;
use std::sync::OnceLock;

use crate::clangfmt::{Kind, Macros};

// ---------------------------------------------------------------------
// The text under rewrite
// ---------------------------------------------------------------------

/// The bytes, which of them are code, and where the quoted literals are.
struct Text {
    out: Vec<u8>,
    code: Vec<bool>,
    literals: Vec<Range<usize>>,
}

impl Text {
    fn new(out: Vec<u8>) -> Text {
        let (code, literals) = lex(&out);
        Text {
            out,
            code,
            literals,
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

    /// The first byte at or after `at` that is not whitespace, code or
    /// not. A literal is not code, and a rule that reads one needs this.
    fn next_any(&self, at: usize) -> Option<usize> {
        (at..self.len()).find(|&i| !self.out[i].is_ascii_whitespace())
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
    /// they are, and the backslashes that join a line to the next. A
    /// `delete("...")` reason or a contract predicate can run across
    /// several lines. A rewrite that removes a line break moves every
    /// finding below it onto the wrong line, and a rewrite that removes
    /// a splice joins two lines that the compiler reads apart.
    fn blank(&mut self, range: Range<usize>, fill: u8) {
        for k in range {
            let byte = self.out[k];
            let splice = byte == b'\\'
                && match self.out.get(k + 1) {
                    Some(b'\n') => true,
                    Some(b'\r') => self.out.get(k + 2) == Some(&b'\n'),
                    _ => false,
                };
            if !matches!(byte, b'\n' | b'\r') && !splice {
                self.out[k] = fill;
            }
        }
    }

    /// Put a name where the span was: two underscores, then spaces. A
    /// span that runs across lines stays one name, because underscores
    /// on every line would read as one name for each line.
    fn name(&mut self, range: Range<usize>) {
        let start = range.start;
        let end = range.end;
        self.blank(range, b' ');
        for k in start..end.min(start + 2) {
            if !matches!(self.out[k], b'\n' | b'\r') {
                self.out[k] = b'_';
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
    fn overwrite(&mut self, range: Range<usize>, text: &[u8]) -> bool {
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

/// Which bytes are code, and not comment or literal text, and where each
/// quoted literal is. Every rewrite reads the mask first. A `^^` inside
/// a string is a string, and a `pre(` in a doc comment is prose.
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
fn lex(src: &[u8]) -> (Vec<bool>, Vec<Range<usize>>) {
    let mut code = vec![true; src.len()];
    let mut literals = Vec::new();
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
            b'#' => match directive_text(src, i) {
                Some(text) => {
                    code[text.clone()].fill(false);
                    i = text.end;
                }
                None => i += 1,
            },
            b'\'' if digit_separator(src, i) => i += 1,
            b'"' | b'\'' => match literal_end(src, i) {
                Some(end) => {
                    code[i..end].fill(false);
                    literals.push(i..end);
                    i = end;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    (code, literals)
}

/// The part of a directive that the grammar reads as raw text, and not
/// as tokens. That text is not code, for two reasons.
///
/// `#error` and `#warning` carry prose, and prose carries apostrophes.
///
/// The body of a `#define`, after its name and its parameter list, is
/// raw text to the grammar, and so is the argument of `#undef`,
/// `#pragma` and `#line`. A rule that edits that text changes no parse,
/// and it can damage the directive: a `static_assert` in a macro that
/// loses its message loses the backslash at the end of its line too,
/// and the macro stops at that line.
///
/// `None` for a directive whose argument the grammar parses, such as
/// `#if`, `#include` and `#embed`, and for a `#` that does not open its
/// line, such as the stringize operator.
fn directive_text(src: &[u8], at: usize) -> Option<Range<usize>> {
    let line_start = src[..at]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |k| k + 1);
    if src[line_start..at]
        .iter()
        .any(|b| !matches!(b, b' ' | b'\t'))
    {
        return None;
    }
    let mut end = line_comment_end(src, at);
    if directive_is_prose(src, at) {
        return Some(at..end);
    }
    let word = (at + 1..end).find(|&k| !matches!(src[k], b' ' | b'\t'))?;
    let word_stop = word_end(src, word);
    let start = match &src[word..word_stop] {
        b"undef" | b"pragma" | b"line" | b"ident" | b"sccs" | b"assert" | b"unassert" => word_stop,
        b"define" => {
            let name = (word_stop..end).find(|&k| !matches!(src[k], b' ' | b'\t'))?;
            let name_stop = word_end(src, name);
            // A function-like macro writes `(` against its name, and the
            // grammar parses the parameters in it.
            if src.get(name_stop) == Some(&b'(') {
                (name_stop..end)
                    .find(|&k| src[k] == b')')
                    .map_or(end, |k| k + 1)
            } else {
                name_stop
            }
        }
        _ => return None,
    };
    // A block comment that opens in the text and closes on a later line
    // is still one comment, and the text ends where it does.
    if let Some(open) = find(&src[..end], start, b"/*")
        && find(&src[..end], open + 2, b"*/").is_none()
    {
        end = find(src, open + 2, b"*/").map_or(src.len(), |close| close + 2);
    }
    (start < end).then_some(start..end)
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

/// The start of each line, and the end of each, the line break not
/// included.
fn lines(src: &[u8]) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start > src.len() || (start == src.len() && !src.is_empty()) {
            return None;
        }
        let end = find(src, start, b"\n").unwrap_or(src.len());
        let line = start..end;
        start = end + 1;
        Some(line)
    })
}

/// The directive a line holds, as its word: `if` for `#  if 0`.
fn directive_word(line: &[u8]) -> Option<&[u8]> {
    let hash = line.iter().position(|&b| !matches!(b, b' ' | b'\t'))?;
    if line[hash] != b'#' {
        return None;
    }
    let word = hash
        + 1
        + line[hash + 1..]
            .iter()
            .position(|&b| !matches!(b, b' ' | b'\t'))?;
    Some(&line[word..word_end(line, word)])
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

/// The words that open the head of a class.
const CLASS_KEYS: [&[u8]; 3] = [b"class", b"struct", b"union"];

/// The characters an overloaded operator is spelled with.
const OPERATOR_BYTES: &[u8] = b"+-*/%^&|~!=<>,";

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
    open_paren_of(t, at).is_some_and(|open| {
        t.prev(open)
            .is_some_and(|p| CONDITIONS.contains(&t.word_before(p + 1)))
    })
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

/// The `[` that opens the brackets that `close` ends.
fn open_bracket_of(t: &Text, close: usize) -> Option<usize> {
    let mut depth = 0u32;
    for i in (0..=close).rev() {
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b']' => depth += 1,
            b'[' => {
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

/// True when a declaration can start at `at`: nothing stands before it,
/// or a statement ends before it, or a directive line does.
fn starts_a_statement(t: &Text, at: usize) -> bool {
    t.prev(at)
        .is_none_or(|p| matches!(t.at(p), b';' | b'{' | b'}') || on_a_directive(t, p))
}

/// The start of `operator` when the tokens before the `(` at `open`
/// spell the name of an operator: `operator=`, `operator()`,
/// `operator[]`. The `=` in `operator=` is not an assignment.
fn operator_name_start(t: &Text, open: usize) -> Option<usize> {
    let mut i = t.prev(open)?;
    if matches!(t.at(i), b')' | b']') {
        let pair = if t.at(i) == b')' { b'(' } else { b'[' };
        i = t.prev(i).filter(|&k| t.at(k) == pair)?;
        i = t.prev(i)?;
    } else if OPERATOR_BYTES.contains(&t.at(i)) {
        while OPERATOR_BYTES.contains(&t.at(i)) {
            i = t.prev(i)?;
        }
    } else {
        return None;
    }
    (t.word_before(i + 1) == b"operator").then(|| i + 1 - b"operator".len())
}

/// True when the parameter list that ends at `close` belongs to a
/// declaration, and not to a call in an expression.
///
/// The scan steps over the parameter list and the name before it, and
/// reads back to the start of the statement. Four things end it. A `]`
/// directly before the list is the introducer of a lambda, and a lambda
/// takes a contract clause of its own. An unmatched `(` or `[` means an
/// argument list or a subscript holds the call, as in `f(g() & pre(h))`.
/// An `=` or one of the expression keywords before it means an
/// expression, as in `return g() & pre(2)`. A statement boundary means a
/// declaration.
fn declares_a_function(t: &Text, close: usize) -> bool {
    let Some(open) = open_paren_of(t, close) else {
        return false;
    };
    let head = operator_name_start(t, open).unwrap_or(open);
    if head == open && t.prev(open).is_some_and(|p| t.at(p) == b']') {
        return true;
    }
    let mut depth = 0u32;
    let mut i = head;
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

/// The start of a trailing requires-clause that ends at `last`, when one
/// does. `f() requires C<T> pre(x)` puts a constraint between the
/// declarator and the contract clause, and the scan for the declarator
/// has to step over it.
///
/// The scan reads only what a constraint can hold, and it stops at the
/// declarator. A qualifier such as `noexcept` ends a declarator, and so
/// does a parameter list, which follows a name where a parenthesized
/// constraint follows `requires` or an operator. Without those two
/// stops the scan walks past the declarator into the `requires` of the
/// template header above it, and every contract clause under such a
/// header reads as a call.
fn requires_clause_start(t: &Text, last: usize) -> Option<usize> {
    let mut i = last;
    loop {
        let byte = t.at(i);
        if is_word(byte) {
            let word = t.word_before(i + 1);
            let start = i + 1 - word.len();
            if word == b"requires" {
                return Some(start);
            }
            if QUALIFIERS.contains(&word) || EXPRESSIONS.contains(&word) {
                return None;
            }
            i = t.prev(start)?;
            continue;
        }
        match byte {
            b')' => {
                let open = open_paren_of(t, i)?;
                let before = t.prev(open)?;
                if is_word(t.at(before)) && t.word_before(before + 1) != b"requires" {
                    return None;
                }
                i = open;
            }
            b'>' => i = angle_open(t, i)?,
            b':' | b'&' | b'|' | b'!' | b'<' => {}
            _ => return None,
        }
        i = t.prev(i)?;
    }
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
///    virt-specifier, a trailing return type, or a requires-clause.
/// 2. The `)` it reads back to closes a parameter list. It does not
///    close the condition of an `if`, and the declarator it ends is not
///    a call in an expression.
/// 3. A body or another clause follows it. A call is followed by an
///    operator or an argument.
fn contract_clause(t: &Text, at: usize) -> Option<usize> {
    let open = t.next(t.word_end(at)).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;

    // 3. What follows a clause is a body, a declaration end, another
    //    clause, a trailing return type, the initializer list of a
    //    constructor, `= default` or `= 0`, or an attribute.
    let follows = t.next(close + 1)?;
    let tail_ok = matches!(t.at(follows), b'{' | b';' | b'-' | b'=' | b':')
        || t.starts(follows, b"[[")
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
    if let Some(requires) = requires_clause_start(t, i) {
        i = t.prev(requires)?;
    }
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

/// The end of a type-id that starts at `at` with a type keyword or a
/// cv-qualifier, as in `unsigned long`, `const char*` or `const
/// Widget&`. `None` when the tokens at `at` are an expression, and an
/// expression needs no rewrite to stand where one stands.
fn keyword_type_end(t: &Text, at: usize) -> Option<usize> {
    let mut end = None;
    let mut cv = false;
    let mut named = false;
    let mut i = at;
    while let Some(k) = t.next(i) {
        let byte = t.at(k);
        if is_word(byte) {
            let word = t.word(k);
            if word == b"const" || word == b"volatile" {
                cv = true;
            } else if BUILTIN_TYPES.contains(&word) {
                named = true;
            } else if cv && !named {
                // The type a cv-qualifier names, with its scope.
                named = true;
                let mut stop = t.word_end(k);
                while let Some(colons) = t.next(stop).filter(|&c| t.starts(c, b"::")) {
                    match t.next(colons + 2).filter(|&w| is_word(t.at(w))) {
                        Some(w) => stop = t.word_end(w),
                        None => break,
                    }
                }
                end = Some(stop);
                i = stop;
                continue;
            } else {
                break;
            }
            end = Some(t.word_end(k));
            i = t.word_end(k);
            continue;
        }
        if matches!(byte, b'*' | b'&') && named {
            end = Some(k + 1);
            i = k + 1;
            continue;
        }
        break;
    }
    end.filter(|_| named || cv)
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
    if !starts_a_statement(t, at) {
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

/// True when the `]` at `close` ends the introducer of a lambda, and
/// not a subscript, the brackets of `delete[]`, or an attribute.
fn ends_a_lambda_introducer(t: &Text, close: usize) -> bool {
    if close > 0 && t.at(close - 1) == b']' {
        return false;
    }
    let Some(open) = open_bracket_of(t, close) else {
        return false;
    };
    if t.get(open + 1) == Some(b'[') {
        return false;
    }
    match t.prev(open) {
        None => true,
        // An operand before `[` makes the brackets a subscript. A
        // keyword that an expression follows does not.
        Some(p) if is_word(t.at(p)) => {
            let word = t.word_before(p + 1);
            EXPRESSIONS.contains(&word) && !matches!(word, b"new" | b"delete")
        }
        Some(p) => !matches!(t.at(p), b')' | b']' | b'>' | b'['),
    }
}

/// True when `at` sits in the body of a class, and not in the body of a
/// function, a namespace or an enumeration.
fn in_a_class_body(t: &Text, at: usize) -> bool {
    let mut depth = 0u32;
    let mut i = at;
    let open = loop {
        if i == 0 {
            return false;
        }
        i -= 1;
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b'}' => depth += 1,
            b'{' if depth == 0 => break i,
            b'{' => depth -= 1,
            _ => {}
        }
    };
    in_a_class_head(t, open)
}

/// True when the tokens before `at`, back to the start of the statement,
/// are the head of a class: a class key stands there, and no `(`, `)`,
/// `=` or `enum` does.
fn in_a_class_head(t: &Text, at: usize) -> bool {
    let mut class = false;
    let mut k = at;
    while let Some(p) = t.prev(k) {
        match t.at(p) {
            b';' | b'{' | b'}' => break,
            b'(' | b')' | b'=' => return false,
            byte if is_word(byte) => {
                let word = t.word_before(p + 1);
                if word == b"enum" {
                    return false;
                }
                class |= CLASS_KEYS.contains(&word);
                k = p + 1 - word.len();
                continue;
            }
            _ => {}
        }
        k = p;
    }
    class
}

/// True when `at` sits in the parameter list of a template header.
fn in_a_template_header(t: &Text, at: usize) -> bool {
    enclosing_angle(t, at).is_some_and(|open| {
        t.prev(open)
            .is_some_and(|p| t.word_before(p + 1) == b"template")
    })
}

/// True when `at` sits in the argument list of a template name.
fn in_template_arguments(t: &Text, at: usize) -> bool {
    enclosing_angle(t, at).is_some_and(|open| {
        t.prev(open).is_some_and(|p| {
            (is_word(t.at(p)) && t.word_before(p + 1) != b"template") || t.at(p) == b'>'
        })
    })
}

/// The `<` that holds `at` when the nearest unmatched bracket before it
/// is an angle bracket, and not a parenthesis, a bracket or a brace.
///
/// The scan stops at the ninth braced group that it passes at the top
/// level. A template argument list holds one such group for each braced
/// argument, and a table holds one for each row. Without the stop, each
/// row of a table scans back to the first row: v8's gay-fixed.cc has
/// 100000 rows, and its rewrite took hours.
fn enclosing_angle(t: &Text, at: usize) -> Option<usize> {
    const BRACED_GROUPS: u32 = 8;
    let mut depth = 0u32;
    let mut angles = 0u32;
    let mut groups = 0u32;
    let mut i = at;
    while i > 0 {
        i -= 1;
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b')' | b']' | b'}' => depth += 1,
            b'{' if depth == 1 => {
                depth = 0;
                groups += 1;
                if groups > BRACED_GROUPS {
                    return None;
                }
            }
            b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
            b'(' | b'[' | b'{' => return None,
            b';' if depth == 0 => return None,
            b'>' if depth == 0 && t.get(i.wrapping_sub(1)) != Some(b'-') => angles += 1,
            b'<' if depth == 0 && angles > 0 => angles -= 1,
            b'<' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// The first word of the template parameter that holds the `=` at `eq`.
fn template_parameter_head(t: &Text, eq: usize) -> Option<&[u8]> {
    let mut depth = 0u32;
    let mut angles = 0u32;
    let mut i = eq;
    let start = loop {
        if i == 0 {
            return None;
        }
        i -= 1;
        if !t.is_code(i) {
            continue;
        }
        match t.at(i) {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => depth = depth.checked_sub(1)?,
            b'>' if depth == 0 => angles += 1,
            b'<' | b',' if depth == 0 && angles == 0 => break i,
            b'<' if depth == 0 => angles -= 1,
            _ => {}
        }
    };
    let first = t.next(start + 1)?;
    Some(t.word(first))
}

/// The positions of the `;` that stand at the top level of the
/// parentheses from `open` to `close`.
fn top_level_semicolons(t: &Text, open: usize, close: usize) -> Vec<usize> {
    let mut depth = 0u32;
    let mut found = Vec::new();
    for k in open + 1..close {
        if !t.is_code(k) {
            continue;
        }
        match t.at(k) {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => found.push(k),
            _ => {}
        }
    }
    found
}

/// True when the tokens from `start` to the `=` at `eq` declare a name:
/// a type and a declarator, or `auto` and a binding list. An assignment
/// such as `x = g()`, `*p = g()` or `a.b = g()` is not one.
fn declares_a_name(t: &Text, start: usize, eq: usize) -> bool {
    let mut words = 0;
    let mut angles = 0u32;
    let mut binding = false;
    let mut k = start;
    while let Some(n) = t.next(k).filter(|&n| n < eq) {
        let byte = t.at(n);
        if is_word(byte) {
            words += 1;
            k = t.word_end(n);
            continue;
        }
        match byte {
            b'[' => {
                let Some(close) = t.match_close(n, b'[', b']') else {
                    return false;
                };
                let names = (n + 1..close).all(|j| {
                    !t.is_code(j) || is_word(t.at(j)) || matches!(t.at(j), b',' | b' ' | b'\t')
                });
                if !names || words == 0 {
                    return false;
                }
                binding = true;
                k = close + 1;
                continue;
            }
            b'<' => angles += 1,
            b'>' => angles = angles.saturating_sub(1),
            b',' if angles > 0 => {}
            b'*' | b'&' => {}
            b':' if t.get(n + 1) == Some(b':') => {
                k = n + 2;
                continue;
            }
            _ => return false,
        }
        k = n + 1;
    }
    if binding {
        return true;
    }
    // A declarator is one name, and a qualified name is an assignment to
    // a member that already exists.
    words >= 2
        && t.prev(eq).is_some_and(|p| {
            is_word(t.at(p)) && {
                let start = p + 1 - t.word_before(p + 1).len();
                t.prev(start).is_none_or(|q| t.at(q) != b':')
            }
        })
}

/// The end of the token that names an operator at `at`, in an explicit
/// call such as `a.operator+(b)`.
fn operator_token_end(t: &Text, at: usize) -> Option<usize> {
    match t.at(at) {
        b'(' => t.next(at + 1).filter(|&k| t.at(k) == b')').map(|k| k + 1),
        b'[' => t.next(at + 1).filter(|&k| t.at(k) == b']').map(|k| k + 1),
        byte if is_word(byte) => {
            // `new`, `delete`, `co_await`, a conversion to a type, or the
            // suffix of a literal operator.
            let mut end = t.word_end(at);
            while let Some(k) = t.next(end).filter(|&k| t.at(k) != b'(') {
                match t.at(k) {
                    b'[' => end = t.next(k + 1).filter(|&c| t.at(c) == b']')? + 1,
                    b'*' | b'&' => end = k + 1,
                    b':' if t.starts(k, b"::") => end = k + 2,
                    byte if is_word(byte) => end = t.word_end(k),
                    _ => return None,
                }
            }
            Some(end)
        }
        byte if OPERATOR_BYTES.contains(&byte) => {
            let mut end = at;
            while end < t.len() && OPERATOR_BYTES.contains(&t.at(end)) {
                end += 1;
            }
            Some(end)
        }
        _ => None,
    }
}

/// True when the rest of the line after `at` holds no code, and the next
/// line that holds anything is a directive.
fn directive_below(t: &Text, at: usize) -> bool {
    let Some(nl) = find(&t.out, at + 1, b"\n") else {
        return false;
    };
    if (at + 1..nl).any(|k| t.is_code(k) && !t.at(k).is_ascii_whitespace()) {
        return false;
    }
    lines(&t.out[nl + 1..])
        .map(|line| &t.out[nl + 1 + line.start..nl + 1 + line.end])
        .find(|line| line.iter().any(|b| !b.is_ascii_whitespace()))
        .is_some_and(|line| directive_word(line).is_some())
}

/// True when the line before `at` holds no code before it, and the line
/// above that holds anything is a directive.
fn directive_above(t: &Text, at: usize) -> bool {
    let line_start = (0..at)
        .rev()
        .find(|&k| t.at(k) == b'\n')
        .map_or(0, |k| k + 1);
    if (line_start..at).any(|k| t.is_code(k) && !t.at(k).is_ascii_whitespace()) {
        return false;
    }
    lines(&t.out[..line_start.saturating_sub(1)])
        .map(|line| &t.out[line])
        .filter(|line| line.iter().any(|b| !b.is_ascii_whitespace()))
        .last()
        .is_some_and(|line| directive_word(line).is_some())
}

// ---------------------------------------------------------------------
// The passes
// ---------------------------------------------------------------------

/// What a pass reads.
enum Stage {
    /// The raw bytes, before the mask is built, because what the pass
    /// changes decides the mask.
    Bytes(fn(&mut [u8]) -> usize),
    /// The quoted literals, which the scan of code never visits.
    Literals(fn(&mut Text) -> usize),
}

/// A rewrite that reads the whole file at one time, and returns how many
/// times it fired.
struct Pass {
    name: &'static str,
    paper: &'static str,
    stage: Stage,
}

/// P2223. A backslash, then spaces or tabs, then a line break is a line
/// splice. The grammar knows only the form with no space, and reads the
/// next line as code where the compiler joins it to the comment above.
/// The backslash moves to the end of its line, so the two agree.
fn trailing_splices(out: &mut [u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < out.len() {
        if out[i] != b'\\' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < out.len() && matches!(out[j], b' ' | b'\t') {
            j += 1;
        }
        let breaks = out.get(j) == Some(&b'\n')
            || (out.get(j) == Some(&b'\r') && out.get(j + 1) == Some(&b'\n'));
        if j > i + 1 && breaks {
            out[i] = b' ';
            out[j - 1] = b'\\';
            count += 1;
        }
        i = j.max(i + 1);
    }
    count
}

/// A group under `#if 0` or `#if false` is not code. The compiler skips
/// it, and the text in it need not be tokens at all: an apostrophe in a
/// note, half a function, a quote that never closes. The group goes, up
/// to the `#else`, `#elif` or `#endif` that ends it, and the directive
/// lines stay, so the branch still counts as a branch.
fn skipped_groups(out: &mut [u8]) -> usize {
    let mut count = 0;
    // The first byte of the group under the `#if 0`, and how many
    // conditionals deep the scan is inside it.
    let mut skipping: Option<(usize, u32)> = None;
    let spans: Vec<Range<usize>> = lines(out).collect();
    for line in spans {
        let (directive, never) = {
            let text = &out[line.clone()];
            let Some(word) = directive_word(text) else {
                continue;
            };
            let directive = match word {
                b"if" | b"ifdef" | b"ifndef" => Directive::Opens,
                b"endif" => Directive::Closes,
                b"else" | b"elif" | b"elifdef" | b"elifndef" => Directive::Turns,
                _ => Directive::Other,
            };
            (directive, word == b"if" && condition_is_false(text))
        };
        match (skipping, directive) {
            (None, Directive::Opens) if never => {
                skipping = Some(((line.end + 1).min(out.len()), 0));
            }
            (None, _) => {}
            (Some((start, depth)), Directive::Opens) => skipping = Some((start, depth + 1)),
            (Some((start, depth)), Directive::Closes) if depth > 0 => {
                skipping = Some((start, depth - 1));
            }
            (Some((start, 0)), Directive::Closes | Directive::Turns) => {
                for byte in &mut out[start..line.start] {
                    if !matches!(*byte, b'\n' | b'\r') {
                        *byte = b' ';
                    }
                }
                count += 1;
                skipping = None;
            }
            (Some(_), _) => {}
        }
    }
    count
}

/// What a directive does to the nesting of conditional groups.
#[derive(Clone, Copy)]
enum Directive {
    /// `#if`, `#ifdef`, `#ifndef`.
    Opens,
    /// `#endif`.
    Closes,
    /// `#else` and the `#elif` family, which end one group and open the
    /// next at the same depth.
    Turns,
    Other,
}

/// True when an `#if` line tests `0` or `false`, with a comment or not.
fn condition_is_false(line: &[u8]) -> bool {
    let Some(at) = find(line, 0, b"if") else {
        return false;
    };
    let rest = &line[at + 2..];
    let rest = match (find(rest, 0, b"//"), find(rest, 0, b"/*")) {
        (Some(a), Some(b)) => &rest[..a.min(b)],
        (Some(a), None) | (None, Some(a)) => &rest[..a],
        (None, None) => rest,
    };
    matches!(rest.trim_ascii(), b"0" | b"false")
}

/// P2290. `\x{41}`, `\o{101}` and `\u{1F600}` delimit their digits, and
/// the grammar knows only the older escapes. The text of a literal is
/// not measured, so the escape becomes underscores inside the literal.
fn delimited_escapes(t: &mut Text) -> usize {
    let mut count = 0;
    for span in t.literals.clone() {
        let mut k = span.start;
        while k + 2 < span.end {
            if t.out[k] != b'\\' {
                k += 1;
                continue;
            }
            if matches!(t.out[k + 1], b'x' | b'o' | b'u')
                && t.out[k + 2] == b'{'
                && let Some(close) = (k + 3..span.end).find(|&j| t.out[j] == b'}')
            {
                t.blank(k..close + 1, b'_');
                count += 1;
                k = close + 1;
                continue;
            }
            k += 2;
        }
    }
    count
}

/// Every pass, in the order it runs.
const PASSES: &[Pass] = &[
    Pass {
        name: "line splice after whitespace",
        paper: "P2223",
        stage: Stage::Bytes(trailing_splices),
    },
    Pass {
        name: "skipped group",
        paper: "C++98",
        stage: Stage::Bytes(skipped_groups),
    },
    Pass {
        name: "delimited escape",
        paper: "P2290",
        stage: Stage::Literals(delimited_escapes),
    },
];

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
/// operator is gone, unless it is a type-id that opens with a keyword,
/// or the global namespace. Neither of those is an expression, so the
/// operator and the operand become one name.
fn reflect(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'^') {
        return None;
    }
    let start = t.next(i + 2);
    let operand_end = match start {
        // `^^::` alone names the global namespace. `^^::std::vector`
        // is a qualified name, which is an expression once `^^` goes.
        Some(s) if t.starts(s, b"::") && !t.next(s + 2).is_some_and(|k| is_word(t.at(k))) => {
            Some(s + 2)
        }
        Some(s) => keyword_type_end(t, s),
        None => None,
    };
    match operand_end {
        Some(end) => {
            t.name(i..end);
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
    t.name(i..end);
    Some(end)
}

/// An attribute. It states a property of what it is written on, and it
/// carries no work: not the arguments of `[[gnu::aligned(64)]]`, and not
/// the expression of `[[assume(x > 0)]]`, which the compiler does not
/// evaluate. So every one goes.
///
/// That is what makes the grammar read them in every position the
/// language allows: before `friend`, after an enumerator, in a binding,
/// on a lambda, in `[[using gnu: hot]]`, and as an annotation (P3394).
/// The grammar reads some of these positions and not the others, and
/// the language adds positions faster than the grammar does.
///
/// `[[:` is not an attribute. P2996 reads it as `[` and a splice.
fn attribute(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'[') || t.get(i + 2) == Some(b':') {
        return None;
    }
    let close = t.match_close(i, b'[', b']')?;
    if close <= i + 2 || t.at(close - 1) != b']' {
        return None;
    }
    if t.match_close(i + 1, b'[', b']') != Some(close - 1) {
        return None;
    }
    t.blank(i..close + 1, b' ');
    Some(close + 1)
}

/// A GNU attribute, `__attribute__((...))`. It carries no work, for the
/// reason `attribute` gives, and the grammar reads it before a
/// declaration and not after a declarator, a namespace name or `enum`.
fn gnu_attribute(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let open = t.next(t.word_end(i)).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    t.blank(i..close + 1, b' ');
    Some(close + 1)
}

/// `__extension__` tells GCC not to warn about the extension that
/// follows. It changes nothing that the code does.
fn extension_keyword(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = t.word_end(i);
    t.blank(i..end, b' ');
    Some(end)
}

/// A qualifier the grammar has no rule for, and that carries no work:
/// the nullability of a pointer, a calling convention, `_Complex`.
fn qualifier_keyword(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = t.word_end(i);
    t.blank(i..end, b' ');
    Some(end)
}

/// GNU `typeof(expr)` and `__typeof__(expr)` in the position of a type.
/// The operand is not evaluated, so the whole of it becomes one name.
/// A call to a function named `typeof` is not followed by a declarator,
/// and it stays.
fn gnu_typeof(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let open = t.next(t.word_end(i)).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    let after = t.next(close + 1)?;
    if !(is_word(t.at(after)) || matches!(t.at(after), b'*' | b'&')) {
        return None;
    }
    t.name(i..close + 1);
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

/// P2893 variadic friends, and P0195 pack expansions in a
/// using-declaration: `friend Ts...;` and `using Ts::operator()...;`.
/// The ellipsis expands the one declaration for every element, and the
/// grammar reads the declaration without it.
fn variadic_declaration(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !t.starts(i, b"...")
        || !t
            .next(i + 3)
            .is_some_and(|k| matches!(t.at(k), b';' | b','))
    {
        return None;
    }
    let mut k = i;
    let declares = loop {
        let p = t.prev(k)?;
        match t.at(p) {
            b';' | b'{' | b'}' => break false,
            byte if is_word(byte) => {
                let word = t.word_before(p + 1);
                if matches!(word, b"friend" | b"using") {
                    break true;
                }
                k = p + 1 - word.len();
            }
            _ => k = p,
        }
    };
    if !declares {
        return None;
    }
    t.blank(i..i + 3, b' ');
    Some(i + 3)
}

/// P0329 designated initializers. `.p{1, 2}` initializes a member with a
/// braced list, and the grammar reads a designator only before `=`. The
/// designator names a member and does no work, so it goes, and the list
/// stays.
fn designator_with_braces(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.starts(i, b"..") || !t.prev(i).is_some_and(|p| matches!(t.at(p), b'{' | b',')) {
        return None;
    }
    let name = t
        .next(i + 1)
        .filter(|&k| is_word(t.at(k)) && !t.at(k).is_ascii_digit())?;
    let end = t.word_end(name);
    t.next(end).filter(|&k| t.at(k) == b'{')?;
    t.blank(i..end, b' ');
    Some(end)
}

/// GNU case ranges, `case 1 ... 5:`. The branch is one branch, and the
/// upper bound goes.
fn case_range(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !t.starts(i, b"...") {
        return None;
    }
    let mut k = i;
    loop {
        let p = t.prev(k)?;
        match t.at(p) {
            b';' | b'{' | b'}' => return None,
            b':' if !(p > 0 && t.at(p - 1) == b':') && t.get(p + 1) != Some(b':') => return None,
            byte if is_word(byte) => {
                let word = t.word_before(p + 1);
                if word == b"case" {
                    break;
                }
                k = p + 1 - word.len();
            }
            _ => k = p,
        }
    }
    let colon = (i + 3..t.len()).find(|&c| {
        t.is_code(c) && t.at(c) == b':' && t.get(c + 1) != Some(b':') && t.at(c - 1) != b':'
    })?;
    t.blank(i..colon, b' ');
    Some(colon)
}

/// GNU named variadic parameters in a macro, `#define F(a, args...)`.
/// The grammar reads only `...`, and the name of the pack is not code.
fn named_variadic_parameter(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !t.starts(i, b"...") || i == 0 || !is_word(t.at(i - 1)) {
        return None;
    }
    let line_start = (0..i)
        .rev()
        .find(|&k| t.at(k) == b'\n')
        .map_or(0, |k| k + 1);
    if directive_word(&t.out[line_start..i]) != Some(b"define") {
        return None;
    }
    let name_start = i - t.word_before(i).len();
    t.blank(name_start..i, b' ');
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

/// An explicit instantiation of a class template, `template class
/// Pool<2>;`, and its `extern` form. The declaration instantiates what
/// is written elsewhere and holds no work of its own, so it goes, and
/// its `;` stays. An `extern template` of a function reads once the
/// `extern` goes.
fn explicit_instantiation(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !starts_a_statement(t, i) {
        return None;
    }
    let template = if t.word_at(i, b"extern") {
        t.next(i + b"extern".len())
            .filter(|&k| t.word_at(k, b"template"))?
    } else {
        i
    };
    let after = t.next(template + b"template".len())?;
    if !CLASS_KEYS.iter().any(|key| t.word_at(after, key)) {
        if template != i && t.at(after) != b'<' {
            let end = i + b"extern".len();
            t.blank(i..end, b' ');
            return Some(end);
        }
        return None;
    }
    let end = (after..t.len()).find(|&k| t.is_code(k) && matches!(t.at(k), b';' | b'{'))?;
    if t.at(end) == b'{' {
        return None;
    }
    t.blank(i..end, b' ');
    Some(end)
}

/// `extern "C" {` that opens inside `#ifdef __cplusplus` and closes
/// inside another. The grammar reads a conditional group as whole
/// declarations, and half a linkage block is not one. The linkage states
/// how names are mangled and does no work, so the block's two braces go,
/// and everything between them stays.
fn linkage_across_a_conditional(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let quote = t
        .next_any(i + b"extern".len())
        .filter(|&k| t.at(k) == b'"')?;
    let end = literal_end(&t.out, quote)?;
    if !matches!(&t.out[quote..end], b"\"C\"" | b"\"C++\"") {
        return None;
    }
    let open = t.next(end).filter(|&k| t.at(k) == b'{')?;
    let close = t.match_close(open, b'{', b'}')?;
    if !(directive_below(t, open) && directive_above(t, close) && directive_below(t, close)) {
        return None;
    }
    t.blank(close..close + 1, b' ');
    t.blank(i..open + 1, b' ');
    Some(open + 1)
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

/// P1102. A lambda specifier with no parameter list. `[] consteval {}`
/// is C++23, and the grammar reads the word only after a `()` that this
/// lambda does not write. A specifier states how the body may be called
/// and adds nothing to measure, and `noexcept(true)` takes its condition
/// with it.
///
/// The introducer is what identifies one. `]]` closes an attribute, and
/// `a[i]` is a subscript, and neither one is a lambda.
fn lambda_specifier(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !LAMBDA_SPECIFIERS.contains(&t.word(i)) {
        return None;
    }
    let prev = t.prev(i).filter(|&p| t.at(p) == b']')?;
    if !ends_a_lambda_introducer(t, prev) {
        return None;
    }
    let mut end = t.word_end(i);
    if t.word_at(i, b"noexcept")
        && let Some(open) = t.next(end).filter(|&k| t.at(k) == b'(')
        && let Some(close) = t.match_paren(open)
    {
        end = close + 1;
    }
    t.blank(i..end, b' ');
    Some(end)
}

/// P1169 `static` on a lambda, as in `[]() static {}`. The grammar reads
/// the other specifiers after a parameter list and not this one.
fn static_lambda(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let close = t.prev(i).filter(|&p| t.at(p) == b')')?;
    let open = open_paren_of(t, close)?;
    let before = t.prev(open)?;
    let lambda = match t.at(before) {
        b']' => ends_a_lambda_introducer(t, before),
        b'>' => angle_open(t, before)
            .and_then(|lt| t.prev(lt))
            .is_some_and(|p| t.at(p) == b']' && ends_a_lambda_introducer(t, p)),
        _ => false,
    };
    if !lambda {
        return None;
    }
    let end = t.word_end(i);
    t.blank(i..end, b' ');
    Some(end)
}

/// P1102. A trailing return type on a lambda with no parameter list,
/// as in `[] -> int {}`. The type states what the body returns and does
/// no work.
fn lambda_trailing_return(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'>') {
        return None;
    }
    let prev = t.prev(i).filter(|&p| t.at(p) == b']')?;
    if !ends_a_lambda_introducer(t, prev) {
        return None;
    }
    let body = (i + 2..t.len()).find(|&k| t.is_code(k) && matches!(t.at(k), b'{' | b';'))?;
    if t.at(body) != b'{' {
        return None;
    }
    t.blank(i..body, b' ');
    Some(body)
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

/// P3289 consteval block, `consteval { ... }`. The block is a body that
/// runs at compile time, and its complexity is real, so it becomes a
/// function named `_`, the name P2169 gives to what has no name.
fn consteval_block(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = i + b"consteval".len();
    if !starts_a_statement(t, i) || !t.next(end).is_some_and(|k| t.at(k) == b'{') {
        return None;
    }
    t.overwrite(i..end, b"void _()").then_some(end)
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

/// A braced default argument, and P2308 a braced default template
/// argument. The grammar reads `= Cfg{}` and `= 0`, and no `= {}`. What
/// follows the closing brace is what says the value is one: a default
/// ends at the next parameter, at the parameter list, or at the template
/// header.
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
    let ends = t.next(close + 1).is_some_and(|k| match t.at(k) {
        b',' | b')' => true,
        b'>' => in_a_template_header(t, i),
        _ => false,
    });
    if !ends {
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

/// P0734. A constrained template parameter whose default is a type that
/// opens with a keyword, as in `template <C T = int>`. The grammar reads
/// `C T` as a value parameter, and `int` is not a value, so the default
/// becomes a name.
fn type_default_in_template_header(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    // The type comes first because it reads a few bytes forward, and the
    // header test scans back. `enum { A = 1, B = 2 }` is one scan for
    // each `=` without it.
    let start = t.next(i + 1)?;
    let end = keyword_type_end(t, start)?;
    if !in_a_template_header(t, i)
        || template_parameter_head(t, i)
            .is_none_or(|head| matches!(head, b"class" | b"typename" | b"template"))
    {
        return None;
    }
    t.name(start..end);
    Some(end)
}

/// P0732. A braced list as a template argument, as in `S<{1, 2}>`. An
/// empty pair goes, and a pair with something in it becomes parentheses,
/// for the reason `braced_default_argument` gives.
fn braced_template_argument(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    t.prev(i).filter(|&p| matches!(t.at(p), b'<' | b','))?;
    let close = t.match_close(i, b'{', b'}')?;
    if !t
        .next(close + 1)
        .is_some_and(|k| matches!(t.at(k), b'>' | b','))
        || !in_template_arguments(t, i)
    {
        return None;
    }
    if t.next(i + 1) == Some(close) {
        t.blank(i..close + 1, b' ');
        return Some(close + 1);
    }
    t.out[i] = b'(';
    t.out[close] = b')';
    Some(close + 1)
}

/// P2841. A concept or a variable template as a template parameter:
/// `template <template <class> concept C>` and `template <template
/// <class> auto V>`. The grammar reads the first as a class template
/// parameter once `concept` becomes `class`, and the second as a value
/// parameter once its header goes.
fn template_template_parameter(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let close = t.prev(i).filter(|&p| t.at(p) == b'>')?;
    let open = angle_open(t, close)?;
    let template_end = t
        .prev(open)
        .filter(|&p| t.word_before(p + 1) == b"template")?
        + 1;
    let template = template_end - b"template".len();
    if !t
        .prev(template)
        .is_some_and(|p| matches!(t.at(p), b'<' | b','))
    {
        return None;
    }
    let end = t.word_end(i);
    if t.word_at(i, b"concept") {
        return t.overwrite(i..end, b"class").then_some(end);
    }
    t.blank(template..close + 1, b' ');
    Some(end)
}

/// A pointer to member as an abstract declarator, as in `W<void
/// (S::*)()>` or `using F = int (ns::S::*)(int)`. The grammar reads one
/// only with a name after the `*`. The class qualifier goes, and what
/// remains is a pointer to a function, which has the same shape.
fn member_pointer_declarator(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let start = t.next(i + 1)?;
    let mut k = start;
    if t.starts(k, b"::") {
        k = t.next(k + 2)?;
    }
    let star = loop {
        if !is_word(t.at(k)) {
            return None;
        }
        let mut after = t.next(t.word_end(k))?;
        if t.at(after) == b'<' {
            let mut depth = 0u32;
            let close = (after..t.len()).find(|&j| {
                if !t.is_code(j) {
                    return false;
                }
                match t.at(j) {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        return depth == 0;
                    }
                    _ => {}
                }
                false
            })?;
            after = t.next(close + 1)?;
        }
        if !t.starts(after, b"::") {
            return None;
        }
        let next = t.next(after + 2)?;
        if t.at(next) == b'*' {
            break next;
        }
        k = next;
    };
    let after_star = t.next(star + 1)?;
    let abstract_declarator = matches!(t.at(after_star), b')' | b'[')
        || t.word_at(after_star, b"const")
        || t.word_at(after_star, b"volatile");
    if !abstract_declarator {
        return None;
    }
    t.blank(start..star, b' ');
    Some(star)
}

/// `p->*pm`, a member access through a pointer to member. The grammar
/// reads `.*` and not `->*`, and `->` with the name after it has the
/// same shape: an access to a member of the object `p` points to.
fn member_pointer_access(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !t.starts(i, b"->*")
        || t.prev(i)
            .is_some_and(|p| t.word_before(p + 1) == b"operator")
    {
        return None;
    }
    t.blank(i + 2..i + 3, b' ');
    Some(i + 3)
}

/// An explicit call of an operator through a member access, as in
/// `a.operator+(b)` or `p->operator()(x)`. The call stays a call, and
/// the operator's name becomes a name.
fn explicit_operator_call(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let access = t.prev(i)?;
    let member =
        t.at(access) == b'.' || (t.at(access) == b'>' && access > 0 && t.at(access - 1) == b'-');
    if !member {
        return None;
    }
    let token = t.next(i + b"operator".len())?;
    let end = operator_token_end(t, token)?;
    t.next(end).filter(|&k| t.at(k) == b'(')?;
    t.name(i..end);
    Some(end)
}

/// A qualified destructor call, as in `p->T::~T()`. The grammar reads
/// `p->~T()`, and the qualifier names the class the destructor belongs
/// to and does no work.
fn qualified_destructor_call(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let colons = t
        .prev(i)
        .filter(|&p| t.at(p) == b':' && p > 0 && t.at(p - 1) == b':')?
        - 1;
    let mut k = colons;
    let start = loop {
        let w = t.prev(k).filter(|&w| is_word(t.at(w)))?;
        let word_start = w + 1 - t.word_before(w + 1).len();
        let before = t.prev(word_start)?;
        if t.at(before) == b':' && before > 0 && t.at(before - 1) == b':' {
            k = before - 1;
            continue;
        }
        if t.at(before) == b'.' || (t.at(before) == b'>' && before > 0 && t.at(before - 1) == b'-')
        {
            break word_start;
        }
        return None;
    };
    t.blank(start..i, b' ');
    Some(i)
}

/// `typeid(T)`. The grammar reads it only as a call, and a type is not
/// an argument. `sizeof` has the same length and the same shape, and it
/// takes a type, and neither one evaluates a type operand.
fn typeid_operand(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let end = i + b"typeid".len();
    t.next(end).filter(|&k| t.at(k) == b'(')?;
    t.overwrite(i..end, b"sizeof").then_some(end)
}

/// P2741 a static assertion with a message that is not a string
/// literal. The grammar reads only a literal there. The message is text
/// for a failed build and does no work, so it goes.
fn static_assert_message(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let open = t
        .next(i + b"static_assert".len())
        .filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    let mut depth = 0u32;
    let mut angles = 0u32;
    let mut comma = None;
    for k in open + 1..close {
        if !t.is_code(k) {
            continue;
        }
        let byte = t.at(k);
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'<' if depth == 0
                && t.prev(k)
                    .is_some_and(|p| is_word(t.at(p)) || t.at(p) == b'>')
                && !matches!(t.get(k + 1), Some(b'<' | b'=')) =>
            {
                angles += 1;
            }
            b'>' if depth == 0 && angles > 0 && t.at(k - 1) != b'-' => angles -= 1,
            b',' if depth == 0 && angles == 0 => comma = Some(k),
            _ => {}
        }
    }
    let comma = comma?;
    let message = t.next_any(comma + 1)?;
    let prefix = t.word(message);
    let literal = t.at(message) == b'"'
        || (matches!(
            prefix,
            b"u8" | b"u" | b"U" | b"L" | b"R" | b"u8R" | b"uR" | b"UR" | b"LR"
        ) && t.get(message + prefix.len()) == Some(b'"'));
    if literal {
        return None;
    }
    t.blank(comma..close, b' ');
    Some(close)
}

/// P2324 a label at the end of a block, as in `{ goto done; done: }`.
/// The grammar reads a label only before a statement. A label names a
/// place and does no work, so it goes.
fn label_at_block_end(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) == Some(b':') || (i > 0 && t.at(i - 1) == b':') {
        return None;
    }
    t.next(i + 1).filter(|&k| t.at(k) == b'}')?;
    let name_end = t.prev(i).filter(|&p| is_word(t.at(p)))?;
    let name = t.word_before(name_end + 1);
    if matches!(name, b"default" | b"public" | b"private" | b"protected") {
        return None;
    }
    let start = name_end + 1 - name.len();
    if !t
        .prev(start)
        .is_none_or(|p| matches!(t.at(p), b';' | b'{' | b'}' | b':'))
    {
        return None;
    }
    t.blank(start..i + 1, b' ');
    Some(i + 1)
}

/// P0683 a default member initializer on a bit-field, as in `int x : 3
/// = 1;`. The width is a constant that states the layout and does no
/// work. It goes, and the initializer stays.
fn bitfield_initializer(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) == Some(b':') || (i > 0 && t.at(i - 1) == b':') {
        return None;
    }
    let name_end = t.prev(i).filter(|&p| is_word(t.at(p)))?;
    let name = t.word_before(name_end + 1);
    if matches!(
        name,
        b"public" | b"private" | b"protected" | b"default" | b"final"
    ) {
        return None;
    }
    let before = t.prev(name_end + 1 - name.len())?;
    let typed = if is_word(t.at(before)) {
        !matches!(
            t.word_before(before + 1),
            b"struct" | b"class" | b"union" | b"enum" | b"case" | b"goto" | b"return" | b"virtual"
        )
    } else {
        matches!(t.at(before), b'*' | b'&' | b'>')
    };
    if !typed {
        return None;
    }
    // A `?` or an `=` earlier in the statement makes the `:` part of an
    // expression, as in `int x = c ? *p : q`, and not the start of a
    // width.
    let mut k = name_end + 1 - name.len();
    while let Some(p) = t.prev(k) {
        match t.at(p) {
            b';' | b'{' | b'}' => break,
            b'?' => return None,
            b'=' if t.get(p + 1) != Some(b'=')
                && !matches!(t.get(p.wrapping_sub(1)), Some(b'<' | b'>' | b'!' | b'=')) =>
            {
                return None;
            }
            _ => {}
        }
        k = p;
    }
    let mut depth = 0u32;
    let mut stop = None;
    for k in i + 1..t.len() {
        if !t.is_code(k) {
            continue;
        }
        match t.at(k) {
            b'(' => depth += 1,
            b')' if depth == 0 => return None,
            b')' => depth -= 1,
            b'=' if depth == 0
                && t.get(k + 1) != Some(b'=')
                && !matches!(t.at(k - 1), b'<' | b'>' | b'!' | b'=') =>
            {
                stop = Some(k);
                break;
            }
            b'{' if depth == 0 => {
                stop = Some(k);
                break;
            }
            b';' | b',' | b'}' | b'?' if depth == 0 => return None,
            _ => {}
        }
    }
    let stop = stop?;
    if !in_a_class_body(t, i) {
        return None;
    }
    t.blank(i..stop, b' ');
    Some(stop)
}

/// P2360 an alias declaration as the init-statement of a `for`, as in
/// `for (using T = int; n > 0; --n)`. The grammar reads the range form
/// and not the other one. The alias names a type and does no work, so it
/// goes, and its `;` stays.
fn alias_in_for_init(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let open = t.next(i + b"for".len()).filter(|&k| t.at(k) == b'(')?;
    let using = t.next(open + 1).filter(|&k| t.word_at(k, b"using"))?;
    let close = t.match_paren(open)?;
    let semicolons = top_level_semicolons(t, open, close);
    if semicolons.len() < 2 {
        return None;
    }
    t.blank(using..semicolons[0], b' ');
    Some(semicolons[0])
}

/// A declaration as the condition of a `for`, as in `for (; T x = g();)`
/// or `for (; auto [ok, v] = g();)`. The grammar reads a declaration as
/// the condition of an `if` or a `while` and not of a `for`. The type
/// and the name go, and the initializer, which does the work, stays.
fn declaration_as_for_condition(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let open = t.next(i + b"for".len()).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    let semicolons = top_level_semicolons(t, open, close);
    let (first, second) = (*semicolons.first()?, *semicolons.get(1)?);
    let start = t.next(first + 1).filter(|&k| k < second)?;
    let eq = (start..second).find(|&k| {
        t.is_code(k)
            && t.at(k) == b'='
            && t.get(k + 1) != Some(b'=')
            && !matches!(
                t.at(k - 1),
                b'<' | b'>' | b'!' | b'=' | b'+' | b'-' | b'*' | b'/'
            )
    })?;
    if !declares_a_name(t, start, eq) {
        return None;
    }
    t.blank(start..eq + 1, b' ');
    Some(eq + 1)
}

/// P3822 a condition on `noexcept` in a compound requirement, as in
/// `{ t.f() } noexcept(true) -> C;`. The grammar reads the bare word.
fn noexcept_condition_in_a_requirement(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    t.prev(i).filter(|&p| t.at(p) == b'}')?;
    let open = t.next(i + b"noexcept".len()).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    t.blank(open..close + 1, b' ');
    Some(close + 1)
}

/// C++98 digraphs, `<%` `%>` `<:` `:>` `%:`, which are the braces, the
/// brackets and the `#` under other spellings. `<::` is `<` and `::`
/// unless a `:` or a `>` follows it, as the lexer reads it.
fn digraph(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let replacement: &[u8] = match (t.at(i), t.get(i + 1)?) {
        (b'<', b'%') => b"{",
        (b'%', b'>') => b"}",
        (b'<', b':') => {
            if t.get(i + 2) == Some(b':') && !matches!(t.get(i + 3), Some(b':' | b'>')) {
                return None;
            }
            b"["
        }
        (b':', b'>') => b"]",
        (b'%', b':') if t.starts(i, b"%:%:") => {
            return t.overwrite(i..i + 4, b"##").then_some(i + 4);
        }
        (b'%', b':') => b"#",
        _ => return None,
    };
    t.overwrite(i..i + 2, replacement).then_some(i + 2)
}

/// P0061 `__has_include(<x>)`, and P1967 `__has_embed("x")`. The grammar
/// reads the operator and not a header name as its operand. The operand
/// becomes a name.
fn has_include(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if !on_a_directive(t, i) {
        return None;
    }
    let open = t.next(t.word_end(i)).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    if close > open + 1 {
        t.blank(open + 1..close, b'_');
    }
    Some(close + 1)
}

/// P1967 `#embed`, which the grammar has no directive for. The directive
/// expands to a list of numbers, and `0` stands where that list stands.
fn embed_directive(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let line_start = (0..i)
        .rev()
        .find(|&k| t.at(k) == b'\n')
        .map_or(0, |k| k + 1);
    if (line_start..i).any(|k| !t.at(k).is_ascii_whitespace()) {
        return None;
    }
    t.next(i + 1).filter(|&k| t.word_at(k, b"embed"))?;
    let end = line_comment_end(&t.out, i);
    t.overwrite(i..end, b"0").then_some(end)
}

/// P2786 the class properties `trivially_relocatable_if_eligible` and
/// `replaceable_if_eligible`. The grammar reads the first one as the
/// name of the class, and the class then reports under it. A property
/// states what the compiler may do with the type and does no work.
fn class_property(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let prev = t.prev(i)?;
    if !(is_word(t.at(prev)) || t.at(prev) == b'>') {
        return None;
    }
    let end = t.word_end(i);
    let next = t.next(end)?;
    let next_ok = matches!(t.at(next), b'{' | b':')
        || [
            &b"final"[..],
            b"trivially_relocatable_if_eligible",
            b"replaceable_if_eligible",
        ]
        .iter()
        .any(|word| t.word_at(next, word));
    if !next_ok || !in_a_class_head(t, i) {
        return None;
    }
    t.blank(i..end, b' ');
    Some(end)
}

/// GNU labels as values: `&&label` and `goto *p`. The address of a label
/// reads as the address of a name, and the computed jump as a jump to a
/// name.
fn label_address(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'&') || (i > 0 && t.at(i - 1) == b'&') {
        return None;
    }
    t.next(i + 2)
        .filter(|&k| is_word(t.at(k)) && !t.at(k).is_ascii_digit())?;
    let unary = t.prev(i).is_some_and(|p| {
        if is_word(t.at(p)) {
            EXPRESSIONS.contains(&t.word_before(p + 1))
        } else {
            matches!(t.at(p), b'=' | b'(' | b',' | b'{' | b'?' | b':' | b'[')
        }
    });
    if !unary {
        return None;
    }
    t.blank(i..i + 1, b' ');
    Some(i + 2)
}

/// The GNU computed jump, `goto *p`.
fn computed_goto(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let star = t.next(i + b"goto".len()).filter(|&k| t.at(k) == b'*')?;
    t.blank(star..star + 1, b' ');
    Some(star + 1)
}

/// A typename before a qualified name. The keyword disambiguates for the
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
        // It expands to an attribute, which is not complexity, and its
        // arguments are part of that attribute: `GUARDED_BY(mu)` leaves
        // nothing behind, and a bare `(mu)` would not parse.
        Kind::Attribute | Kind::Statement => {
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

macro_rules! rule {
    ($name:literal, $paper:literal, $trigger:expr, $apply:expr) => {
        Rule {
            name: $name,
            paper: $paper,
            trigger: $trigger,
            apply: $apply,
        }
    };
}

/// Every rule, in the order the driver tries them at one position.
const RULES: &[Rule] = &[
    rule!("reflect", "P2996", Trigger::Byte(b'^'), reflect),
    rule!("splice", "P2996", Trigger::Byte(b'['), splice),
    rule!("attribute", "N2761", Trigger::Byte(b'['), attribute),
    rule!("pack index", "P2662", Trigger::Byte(b'.'), pack_index),
    rule!("binding pack", "P1061", Trigger::Byte(b'.'), binding_pack),
    rule!(
        "variadic friend or using-declaration",
        "P2893",
        Trigger::Byte(b'.'),
        variadic_declaration
    ),
    rule!(
        "designator with braces",
        "P0329",
        Trigger::Byte(b'.'),
        designator_with_braces
    ),
    rule!("case range", "GNU", Trigger::Byte(b'.'), case_range),
    rule!(
        "named variadic macro parameter",
        "GNU",
        Trigger::Byte(b'.'),
        named_variadic_parameter
    ),
    rule!(
        "member pointer access",
        "C++98",
        Trigger::Byte(b'-'),
        member_pointer_access
    ),
    rule!(
        "lambda trailing return type",
        "P1102",
        Trigger::Byte(b'-'),
        lambda_trailing_return
    ),
    rule!(
        "member pointer declarator",
        "C++98",
        Trigger::Byte(b'('),
        member_pointer_declarator
    ),
    rule!(
        "braced default argument",
        "N2672",
        Trigger::Byte(b'='),
        braced_default_argument
    ),
    rule!(
        "type default in a template header",
        "P0734",
        Trigger::Byte(b'='),
        type_default_in_template_header
    ),
    rule!(
        "braced template argument",
        "P0732",
        Trigger::Byte(b'{'),
        braced_template_argument
    ),
    rule!("digraph", "C++98", Trigger::Byte(b'<'), digraph),
    rule!("digraph", "C++98", Trigger::Byte(b'%'), digraph),
    rule!("digraph", "C++98", Trigger::Byte(b':'), digraph),
    rule!(
        "bit-field initializer",
        "P0683",
        Trigger::Byte(b':'),
        bitfield_initializer
    ),
    rule!(
        "label at the end of a block",
        "P2324",
        Trigger::Byte(b':'),
        label_at_block_end
    ),
    rule!(
        "qualified destructor call",
        "C++98",
        Trigger::Byte(b'~'),
        qualified_destructor_call
    ),
    rule!("embed", "P1967", Trigger::Byte(b'#'), embed_directive),
    rule!("label address", "GNU", Trigger::Byte(b'&'), label_address),
    rule!(
        "expansion statement",
        "P1306",
        Trigger::Words(&[b"template"]),
        expansion_statement
    ),
    rule!(
        "explicit instantiation",
        "C++98",
        Trigger::Words(&[b"template", b"extern"]),
        explicit_instantiation
    ),
    rule!(
        "linkage block across a conditional",
        "C++98",
        Trigger::Words(&[b"extern"]),
        linkage_across_a_conditional
    ),
    rule!(
        "delete with a reason",
        "P2573",
        Trigger::Words(&[b"delete"]),
        delete_reason
    ),
    rule!(
        "contract clause",
        "P2900",
        Trigger::Words(&[b"pre", b"post"]),
        contract
    ),
    rule!(
        "explicit object parameter",
        "P0847",
        Trigger::Words(&[b"this"]),
        explicit_object_parameter
    ),
    rule!(
        "lambda specifier",
        "P1102",
        Trigger::Words(&[
            b"consteval",
            b"constexpr",
            b"static",
            b"mutable",
            b"noexcept"
        ]),
        lambda_specifier
    ),
    rule!(
        "static lambda",
        "P1169",
        Trigger::Words(&[b"static"]),
        static_lambda
    ),
    rule!(
        "noexcept condition in a requirement",
        "P3822",
        Trigger::Words(&[b"noexcept"]),
        noexcept_condition_in_a_requirement
    ),
    rule!(
        "if consteval",
        "P1938",
        Trigger::Words(&[b"consteval"]),
        if_consteval
    ),
    rule!(
        "consteval block",
        "P3289",
        Trigger::Words(&[b"consteval"]),
        consteval_block
    ),
    rule!(
        "module declaration",
        "P1103",
        Trigger::Words(&[b"export", b"module", b"import"]),
        module
    ),
    rule!(
        "typename before a qualified name",
        "C++98",
        Trigger::Words(&[b"typename"]),
        typename_qualified
    ),
    rule!(
        "typeid of a type",
        "C++98",
        Trigger::Words(&[b"typeid"]),
        typeid_operand
    ),
    rule!(
        "static assertion message",
        "P2741",
        Trigger::Words(&[b"static_assert"]),
        static_assert_message
    ),
    rule!(
        "alias in the init-statement of a for",
        "P2360",
        Trigger::Words(&[b"for"]),
        alias_in_for_init
    ),
    rule!(
        "declaration as the condition of a for",
        "C++98",
        Trigger::Words(&[b"for"]),
        declaration_as_for_condition
    ),
    rule!(
        "concept or variable template parameter",
        "P2841",
        Trigger::Words(&[b"concept", b"auto"]),
        template_template_parameter
    ),
    rule!(
        "class property",
        "P2786",
        Trigger::Words(&[
            b"trivially_relocatable_if_eligible",
            b"replaceable_if_eligible"
        ]),
        class_property
    ),
    rule!(
        "explicit operator call",
        "C++98",
        Trigger::Words(&[b"operator"]),
        explicit_operator_call
    ),
    rule!(
        "has include",
        "P0061",
        Trigger::Words(&[b"__has_include", b"__has_include_next", b"__has_embed"]),
        has_include
    ),
    rule!(
        "GNU attribute",
        "GNU",
        Trigger::Words(&[b"__attribute__", b"__attribute"]),
        gnu_attribute
    ),
    rule!(
        "extension keyword",
        "GNU",
        Trigger::Words(&[b"__extension__"]),
        extension_keyword
    ),
    rule!(
        "qualifier keyword",
        "Clang",
        Trigger::Words(&[
            b"_Nullable",
            b"_Nonnull",
            b"_Null_unspecified",
            b"_Nullable_result",
            b"__cdecl",
            b"__stdcall",
            b"__fastcall",
            b"__vectorcall",
            b"__thiscall",
            b"__clrcall",
            b"_Complex",
            b"__complex__",
        ]),
        qualifier_keyword
    ),
    rule!(
        "typeof",
        "GNU",
        Trigger::Words(&[b"typeof", b"__typeof__", b"__typeof"]),
        gnu_typeof
    ),
    rule!(
        "computed goto",
        "GNU",
        Trigger::Words(&[b"goto"]),
        computed_goto
    ),
    rule!(
        "macro that expands to a string",
        "C++98",
        Trigger::AnyWord,
        string_macro
    ),
    rule!(
        "macro the project declared",
        "clang-format",
        Trigger::AnyWord,
        declared_macro
    ),
];

// ---------------------------------------------------------------------
// The rules for a Clang
// ---------------------------------------------------------------------

/// P2900 `contract_assert(expr)`. A Clang without contracts reads it as
/// a call to a function it has not seen. The assertion goes and its `;`
/// stays, which is the program with the check off.
fn contract_assert_statement(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    let open = t
        .next(i + b"contract_assert".len())
        .filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    t.blank(i..close + 1, b' ');
    Some(close + 1)
}

/// P3394 annotations in an attribute list, as in `[[nodiscard, =tag]]`.
/// A Clang without annotations rejects the whole attribute. Each
/// annotation goes with one comma, and every other attribute in the
/// list stays, because the others change what the compiler checks.
fn annotation_items(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    if t.get(i + 1) != Some(b'[') || t.get(i + 2) == Some(b':') {
        return None;
    }
    let close = t.match_close(i, b'[', b']')?;
    if close <= i + 2
        || t.at(close - 1) != b']'
        || t.match_close(i + 1, b'[', b']') != Some(close - 1)
    {
        return None;
    }
    // The items of the list, divided at the commas at its top level.
    let (start, end) = (i + 2, close - 1);
    let mut items = Vec::new();
    let mut depth = 0u32;
    let mut from = start;
    for k in start..end {
        if !t.is_code(k) {
            continue;
        }
        match t.at(k) {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                items.push(from..k);
                from = k + 1;
            }
            _ => {}
        }
    }
    items.push(from..end);
    let mut removed = false;
    for (n, item) in items.iter().enumerate() {
        if !t
            .next(item.start)
            .is_some_and(|k| k < item.end && t.at(k) == b'=')
        {
            continue;
        }
        // The comma before the item goes with it, or the comma after it
        // when the item is the first.
        let span = if n > 0 {
            item.start - 1..item.end
        } else if items.len() > 1 {
            item.start..item.end + 1
        } else {
            item.clone()
        };
        t.blank(span, b' ');
        removed = true;
    }
    removed.then_some(close + 1)
}

/// P2573 `= delete("reason")` for a Clang before 19. The reason goes,
/// and `= delete` stays, which is the declaration Clang 18 reads.
fn delete_reason_for_clang(t: &mut Text, i: usize, _cx: &Cx) -> Option<usize> {
    t.prev(i).filter(|&p| t.at(p) == b'=')?;
    let open = t.next(i + b"delete".len()).filter(|&k| t.at(k) == b'(')?;
    let close = t.match_paren(open)?;
    t.blank(open..close + 1, b' ');
    Some(close + 1)
}

/// The rules for a Clang from version 19. A rule is here only when a
/// released Clang does not read the construct, and only when a program
/// without the construct does the same work. A contract is a check, an
/// annotation is data for reflection, and a class property states what
/// the compiler may do with a type.
///
/// The other grammar rules change what Clang checks, and Clang reads
/// those constructs anyway: an attribute, a digraph, `typeid`. So none
/// of them is here.
const CLANG_RULES: &[Rule] = &[
    rule!("annotation", "P3394", Trigger::Byte(b'['), annotation_items),
    rule!(
        "contract clause",
        "P2900",
        Trigger::Words(&[b"pre", b"post"]),
        contract
    ),
    rule!(
        "contract assertion",
        "P2900",
        Trigger::Words(&[b"contract_assert"]),
        contract_assert_statement
    ),
    rule!(
        "class property",
        "P2786",
        Trigger::Words(&[
            b"trivially_relocatable_if_eligible",
            b"replaceable_if_eligible"
        ]),
        class_property
    ),
];

/// The rules for a Clang before 19, which also does not read a reason
/// on `= delete`.
const CLANG_18_RULES: &[Rule] = &[
    rule!("annotation", "P3394", Trigger::Byte(b'['), annotation_items),
    rule!(
        "contract clause",
        "P2900",
        Trigger::Words(&[b"pre", b"post"]),
        contract
    ),
    rule!(
        "contract assertion",
        "P2900",
        Trigger::Words(&[b"contract_assert"]),
        contract_assert_statement
    ),
    rule!(
        "class property",
        "P2786",
        Trigger::Words(&[
            b"trivially_relocatable_if_eligible",
            b"replaceable_if_eligible"
        ]),
        class_property
    ),
    rule!(
        "delete with a reason",
        "P2573",
        Trigger::Words(&[b"delete"]),
        delete_reason_for_clang
    ),
];

/// What the rewrite is for.
#[derive(Clone, Copy)]
pub enum Target<'a> {
    /// The bundled tree-sitter grammar, with the macros the project
    /// declared to clang-format.
    Grammar(&'a Macros),
    /// A Clang of this major version, which clang-tidy runs.
    Clang(u32),
}

/// The byte table for one list of rules: for each byte, the rules that
/// can fire where it stands, in the order of the list.
fn by_byte(rules: &'static [Rule]) -> Vec<Vec<&'static Rule>> {
    let mut table: Vec<Vec<&'static Rule>> = vec![Vec::new(); 256];
    for rule in rules {
        match rule.trigger {
            Trigger::Byte(byte) => table[usize::from(byte)].push(rule),
            Trigger::Words(words) => {
                for word in words {
                    let slot = &mut table[usize::from(word[0])];
                    // Two words with one first byte name the rule one
                    // time, so it runs one time.
                    if !slot.iter().any(|seen| std::ptr::eq(*seen, rule)) {
                        slot.push(rule);
                    }
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
}

/// The rules that can fire at a byte for a target.
fn dispatch(target: Target, byte: u8) -> &'static [&'static Rule] {
    static GRAMMAR: OnceLock<Vec<Vec<&'static Rule>>> = OnceLock::new();
    static CLANG: OnceLock<Vec<Vec<&'static Rule>>> = OnceLock::new();
    static CLANG_18: OnceLock<Vec<Vec<&'static Rule>>> = OnceLock::new();
    let table = match target {
        Target::Grammar(_) => GRAMMAR.get_or_init(|| by_byte(RULES)),
        Target::Clang(major) if major < 19 => CLANG_18.get_or_init(|| by_byte(CLANG_18_RULES)),
        Target::Clang(_) => CLANG.get_or_init(|| by_byte(CLANG_RULES)),
    };
    &table[usize::from(byte)]
}

impl Text {
    /// The text as a Clang reads it. The body of a `#define` is code to
    /// a Clang, because Clang expands it: `#define ASSERT(c)
    /// contract_assert(c)` puts an assertion at each use, and only the
    /// body can take it out. So each such body is read again as code,
    /// with its own comments and literals.
    fn for_clang(out: Vec<u8>) -> Text {
        let (mut code, mut literals) = lex(&out);
        for line in lines(&out) {
            let Some(hash) = (line.start..line.end).find(|&k| !matches!(out[k], b' ' | b'\t'))
            else {
                continue;
            };
            if out[hash] != b'#' || !code[hash] || directive_is_prose(&out, hash) {
                continue;
            }
            let Some(body) = directive_text(&out, hash) else {
                continue;
            };
            let (inner, inner_literals) = lex(&out[body.clone()]);
            code[body.clone()].copy_from_slice(&inner);
            literals.extend(
                inner_literals
                    .into_iter()
                    .map(|span| span.start + body.start..span.end + body.start),
            );
        }
        Text {
            out,
            code,
            literals,
        }
    }
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
    rewrite_for(src, Target::Grammar(macros))
}

/// The C++ text with what this Clang does not read removed, for
/// clang-tidy. `None` when the source needs none of it.
pub fn lower_for_clang(src: &str, major: u32) -> Option<Rewritten> {
    rewrite_for(src, Target::Clang(major))
}

/// The constructs in the source that this Clang does not read and that
/// no removal keeps the meaning of, one name for each: reflection, a
/// consteval block, and an expansion statement before Clang 23.
pub fn clang_gaps(src: &str, major: u32) -> Vec<&'static str> {
    let Some(done) = rewrite(src, &Macros::default()) else {
        return Vec::new();
    };
    let mut gaps = Vec::new();
    for &(name, _) in &done.rules {
        let gap = match name {
            "reflect" | "splice" => "reflection",
            "consteval block" => "consteval block",
            "expansion statement" if major < 23 => "expansion statement",
            _ => continue,
        };
        if !gaps.contains(&gap) {
            gaps.push(gap);
        }
    }
    gaps
}

/// Rewrite for a target. `None` when the source needs none of it.
fn rewrite_for(src: &str, target: Target) -> Option<Rewritten> {
    let mut fired: Vec<(&'static str, &'static str)> = Vec::new();
    let mut bytes = src.as_bytes().to_vec();
    let grammar = matches!(target, Target::Grammar(_));
    // A Clang reads a splice after whitespace, a skipped group and a
    // delimited escape as the standard writes them, so the passes are
    // for the grammar only.
    if grammar {
        for pass in PASSES {
            if let Stage::Bytes(apply) = pass.stage {
                fired.extend(std::iter::repeat_n(
                    (pass.name, pass.paper),
                    apply(&mut bytes),
                ));
            }
        }
    }
    let mut text = if grammar {
        Text::new(bytes)
    } else {
        Text::for_clang(bytes)
    };
    if grammar {
        for pass in PASSES {
            if let Stage::Literals(apply) = pass.stage {
                fired.extend(std::iter::repeat_n(
                    (pass.name, pass.paper),
                    apply(&mut text),
                ));
            }
        }
    }
    let none = Macros::default();
    let cx = Cx {
        macros: match target {
            Target::Grammar(macros) => macros,
            Target::Clang(_) => &none,
        },
    };
    let mut i = 0;
    while i < text.len() {
        if !text.is_code(i) {
            i += 1;
            continue;
        }
        let mut next = i + 1;
        for rule in dispatch(target, text.at(i)) {
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

/// Every pass and every rule, by name and paper, for the test that holds
/// each one to a case that reaches it.
#[cfg(test)]
pub fn rules() -> Vec<(&'static str, &'static str)> {
    PASSES
        .iter()
        .map(|pass| (pass.name, pass.paper))
        .chain(RULES.iter().map(|rule| (rule.name, rule.paper)))
        .collect()
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

    /// Ordinary C++ that happens to spell one of the words or the bytes a
    /// rewrite looks for. None of it may change. A rewrite here would
    /// delete real work, and the complexity of that work would go
    /// unreported without any sign that it had.
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
        ("a for with no condition", "void f() { for (;;) { g(); } }"),
        (
            "a for whose condition assigns",
            "void f(int x) { for (; x = g();) { h(x); } }",
        ),
        (
            "a for whose condition compares",
            "void f(int n) { for (int i = 0; i < n; ++i) { g(i); } }",
        ),
        (
            "a range for over a braced list",
            "void f() { for (int x : {1, 2}) { g(x); } }",
        ),
        (
            "a range for with a binding",
            "void f(M& m) { for (auto& [k, v] : m) { g(k, v); } }",
        ),
        (
            "a member access after a subscript",
            "int f(A* a[]) { return a[0]->b; }",
        ),
        (
            "a ternary in a member initializer",
            "struct S { int* p; int x = c ? *p : Cfg{}.v; };",
        ),
        (
            "a case label",
            "void f(int x) { switch (x) { case 1: { g(); break; } default: break; } }",
        ),
        (
            "an enumeration with a base",
            "enum class E : unsigned char { A, B };",
        ),
        (
            "a class with a base",
            "struct D : public B { int f() { return 1; } };",
        ),
        (
            "a final class with a base",
            "class S final : public B { public: int f() { return 1; } };",
        ),
        (
            "a bit-field with no initializer",
            "struct S { unsigned x : 3; unsigned : 0; };",
        ),
        (
            "a static assertion with a message",
            "static_assert(sizeof(int) == 4, \"four\");",
        ),
        (
            "a static assertion on a template",
            "static_assert(std::is_same_v<A, B>);",
        ),
        (
            "a defaulted template parameter",
            "template <class T = int> void f(T x) { }",
        ),
        (
            "a concept definition",
            "template <class T> concept C = sizeof(T) > 1;",
        ),
        (
            "an explicit specialization",
            "template <> struct S<int> { };",
        ),
        ("a linkage block", "extern \"C\" {\nint f(int);\n}\n"),
        ("a live preprocessor group", "#if 1\nint x;\n#endif\n"),
        (
            "a comparison in a preprocessor condition",
            "#if X > 1\nint x;\n#endif\n",
        ),
        (
            "an include",
            "#include <vector>\n#include \"x.h\"\nint x;\n",
        ),
        ("a qualified member call", "void f(D* p) { p->Base::g(); }"),
        ("a braced argument", "void f() { g(a, {1, 2}); }"),
        ("a fraction after a comma", "void f() { g(a, .5); }"),
        (
            "a nested initializer list",
            "int a[2][2] = {{1, 2}, {3, 4}};",
        ),
        (
            "a designated initializer with equals",
            "P p{.x = 1, .y = 2};",
        ),
        ("a constructor initializer list", "S::S() : a{1}, b{2} { }"),
        ("a declared type in a template argument", "S<P{1, 2}> t;"),
        ("a logical and", "bool f(bool a, bool b) { return a && b; }"),
        (
            "a forwarding reference",
            "template <class T> void f(T&& x) { g(static_cast<T&&>(x)); }",
        ),
        ("a goto", "void f() { goto done; done: return; }"),
        (
            "an operator declaration",
            "struct S { S operator+(const S& o) const; bool operator==(const S&) const = default; };",
        ),
        ("an address of an operator", "auto p = &S::operator+;"),
        (
            "a new array",
            "void f() { int* q = new int[3]; delete[] q; }",
        ),
        ("a bitwise not", "int f(int y) { return ~y; }"),
        ("a destructor defined out of line", "S::~S() { }"),
        ("a label before a statement", "void f() { again: g(); }"),
        (
            "an access section at the end of a class",
            "class S {\npublic:\n  int f() { return 1; }\nprivate:\n};\n",
        ),
        (
            "a global qualifier in template arguments",
            "std::vector<::std::string> v;",
        ),
        ("a remainder", "int f(int a, int b) { return a % b; }"),
        (
            "a template argument list",
            "std::map<int, std::vector<int>> m;",
        ),
        (
            "typeof as the name of a function",
            "int f(V v) { return typeof(v); }",
        ),
        (
            "a lambda with parameters and a trailing return",
            "auto f = [](int x) -> int { return x; };",
        ),
        (
            "a nested class with a base",
            "struct S { struct T : B { }; };",
        ),
        ("a using-declaration", "struct D : B { using B::f; };"),
        ("a friend class", "struct S { friend class T; };"),
        ("a named pointer to member", "void (S::*pf)() = &S::f;"),
        (
            "an access through a pointer to member and a dot",
            "void f(S s, int S::*pm) { s.*pm = 1; }",
        ),
        ("a structured binding", "void f(P p) { auto [a, b] = p; }"),
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
    fn a_splice_across_lines_stays_one_name() {
        // Underscores on every line would read as one name for each
        // line, and three names in a row is not a declaration.
        let src = "using T = [:\n    substitute(^^X, {^^int})\n:];\n";
        same_length(src);
        let out = normalize(src).expect("the splice goes");
        assert_eq!(parse_errors(Lang::Cpp, &out), 0, "{out}");
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
    fn a_contract_clause_reads_after_operator_equals_a_requirement_and_before_an_attribute() {
        // `operator=` spells an `=` that is not an assignment, a
        // trailing requires-clause stands between the declarator and the
        // clause, and an attribute can follow a clause.
        for src in [
            "struct S { S& operator=(const S& o) pre(&o != this) = default; };\n",
            "template <class T> int f(T n) requires C<T> pre(n > 0) { return n; }\n",
            "int f(int n) pre(n > 0) [[gnu::hot]] { return n; }\n",
        ] {
            same_length(src);
            let out = normalize(src).expect("the clause goes");
            assert!(!out.contains("pre("), "the clause stayed: {out}");
            assert_eq!(parse_errors(Lang::Cpp, &out), 0, "{out}");
        }
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
            assert_eq!(
                parse_errors(Lang::Cpp, &text),
                0,
                "{what}: still fails to parse"
            );
        }
    }

    #[test]
    fn an_attribute_macro_takes_its_arguments_with_it() {
        // `GUARDED_BY(mu)` is one attribute. Blanking only the name
        // leaves `(mu)` after a declarator, and that does not parse.
        let yaml = "AttributeMacros: [GUARDED_BY, REQUIRES]\n";
        let src =
            "struct S {\n  int n GUARDED_BY(mu);\n  int take() REQUIRES(mu) { return n; }\n};\n";
        let out = with_macros(src, yaml).expect("the macros go");
        assert_eq!(out.len(), src.len());
        assert!(!out.contains("(mu)"), "the arguments stayed: {out}");
        assert_eq!(parse_errors(Lang::Cpp, &out), 0, "{out}");
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
            "template <int N = {}> struct S { };\n",
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

    /// A table of braced rows and an enum of many values cost one pass
    /// each. The template tests scan back, and a scan across every row
    /// before it made v8's gay-fixed.cc, a table of 100000 rows, take
    /// hours.
    #[test]
    fn a_large_table_costs_one_pass() {
        let rows = 40_000;
        let mut table = String::from("static const Row kRows[] = {\n");
        let mut values = String::from("enum class Code {\n");
        for n in 0..rows {
            table.push_str(&format!("  {{{n}.5e+14, {n}, \"{n}\", -{n}}},\n"));
            values.push_str(&format!("  kCode{n} = {n},\n"));
        }
        table.push_str("};\n");
        values.push_str("};\n");
        for src in [&table, &values] {
            let started = std::time::Instant::now();
            let _ = normalize(src);
            let spent = started.elapsed();
            assert!(
                spent < std::time::Duration::from_secs(20),
                "{} bytes took {spent:?}",
                src.len()
            );
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
    fn every_attribute_goes_and_the_declaration_under_it_stays() {
        // No attribute carries work, and the grammar reads some of their
        // positions and not the others, so every one goes.
        for (src, kept) in [
            (
                "struct S { [[nodiscard]] friend bool ok(S const& s) { return true; } };\n",
                "friend bool ok(S const& s) { return true; }",
            ),
            (
                "struct S { [[nodiscard]] friend constexpr bool ok(S const& s); };\n",
                "friend constexpr bool ok(S const& s);",
            ),
            (
                "[[nodiscard]] int f() { return 1; }\n",
                "int f() { return 1; }",
            ),
            ("enum E { A [[deprecated]] = 1, B };\n", "= 1, B };"),
            (
                "void f(P p) { auto [a [[maybe_unused]], b] = p; }\n",
                ", b] = p; }",
            ),
            (
                "auto f = [] [[nodiscard]] () { return 1; };\n",
                "() { return 1; };",
            ),
            (
                "struct [[nodiscard]] alignas(64) S { char c; };\n",
                "alignas(64) S { char c; };",
            ),
        ] {
            same_length(src);
            let out = normalize(src).expect("the attribute must go");
            assert!(out.contains(kept), "the declaration changed: {out}");
            assert!(!out.contains("[["), "an attribute stayed: {out}");
            assert_eq!(parse_errors(Lang::Cpp, &out), 0, "still fails: {out}");
        }
        // `[[:` is `[` and a splice, and not an attribute.
        let out = normalize("int x = a[[:r:]];\n").expect("the splice goes");
        assert!(out.contains("a["), "a subscript went: {out}");
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
            "auto f = [] noexcept(true) { return 1; };\n",
            "auto f = [x] consteval { return x; };\n",
            "auto f = [] static constexpr noexcept -> int { return 1; };\n",
            "auto f = []() static { return 1; };\n",
        ] {
            same_length(src);
            let out = normalize(src).expect("the specifier goes");
            assert_eq!(parse_errors(Lang::Cpp, &out), 0, "still fails: {out}");
        }
        // A specifier after an attribute, or after a parameter list,
        // belongs to what the grammar already reads, and it stays.
        for (src, word) in [
            (
                "[[nodiscard]] constexpr int f() { return 1; }\n",
                "constexpr int f",
            ),
            (
                "struct S { [[nodiscard]] static constexpr int f() { return 1; } };\n",
                "static constexpr int f",
            ),
            ("auto f = []() consteval { return 1; };\n", "() consteval {"),
        ] {
            let out = normalize(src).unwrap_or_else(|| src.to_string());
            assert!(out.contains(word), "a specifier went: {out}");
        }
        // `noexcept(true)` takes its condition with it, so the condition
        // does not stay behind as a parameter list.
        let out = normalize("auto f = [] noexcept(true) { return 1; };\n").unwrap();
        assert!(!out.contains("(true)"), "{out}");
    }

    #[test]
    fn a_relocatable_class_keeps_its_own_name() {
        // Unrewritten, the grammar reads the property as the name of the
        // class, and the class reports under a keyword. No parse error
        // shows that.
        let src = "struct Pool trivially_relocatable_if_eligible replaceable_if_eligible {\n\
                   \x20 int take() { return 1; }\n\
                   };\n";
        same_length(src);
        let f = normalized(src);
        // A class is not a unit, and its method carries the class in
        // the qualified name.
        let names: Vec<&str> = f.units[1..].iter().map(|u| &*u.qualname).collect();
        assert_eq!(names, ["Pool::take"]);
    }

    #[test]
    fn a_line_splice_after_trailing_spaces_joins_the_comment_as_the_compiler_does() {
        // C++23 reads a backslash, spaces and a line break as a splice.
        // The grammar reads the next line as code, and a function the
        // compiler never sees reports as a unit.
        let src = "int code = 1; // a comment \\   \n\
                   int not_code() { return 1; }\n\
                   int code_too() { return 2; }\n";
        same_length(src);
        let names: Vec<String> = normalized(src).units[1..]
            .iter()
            .map(|u| u.name.to_string())
            .collect();
        assert_eq!(names, ["code_too"]);
    }

    #[test]
    fn a_group_under_if_0_is_not_code_and_its_else_branch_is() {
        let src = "#if 0\nint dead() { return 'x; }\n#else\nint live() { return 1; }\n#endif\n";
        same_length(src);
        let out = normalize(src).expect("the dead group goes");
        assert_eq!(parse_errors(Lang::Cpp, &out), 0, "{out}");
        let names: Vec<String> = normalized(src).units[1..]
            .iter()
            .map(|u| u.name.to_string())
            .collect();
        assert_eq!(names, ["live"]);
        // A nested conditional inside the dead group does not end it.
        let nested = "#if 0\n#ifdef X\ngarbage '\n#endif\nmore garbage \"\n#endif\nint live() { return 1; }\n";
        let out = normalize(nested).expect("the dead group goes");
        assert_eq!(parse_errors(Lang::Cpp, &out), 0, "{out}");
    }

    #[test]
    fn a_linkage_block_split_by_ifdef_keeps_the_declarations_in_it() {
        let src = "#ifdef __cplusplus\nextern \"C\" {\n#endif\n\
                   int take(int n) { return n; }\n\
                   #ifdef __cplusplus\n}\n#endif\n";
        same_length(src);
        let out = normalize(src).expect("the braces go");
        assert_eq!(parse_errors(Lang::Cpp, &out), 0, "{out}");
        assert!(out.contains("int take(int n) { return n; }"), "{out}");
    }

    #[test]
    fn a_typeid_of_a_type_reads_and_its_member_call_stays() {
        let src = "const char* n = typeid(int).name();\nconst char* m = typeid(x).name();\n";
        same_length(src);
        let out = normalize(src).expect("typeid becomes sizeof");
        assert_eq!(parse_errors(Lang::Cpp, &out), 0, "{out}");
        assert_eq!(out.matches(".name()").count(), 2, "{out}");
    }

    /// The rewrite for a Clang of this major version.
    fn lowered(src: &str, major: u32) -> Option<String> {
        super::lower_for_clang(src, major).map(|done| done.text)
    }

    #[test]
    fn a_clang_loses_only_what_it_cannot_read_and_what_does_no_work() {
        // The source, a piece that has to go, and a piece that has to stay.
        let cases: &[(&str, &str, &str)] = &[
            (
                "int f(int n) pre(n > 0) post(r: r > 0) { return n; }\n",
                "pre(",
                "int f(int n)",
            ),
            (
                "void f(int n) { contract_assert(n > 0); g(n); }\n",
                "contract_assert",
                "; g(n); }",
            ),
            ("struct [[=1]] S { };\n", "=1", "]] S { };"),
            (
                "struct S { [[nodiscard, =tag]] int f(); };\n",
                "=tag",
                "[[nodiscard",
            ),
            (
                "struct S trivially_relocatable_if_eligible { };\n",
                "relocatable",
                "struct S",
            ),
        ];
        for (src, gone, kept) in cases {
            let out = lowered(src, 23).expect("a construct Clang 23 does not read goes");
            assert_eq!(out.len(), src.len(), "a removal moved an offset: {out}");
            assert!(!out.contains(gone), "{gone} stayed: {out}");
            assert!(out.contains(kept), "{kept} went: {out}");
        }
    }

    #[test]
    fn a_clang_keeps_what_it_reads_where_the_grammar_needs_a_rewrite() {
        // An attribute, `typeid`, a digraph, a reason on `delete`, a pack
        // index and a reflection all mean something to Clang, and a
        // removal would change what clang-tidy checks.
        for src in [
            "[[nodiscard]] int f();\n",
            "auto& t = typeid(int);\n",
            "int f() <% return 1; %>\n",
            "struct S { S(const S&) = delete(\"no\"); };\n",
            "template <class... T> using First = T...[0];\n",
            "constexpr auto r = ^^int;\n",
        ] {
            assert_eq!(
                lowered(src, 23),
                None,
                "a removal changed what Clang reads: {src}"
            );
        }
    }

    #[test]
    fn a_reason_on_delete_goes_only_for_a_clang_before_19() {
        let src = "struct S { S(const S&) = delete(\"no copy\"); };\n";
        assert_eq!(lowered(src, 19), None);
        let out = lowered(src, 18).expect("Clang 18 reads no reason");
        assert!(out.contains("= delete"), "{out}");
        assert!(!out.contains("no copy"), "{out}");
    }

    #[test]
    fn the_clang_rewrite_reaches_a_macro_body_and_the_grammar_rewrite_does_not() {
        // Clang expands the body at each use, and only the body can take
        // the assertion out. The grammar reads the body as raw text.
        let src = "#define ASSERT(c) contract_assert(c)\nvoid f(int n) { ASSERT(n > 0); }\n";
        let out = lowered(src, 23).expect("the body loses its assertion");
        assert!(out.starts_with("#define ASSERT(c)"), "{out}");
        assert!(!out.contains("contract_assert"), "{out}");
        assert_eq!(
            normalize(src),
            None,
            "the grammar rewrite edited a macro body"
        );
    }

    #[test]
    fn the_gaps_name_reflection_and_an_expansion_statement_before_clang_23() {
        let src = "void f() { template for (auto x : {1, 2}) { g(^^int); } }\n";
        let mut before = super::clang_gaps(src, 22);
        before.sort_unstable();
        assert_eq!(before, ["expansion statement", "reflection"]);
        assert_eq!(super::clang_gaps(src, 23), ["reflection"]);
        assert!(super::clang_gaps("int f() { return 1; }\n", 22).is_empty());
    }

    #[test]
    fn a_clang_compiles_the_lowered_text_that_it_rejects_as_written() {
        // This test needs clang++ on the PATH, and it stops without a
        // result on a machine that has none. It is the one test that
        // holds a removal to its meaning: what goes has to leave a
        // program that the compiler accepts.
        use std::io::Write as _;
        use std::process::{Command, Stdio};
        let Ok(version) = Command::new("clang++").arg("--version").output() else {
            return;
        };
        let Some(major) = String::from_utf8_lossy(&version.stdout)
            .split_whitespace()
            .skip_while(|word| *word != "version")
            .nth(1)
            .and_then(|v| v.split('.').next()?.parse::<u32>().ok())
        else {
            return;
        };
        let compiles = |text: &str| {
            let mut child = Command::new("clang++")
                .args(["-std=c++2c", "-fsyntax-only", "-w", "-x", "c++", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("clang++ runs");
            child
                .stdin
                .take()
                .expect("a stdin")
                .write_all(text.as_bytes())
                .expect("the source goes in");
            child.wait().expect("clang++ ends").success()
        };
        for src in [
            "int f(int n) pre(n > 0) post(r: r > 0) { return n; }\n",
            "void g(int);\nvoid f(int n) { contract_assert(n > 0); g(n); }\n",
            "struct [[nodiscard, =1]] S { int f() { return 0; } };\n",
            "struct S trivially_relocatable_if_eligible { int x; };\nS s{1};\n",
            "#define ASSERT(c) contract_assert(c)\nvoid f(int n) { ASSERT(n > 0); }\n",
            "struct B { virtual int f(int n) pre(n > 0) = 0; };\n",
            "template <class T>\n  requires (sizeof(T) > 0)\nint f(T n) pre(n > 0) { return 0; }\n",
        ] {
            let out = lowered(src, major).unwrap_or_else(|| src.to_string());
            assert!(
                compiles(&out),
                "clang++ {major} rejects the lowered text:\n{out}"
            );
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
