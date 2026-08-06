//! What a comment SAYS, reduced to counts.
//!
//! Every comment-content metric — doc length, ground density — needs the
//! same thing first: the English out of a comment, with the syntax and
//! the code taken away. That reduction is language-agnostic and belongs
//! here, once, rather than in twenty-two packs.
//!
//! Two decisions carry the module.
//!
//! CODE IS NOT PROSE. A doc comment's fenced block is an EXAMPLE, and
//! counting it as writing is how the doc:body-ratio metric died: gold
//! Rust appeared to out-document FX 2336 to 1751 per mille, because
//! ripgrep and regex carry twenty-to-eighty-line builder docs that are
//! almost entirely `let` bindings. Strip the fences and the two are
//! comparable. Everything downstream depends on this step.
//!
//! COUNTS, NOT STRINGS. A 6.39M-line scan holds ~640k comments; keeping
//! each stripped body as a `Box<str>` costs ~38MB of live heap for text
//! nothing reads twice. A `Prose` is six bytes.

/// One comment run, measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Prose {
    /// Tokens carrying at least one letter. Saturates: a 65k-word
    /// comment and a 66k-word comment are the same finding.
    pub words: u16,
    /// Phrases asserting a REASON — why this code is as it is.
    pub grounds: u8,
    /// Phrases asserting a PURPOSE — what the code is for. Counted
    /// apart from grounds and never added to them.
    pub purposes: u8,
    /// Sentence terminators, floored at one when anything was written.
    pub sentences: u8,
}

/// Phrases that mark a comment as giving GROUNDS: a REASON the code is
/// as it is, which is the thing a reader cannot recover from the code
/// itself.
///
/// Measured human:machine ratios over the paired corpus — thus 193x,
/// so that 11.3x, since 8.1x, otherwise 7.1x, because 4.6x. Written as
/// phrases and matched a token at a time, so `so that` is one ground
/// and `so` alone is none.
///
/// `as` is deliberately absent. "as a result" is a ground, "as usual"
/// is not and "cast as usize" is code; nothing short of a parser tells
/// them apart, and it was never in the measured set.
const GROUNDS: &[&str] = &[
    "because",
    "otherwise",
    "thus",
    "hence",
    "therefore",
    "so that",
];

/// The one ground whose sense depends on what follows it: `since 1.2.0`
/// and `since the last flush` are DATES, `since the buffer is full` is
/// a reason. See `causal_since`.
const SINCE: &str = "since";

/// Phrases that state a PURPOSE — what the code is FOR, or what it
/// stops from happening. Counted separately and NEVER credited as
/// grounds.
///
/// This split is the finding. Pooled as one "subordinator density" the
/// signal measured 0.97 within-repo and died, because the two halves
/// move in OPPOSITE directions: grounds run 4.6x to 193x human, while
/// purposes run at or below parity — to avoid 0.7x, prevents 0.3x. A
/// machine says what a line is for at human rates and says why the
/// obvious alternative fails at a fifth of them.
const PURPOSES: &[&str] = &[
    "to avoid",
    "to prevent",
    "in order to",
    "prevents",
    "due to",
];

/// Tokens after `since` that decide its sense. Three, because that is
/// the span a date or a landmark occupies — `since 1.2.0`, `since the
/// last flush` — and a fourth would start reading the clause itself.
const SINCE_LOOKAHEAD: usize = 3;

/// Comment syntax every ecosystem spells the same way, longest first.
/// A pack's own `doc_markers` are tried alongside these — `#:`, `@doc`,
/// `=head` and the like — and the longest match on a line wins.
const UNIVERSAL: &[&str] = &[
    "\"\"\"", "'''", "///", "//!", "(**", "/**", "=cut", "//", "/*", "*/", "(*", "*)", "#!", "#",
    "--",
];

/// Columns of indentation, after the block is dedented, at which a line
/// stops being a paragraph and becomes an example.
const CODE_INDENT: usize = 4;

/// Share of a comment's LETTERS that may come from a script which does
/// not put spaces between words before the comment is refused. Above
/// this, `words` would count line fragments rather than words, and a
/// count that means something different per language is worse than no
/// count at all.
const MAX_UNSPACED: f32 = 0.30;

/// A tab's width in columns, for the indentation test.
const TAB_WIDTH: usize = 4;

/// The prose in one comment run, or None when this comment cannot be
/// measured in words at all.
///
/// `markers` is the pack's own `doc_markers`; the universal comment
/// syntax is known here. The pipeline is ordered and each step depends
/// on the last: markers must go before fences (the fence is written
/// `/// ` + "```"), fences before indent blocks (a fenced block is
/// indented too), and code before tokenizing (an example is mostly
/// identifiers).
pub fn measure(text: &str, markers: &[&str]) -> Option<Prose> {
    let (stripped, every_line) = strip_markers(text, markers);
    let body = drop_code(&dedent(&stripped, every_line));
    if unspaced_share(&body) > MAX_UNSPACED {
        return None;
    }
    let mut prose = Prose {
        sentences: sentences(&body),
        ..Prose::default()
    };
    // Numbers stay in the token stream although they are not words:
    // `since` is told from a date by what follows it, and dropping the
    // date first would make every `since 1.2.0` read as a reason.
    let tokens: Vec<&str> = body
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '\'')
        .filter(|t| !t.is_empty())
        .collect();
    for (at, token) in tokens.iter().enumerate() {
        let rest = &tokens[at..];
        if token.chars().any(char::is_alphabetic) {
            prose.words = prose.words.saturating_add(1);
        }
        let ground = opens(rest, GROUNDS) || causal_since(rest);
        prose.grounds = prose.grounds.saturating_add(ground as u8);
        prose.purposes = prose.purposes.saturating_add(opens(rest, PURPOSES) as u8);
    }
    Some(prose)
}

/// Does any of these phrases start here? A phrase is spelled with
/// spaces and matched a TOKEN at a time, which is what makes `because`
/// inside `because_of` fail — the underscore is part of the token, and
/// a backticked code span was dropped before any of this.
fn opens(tokens: &[&str], phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| {
        phrase
            .split(' ')
            .enumerate()
            .all(|(n, part)| tokens.get(n).is_some_and(|t| t.eq_ignore_ascii_case(part)))
    })
}

/// Is this `since` a reason rather than a date?
///
/// "since 1.2.0", "since v2", "since the last flush", "since then" are
/// all TEMPORAL and say nothing about why the code is as it is; "since
/// the buffer is full" is a ground. The senses are told apart by what
/// follows: a digit anywhere in the next three tokens (which is also
/// what a version-shaped token carries), the word `then`, or the
/// landmark `the last`.
fn causal_since(tokens: &[&str]) -> bool {
    if !tokens
        .first()
        .is_some_and(|t| t.eq_ignore_ascii_case(SINCE))
    {
        return false;
    }
    let ahead = &tokens[1..tokens.len().min(1 + SINCE_LOOKAHEAD)];
    let dated = ahead
        .iter()
        .any(|t| t.chars().any(|c| c.is_ascii_digit()) || t.eq_ignore_ascii_case("then"));
    let landmark = ahead
        .windows(2)
        .any(|w| w[0].eq_ignore_ascii_case("the") && w[1].eq_ignore_ascii_case("last"));
    !dated && !landmark
}

/// Comment syntax off the front and back of every line, and the `*`
/// that decorates a block comment's continuation lines.
///
/// Indentation is KEPT: it is the only evidence that a line is an
/// indented example, and `dedent` takes the common part away next.
/// Exactly one space after the marker is eaten — the conventional
/// separator — so that `///     let x = 1;` keeps its four columns.
///
/// Also answers whether EVERY line carried a marker, which is how
/// `dedent` tells a comment run from a docstring: a `///` run wears its
/// indentation on every line, a docstring lost the first line's to the
/// opening quote.
fn strip_markers(text: &str, markers: &[&str]) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut every_line = true;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let (pad, rest) = line.split_at(indent);
        let marker = longest_marker(rest, markers);
        let rest = match marker {
            Some(m) => rest[m..].strip_prefix(' ').unwrap_or(&rest[m..]),
            None => rest,
        };
        // A block comment's continuation lines are decorated with `*`,
        // and a bare `*` on its own line is the decoration alone.
        let decorated = rest
            .strip_prefix("* ")
            .or_else(|| rest.strip_suffix('*').filter(|r| r.is_empty()));
        let rest = decorated.unwrap_or(rest);
        let rest = rest.trim_end().trim_end_matches(['*', '/', '"', '\'', ')']);
        every_line &= marker.is_some() || decorated.is_some() || line.trim().is_empty();
        out.push_str(pad);
        out.push_str(rest);
        out.push('\n');
    }
    (out, every_line)
}

/// Length of the longest comment marker this line opens with. Python's
/// string prefixes (`r"""`, `f'''`) are skipped first: they are part of
/// the literal's syntax, not of what it says.
fn longest_marker(rest: &str, markers: &[&str]) -> Option<usize> {
    let prefix = rest
        .find(['"', '\''])
        .filter(|n| *n > 0 && *n <= 2 && rest[..*n].chars().all(|c| "rRbBuUfF".contains(c)))
        .unwrap_or(0);
    let body = &rest[prefix..];
    UNIVERSAL
        .iter()
        .chain(markers)
        .filter(|m| body.starts_with(**m))
        .map(|m| prefix + m.len())
        .max()
}

/// Take the block's own indentation away, so that what is left is
/// indentation the WRITER chose — which is the evidence that a line is
/// an example rather than a paragraph.
///
/// Where the first line lost its indentation to an opening quote it
/// cannot speak for the block, and PEP 257's rule applies: the common
/// indent comes from the later lines, and the summary line is left
/// alone. Where every line carried a marker the pad is uniform and the
/// first line counts like any other — otherwise a `///` run whose only
/// later lines are an indented example would be dedented by the
/// example's own indentation, and the example would read as prose.
fn dedent(text: &str, every_line: bool) -> String {
    let common = text
        .lines()
        .skip(!every_line as usize)
        .filter(|l| !l.trim().is_empty())
        .map(columns)
        .min()
        .unwrap_or(0);
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.lines().enumerate() {
        let cut = if n == 0 && !every_line { 0 } else { common };
        out.push_str(undented(line, cut));
        out.push('\n');
    }
    out
}

/// The line with `cut` columns of leading whitespace taken off. A line
/// with less than that is all indentation, and nothing is left of it.
fn undented(line: &str, cut: usize) -> &str {
    let mut used = 0;
    for (at, c) in line.char_indices() {
        if used >= cut || !matches!(c, ' ' | '\t') {
            return &line[at..];
        }
        used += width(c);
    }
    ""
}

/// Leading whitespace of a line, in columns.
fn columns(line: &str) -> usize {
    line.chars()
        .map_while(|c| matches!(c, ' ' | '\t').then(|| width(c)))
        .sum()
}

/// Columns one whitespace character occupies.
fn width(c: char) -> usize {
    match c {
        '\t' => TAB_WIDTH,
        _ => 1,
    }
}

/// Everything that is code rather than writing: fenced blocks, indented
/// blocks, inline spans, and URLs.
///
/// An unclosed fence swallows the rest of the comment on purpose — a
/// run whose fence never closes is a truncated example, and guessing
/// where it ended would count identifiers as words.
fn drop_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || columns(line) >= CODE_INDENT && !trimmed.is_empty() {
            continue;
        }
        drop_spans(trimmed, &mut out);
        out.push('\n');
    }
    out
}

/// Inline code spans and URLs off one line. A backtick with no partner
/// on its line closes nothing, so it is dropped alone rather than
/// eating the rest of the sentence.
fn drop_spans(line: &str, out: &mut String) {
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        out.push_str(&rest[..open]);
        rest = match rest[open + 1..].find('`') {
            Some(close) => &rest[open + close + 2..],
            None => &rest[open + 1..],
        };
    }
    for word in rest.split_whitespace() {
        if word.contains("://") || word.starts_with("www.") {
            continue;
        }
        out.push_str(word);
        out.push(' ');
    }
}

/// Sentence terminators, floored at one when anything was written at
/// all — an undotted line is still one statement. A period closing a
/// one-letter token does not terminate anything: `e.g.` and `i.e.` are
/// the abbreviations that appear in every corpus.
fn sentences(text: &str) -> u8 {
    let mut count = 0u32;
    let mut run = 0usize;
    let mut ended = false;
    let mut wrote = false;
    for c in text.chars() {
        if matches!(c, '.' | '!' | '?') {
            if (run > 1 || c != '.') && !ended {
                count += 1;
                ended = true;
            }
            continue;
        }
        ended = false;
        run = if c.is_alphanumeric() {
            wrote = true;
            run + 1
        } else {
            0
        };
    }
    count.max(wrote as u32).min(u8::MAX as u32) as u8
}

/// Share of this text's LETTERS written in a script that separates
/// words by something other than a space.
///
/// Cyrillic, Greek and Hangul are deliberately absent: they space their
/// words, so counting them works exactly as it does for English. The
/// scripts here — Han, kana, Thai, Lao, Khmer, Myanmar — do not, and a
/// whitespace word count over them measures line breaks.
fn unspaced_share(text: &str) -> f32 {
    let (mut letters, mut unspaced) = (0u32, 0u32);
    for c in text.chars() {
        if !c.is_alphabetic() {
            continue;
        }
        letters += 1;
        unspaced += matches!(c,
            '\u{3040}'..='\u{30ff}'      // hiragana, katakana
            | '\u{3400}'..='\u{4dbf}'    // CJK extension A
            | '\u{4e00}'..='\u{9fff}'    // CJK unified ideographs
            | '\u{f900}'..='\u{faff}'    // CJK compatibility ideographs
            | '\u{0e00}'..='\u{0eff}'    // Thai, Lao
            | '\u{1000}'..='\u{109f}'    // Myanmar
            | '\u{1780}'..='\u{17ff}'    // Khmer
        ) as u32;
    }
    if letters == 0 {
        return 0.0;
    }
    unspaced as f32 / letters as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str, markers: &[&str]) -> u16 {
        measure(text, markers).expect("measurable").words
    }

    #[test]
    fn every_comment_syntax_loses_its_marker() {
        // One claim, four words, however the ecosystem spells a comment.
        for text in [
            "// the retry is bounded",
            "/// the retry is bounded",
            "//! the retry is bounded",
            "# the retry is bounded",
            "-- the retry is bounded",
            "/** the retry is bounded */",
            "/**\n * the retry is bounded\n */",
            "\"\"\"the retry is bounded\"\"\"",
            "r\"\"\"the retry is bounded\"\"\"",
            "(** the retry is bounded *)",
            "=head1 the retry is bounded",
        ] {
            assert_eq!(words(text, &["=head", "#:"]), 4, "{text:?}");
        }
        // A pack marker the universal list cannot know: Sphinx's `#:`
        // and Elixir's `@doc`, which is a call rather than a comment.
        assert_eq!(words("#: the retry is bounded", &["#:"]), 4);
        assert_eq!(words("@doc \"bounded retry\"", &["@doc"]), 2);
    }

    #[test]
    fn a_builder_doc_measures_as_its_prose_only() {
        // The finding that killed doc:body-ratio. A Rust builder's doc
        // is four words of writing and thirty lines of example; before
        // the fences were stripped it read as thirty-four.
        let mut doc = String::from("/// Sets the case sensitivity.\n///\n/// ```\n");
        for n in 0..30 {
            doc.push_str(&format!(
                "/// let builder{n} = RegexBuilder::new(pattern);\n"
            ));
        }
        doc.push_str("/// ```\n");
        assert_eq!(words(&doc, &[]), 4);
    }

    #[test]
    fn an_indented_example_is_not_writing() {
        // Markdown's other code block, which Rust's own docs use as
        // often as fences, and which survives the dedent that a
        // docstring needs.
        let rust = "/// Returns the parsed value.\n///\n///     let v = parse(text);\n///     assert!(v.is_ok());\n";
        assert_eq!(words(rust, &[]), 4);
        let py = "\"\"\"Parse the text.\n\n    Example:\n        v = parse(text)\n    \"\"\"";
        assert_eq!(words(py, &[]), 4, "docstring dedented, then example cut");
    }

    #[test]
    fn spans_and_links_are_syntax_not_writing() {
        assert_eq!(words("/// Wraps `RegexBuilder::new` for callers.", &[]), 3);
        assert_eq!(
            words("// see https://example.com/spec for the rules", &[]),
            4
        );
        assert_eq!(words("// a lone ` backtick", &[]), 3);
    }

    #[test]
    fn a_comment_in_an_unspaced_script_is_refused() {
        // Whitespace word counting over Han characters measures line
        // breaks, so the comment is skipped rather than mismeasured.
        assert!(measure("// 这个函数返回解析后的值", &[]).is_none());
        assert!(measure("// この関数は解析結果を返す", &[]).is_none());
        // A Japanese identifier inside an English sentence is not a
        // Japanese comment.
        assert!(measure("// the 日本 locale is handled by the parser", &[]).is_some());
        // Cyrillic spaces its words: it counts like any other prose.
        assert_eq!(words("// эта функция возвращает значение", &[]), 4);
    }

    #[test]
    fn sentences_are_counted_by_their_terminators() {
        let s = |t: &str| measure(t, &[]).unwrap().sentences;
        assert_eq!(s("// one claim"), 1, "unterminated is still one");
        assert_eq!(s("// one claim. and a second one."), 2);
        assert_eq!(s("// what now? nothing! at all."), 3);
        assert_eq!(s("// wait... one claim"), 1, "an ellipsis is one break");
        assert_eq!(s("// e.g. the retry path"), 1, "an abbreviation is not");
        assert_eq!(s("//"), 0, "nothing was written");
    }

    #[test]
    fn every_ground_term_is_counted_once() {
        let g = |t: &str| measure(t, &[]).unwrap().grounds;
        for text in [
            "// retried because the socket was reset",
            "// otherwise the socket stays open",
            "// thus the socket is closed here",
            "// hence the socket is closed here",
            "// therefore the socket is closed here",
            "// closed here so that the socket is freed",
            "// since the socket is already closed",
        ] {
            assert_eq!(g(text), 1, "{text:?}");
        }
        // `so` alone is not a ground, and the bigram is one ground
        // rather than two tokens' worth.
        assert_eq!(g("// so the socket is freed"), 0);
        assert_eq!(g("// so that the socket is freed, so that it drains"), 2);
        // Word boundaries: the term inside an identifier, and the term
        // inside a code span the pipeline already removed.
        assert_eq!(g("// see because_of for the reason"), 0);
        assert_eq!(g("// `since_last` is reset here"), 0);
        assert_eq!(g("// Therefore the read is bounded."), 1, "case-folded");
    }

    #[test]
    fn since_is_a_reason_only_when_it_is_not_a_date() {
        let g = |t: &str| measure(t, &[]).unwrap().grounds;
        // Temporal: a version, a digit anywhere in reach, a landmark.
        assert_eq!(g("// deprecated since 1.2.0"), 0);
        assert_eq!(g("// unsupported since v2 of the protocol"), 0);
        assert_eq!(g("// unchanged since the last flush"), 0);
        assert_eq!(g("// unchanged since then"), 0);
        // Causal: nothing ahead dates it.
        assert_eq!(g("// skipped since the buffer is full"), 1);
        assert_eq!(g("// skipped since nothing was written"), 1);
        // The lookahead is three tokens: a digit further out than that
        // is a different clause and does not disarm the reason.
        assert_eq!(g("// skipped since the buffer holds 4 entries"), 1);
    }

    #[test]
    fn a_purpose_is_counted_apart_and_never_credited() {
        let p = |t: &str| measure(t, &[]).unwrap();
        for text in [
            "// batched to avoid a second round trip",
            "// batched to prevent a second round trip",
            "// batched in order to spare a round trip",
            "// batching prevents a second round trip",
            "// batched due to the round trip cost",
        ] {
            let m = p(text);
            assert_eq!((m.purposes, m.grounds), (1, 0), "{text:?}");
        }
        // Both senses in one comment, each counted as itself.
        let both = p("// batched to avoid a round trip, because the link is slow");
        assert_eq!((both.grounds, both.purposes), (1, 1));
    }

    #[test]
    fn a_word_is_a_token_carrying_a_letter() {
        // Numbers, punctuation and bare underscores are not words;
        // an apostrophe and an underscore live inside one.
        assert_eq!(words("// don't retry the read_all path 3 times", &[]), 6);
    }
}
