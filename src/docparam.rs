//! What a doc comment CLAIMS its parameters are called.
//!
//! The signature is the truth and the documentation is a claim about
//! it; a name in the claim that the signature does not declare was
//! renamed or invented, and the reader who trusts it is wrong. Reading
//! the claim is what this module does — comparing it against the
//! signature is `crate::metrics`.
//!
//! CONVENTIONS, NOT LANGUAGES. Every parser here runs on every
//! comment, and that is deliberate: JSDoc's `@param` is written in
//! JavaScript, Java, PHP, C++, Ruby and OCaml, Doxygen's `\param` in C
//! and C++, and a `Args:` block wherever someone liked the look of it.
//! A parameter-documentation convention is a property of a COMMUNITY,
//! not of a grammar, so dispatching on the language would answer the
//! wrong question — and each convention is recognized by syntax no
//! other convention writes, so running them all costs only the scan.
//!
//! THE SET, NOT THE ORDER. `@param b` before `@param a` documents both
//! parameters and says nothing about which comes first. Only presence
//! is decidable from a comment, so only presence is read.

/// Doc tags that introduce one parameter by name: JSDoc and its
/// descendants (`@param`), Doxygen's backslash spelling, and the two
/// abbreviations that appear in the wild. `@tparam` is deliberately
/// absent — it names a TEMPLATE parameter, which no argument list
/// declares.
const TAGS: &[&str] = &["@param", "@arg", "@argument", "\\param", "\\arg"];

/// Doxygen's direction annotations, written flush against the tag:
/// `@param[out] result`. Without this the bracket reads as JSDoc's
/// optional-parameter syntax and the doc claims a parameter named
/// `out`.
const DIRECTIONS: &[&str] = &["[in]", "[out]", "[in,out]", "[inout]", "[out,in]"];

/// Headings that open a block of parameter entries — Google's `Args:`,
/// numpydoc's underlined `Parameters`, and the Markdown `# Arguments`
/// section a Rust doc writes. Compared case-insensitively against the
/// heading with its punctuation removed.
const ARG_HEADINGS: &[&str] = &[
    "args",
    "arguments",
    "parameters",
    "params",
    "keyword args",
    "keyword arguments",
    "other parameters",
];

/// Headings that CLOSE one. The trap this exists for: an `Args:` block
/// read without a terminator swallows the return description, and every
/// word in it reads as a parameter name. Indentation ends a block on
/// its own; this catches the docstrings that misindent their sections.
const END_HEADINGS: &[&str] = &[
    "returns",
    "return",
    "yields",
    "yield",
    "raises",
    "raise",
    "throws",
    "examples",
    "example",
    "note",
    "notes",
    "attributes",
    "warning",
    "warnings",
    "warns",
    "see also",
    "references",
    "todo",
    "panics",
    "safety",
    "errors",
];

/// Sigils and decoration a name may wear in prose: Perl's and PHP's
/// variable marks, a splat, C's address-of, Markdown emphasis, a
/// backtick span, brackets around an optional parameter, and the
/// punctuation that ends the token. `_` is NOT here — a leading
/// underscore is part of the name in every language that allows one,
/// and neither is a BRACE — PHP-Parser writes `@param array{` and
/// carries the shape over four lines, so a token wearing one is the
/// wreckage of a type this line-wise reader cannot follow.
const DECORATION: &[char] = &[
    '`', '*', '[', ']', '(', ')', '<', '>', '"', '\'', ',', ';', ':', '.', '-', '|', '&', '$', '@',
    '%', '\\', '#', '~', '/', '+', '!', '?', '=',
];

/// Words that are never a parameter's name, however a doc writes them.
///
/// `@param The left changes` omits the name and starts on the
/// description, so the first token is an article — and vscode's diff.ts
/// writes it twice. `a` and `it` are deliberately absent: both are real
/// parameter names in code that does arithmetic.
const NOT_A_NAME: &[&str] = &["the", "an", "this", "that", "these", "those", "its"];

/// The parameter names this doc comment claims, deduplicated and in no
/// particular order. Empty for the overwhelming majority of comments,
/// which document a contract in prose and name nothing.
pub fn documented(doc: &str, markers: &[&str]) -> Vec<Box<str>> {
    let body = crate::prose::body(doc, markers);
    let lines: Vec<&str> = body.lines().collect();
    let mut names = Vec::new();
    for line in &lines {
        tagged(line, &mut names);
        sphinx_fields(line, &mut names);
    }
    xml_elements(&body, &mut names);
    pod_items(&lines, &mut names);
    blocks(&lines, &mut names);
    names.sort_unstable();
    names.dedup();
    names
}

/// One `@param name` line. The tag must OPEN the line: redis writes
/// "The number of threads in @param tids" inside a description, and a
/// tag mentioned mid-sentence documents nothing.
fn tagged(line: &str, out: &mut Vec<Box<str>>) {
    let trimmed = line.trim_start();
    let Some(tag) = TAGS.iter().find(|t| trimmed.starts_with(**t)) else {
        return;
    };
    // `@parameters` is not `@param`: the tag ends at a boundary, and
    // the boundary has to be read BEFORE the whitespace is trimmed.
    let after = &trimmed[tag.len()..];
    if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return;
    }
    let mut rest = after.trim_start();
    if let Some(d) = DIRECTIONS.iter().find(|d| rest.starts_with(**d)) {
        rest = rest[d.len()..].trim_start();
    }
    // `@param {string | MessageFunction} name` — a JSDoc type is a
    // brace group, and it may hold braces of its own.
    match rest.starts_with('{') {
        true => rest = rest[brace_group(rest)..].trim_start(),
        // `@param [Array] collection` — YARD writes the type in SQUARE
        // brackets, where JSDoc writes an OPTIONAL PARAMETER's name.
        // The two are told apart by what is inside: a type is
        // capitalized, or namespaced, or a union, and a name is none of
        // those. Only where no brace type was written — after one, a
        // bracket can only be JSDoc's `[name=default]`.
        false if rest.starts_with('[') && yard_type(rest) => {
            rest = rest[group(rest, '[', ']')..].trim_start();
        }
        false => {}
    }
    out.extend(first_name(rest));
}

/// Byte length of the balanced brace group opening this text, or all of
/// it when the group never closes.
fn brace_group(text: &str) -> usize {
    group(text, '{', '}')
}

/// Byte length of the balanced group of these delimiters opening this
/// text, or all of it when the group never closes.
fn group(text: &str, open: char, close: char) -> usize {
    let mut depth = 0u32;
    for (at, c) in text.char_indices() {
        depth += (c == open) as u32;
        depth -= (c == close) as u32;
        if depth == 0 {
            return at + c.len_utf8();
        }
    }
    text.len()
}

/// Is this bracket group a YARD TYPE rather than a JSDoc optional
/// parameter's name? A type is capitalized, namespaced, a union, or a
/// list of any of those; a parameter name is a lone lowercase
/// identifier with an optional default.
fn yard_type(rest: &str) -> bool {
    let inside = &rest[1..group(rest, '[', ']').saturating_sub(1).max(1)];
    inside.starts_with(char::is_uppercase)
        || inside.contains("::")
        || inside.contains([',', '<', '|', ' '])
}

/// The name in what follows a tag.
///
/// PHPDoc writes the TYPE first and the name second — `@param string
/// $limit` — where JSDoc and Javadoc write the name first. PHP's sigil
/// is what tells them apart: a `$` on the second token means the first
/// was a type.
fn first_name(rest: &str) -> Option<Box<str>> {
    /// Tokens of a type before the name it belongs to: `array<string,
    /// mixed> $attributes` splits into three, and a generic with more
    /// commas would split into more.
    const TYPE_TOKENS: usize = 4;
    let tokens: Vec<&str> = rest.split_whitespace().take(TYPE_TOKENS).collect();
    let first = *tokens.first()?;
    let name = match first.starts_with('$') {
        true => first,
        false => tokens.iter().find(|t| t.starts_with('$')).unwrap_or(&first),
    };
    // Javadoc documents a TYPE parameter with the same tag, telling it
    // apart by angle brackets: `@param <K> the key type` beside
    // `@param builder the configured cache builder`. A type parameter
    // is not an argument, and reading caffeine's as one accounted for
    // most of what this metric found in Java.
    match name.starts_with('<') {
        true => None,
        false => name_of(name),
    }
}

/// Sphinx's field list: `:param url:`, or `:param dict headers:` where
/// the type comes first. The field's name runs to the closing colon and
/// the parameter is its LAST word, whether or not a type preceded it.
///
/// `:key` and `:keyword` are deliberately absent: they document a key
/// of `**kwargs`, which no signature declares by name.
fn sphinx_fields(line: &str, out: &mut Vec<Box<str>>) {
    const FIELDS: &[&str] = &[":param", ":parameter", ":arg", ":argument"];
    let trimmed = line.trim_start();
    let Some(rest) = FIELDS.iter().find_map(|f| trimmed.strip_prefix(*f)) else {
        return;
    };
    if !rest.starts_with(char::is_whitespace) {
        return;
    }
    let Some(end) = rest.find(':') else {
        return;
    };
    let field = &rest[..end];
    out.extend(field.split_whitespace().last().and_then(name_of));
}

/// C#'s `<param name="x">`. Read from the whole body rather than a line
/// at a time: the element is XML and nothing stops it wrapping.
/// `<typeparam name=` cannot match — the opening angle bracket is part
/// of the needle.
fn xml_elements(body: &str, out: &mut Vec<Box<str>>) {
    const OPEN: &str = "<param name=";
    let mut rest = body;
    while let Some(at) = rest.find(OPEN) {
        rest = &rest[at + OPEN.len()..];
        let quoted = rest.trim_start();
        let Some(quote) = quoted.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        if let Some(end) = quoted[1..].find(quote) {
            out.extend(name_of(&quoted[1..1 + end]));
        }
    }
}

/// Perl's POD `=item $name`.
///
/// The SCALAR sigil is required, and only that one. `=item` is how POD
/// spells every bulleted list — of methods, of options, of the `%a`,
/// `%h`, `%t` placeholders a log format accepts — and `$` is the
/// narrowest mark that says "this item is an argument". A slurpy
/// `@rest` documented as an item goes unread, which costs a finding and
/// invents none.
fn pod_items(lines: &[&str], out: &mut Vec<Box<str>>) {
    for line in lines {
        let Some(rest) = line.trim_start().strip_prefix("=item ") else {
            continue;
        };
        if rest.trim_start().starts_with('$') {
            out.extend(first_name(rest.trim_start()));
        }
    }
}

/// How one block convention writes itself down.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// `# Arguments` and a Markdown list under it. Ends at the next
    /// heading.
    Bullets,
    /// `Args:` and entries INDENTED under it. Ends where the
    /// indentation returns to the heading's own column.
    Indented,
    /// `Parameters` over a row of dashes, entries at the heading's own
    /// column. Ends at the next underlined heading — which is the only
    /// terminator numpydoc has, its sections being flush left.
    Underlined,
}

/// A heading followed by one entry per parameter — the three block
/// conventions, which differ in how an entry is written and in what
/// ends the block.
///
/// Every style has a terminator and that is the point. An `Args:` block
/// read without one swallows the `Returns:` description, and every word
/// of it reads as a parameter name — which is most of what the first
/// survey of this measured.
fn blocks(lines: &[&str], out: &mut Vec<Box<str>>) {
    let mut at = 0;
    while at < lines.len() {
        at = match opens_block(lines, at) {
            Some(style) => entries(lines, at, style, out),
            None => at + 1,
        };
    }
}

/// One block's entries, from its heading line to whatever closed it.
/// Returns the line the block ended on, which is where the search for
/// the next heading resumes.
fn entries(lines: &[&str], head_at: usize, style: Style, out: &mut Vec<Box<str>>) -> usize {
    let head = crate::prose::columns(lines[head_at]);
    let mut at = head_at + 1 + (style == Style::Underlined) as usize;
    let mut entry = None;
    while at < lines.len() {
        let line = lines[at];
        let (text, col) = (line.trim(), crate::prose::columns(line));
        if text.is_empty() {
            at += 1;
            continue;
        }
        if style.closes(text, col, head, lines.get(at + 1)) {
            break;
        }
        match style {
            Style::Bullets => out.extend(bullet_name(text)),
            // Every entry sits at the same column; anything deeper is
            // the previous entry's description wrapping.
            _ if *entry.get_or_insert(col) == col => out.extend(entry_names(text)),
            _ => {}
        }
        at += 1;
    }
    at
}

impl Style {
    /// Has the block ended at this line? Each style has its own
    /// terminator, and every style ends at a heading of another kind.
    fn closes(self, text: &str, col: usize, head: usize, next: Option<&&str>) -> bool {
        let ended = match self {
            Style::Bullets => is_heading(text),
            Style::Indented => col <= head,
            Style::Underlined => col < head || underlines(next),
        };
        ended || closes_block(text)
    }
}

/// Which parameter block opens at this line, if one does.
fn opens_block(lines: &[&str], at: usize) -> Option<Style> {
    let text = lines[at].trim();
    if is_heading(text) {
        let word = text.trim_start_matches('#').trim();
        return names_arguments(word).then_some(Style::Bullets);
    }
    let word = heading_word(text)?;
    if !names_arguments(word) {
        return None;
    }
    // Google's heading wears a colon; numpydoc's wears an underline.
    match underlines(lines.get(at + 1)) {
        true => Some(Style::Underlined),
        false => text.ends_with(':').then_some(Style::Indented),
    }
}

/// Does a heading of some other kind sit here? Indentation ends a block
/// on its own; this catches the docs that misindent their sections, and
/// a second `Args:` for the keyword arguments starts a block of its own
/// rather than continuing this one.
fn closes_block(text: &str) -> bool {
    heading_word(text).is_some_and(|w| {
        names_arguments(w) || END_HEADINGS.iter().any(|h| w.eq_ignore_ascii_case(h))
    })
}

/// Is this line numpydoc's underline — the row of dashes that turns the
/// line above it into a section heading?
fn underlines(line: Option<&&str>) -> bool {
    line.is_some_and(|l| {
        let text = l.trim();
        !text.is_empty() && text.chars().all(|c| c == '-')
    })
}

/// Is this a Markdown heading line?
fn is_heading(text: &str) -> bool {
    text.starts_with('#')
}

/// The heading this line spells, if it is one: a short phrase alone on
/// its line, with an optional trailing colon.
fn heading_word(text: &str) -> Option<&str> {
    let word = text.strip_suffix(':').unwrap_or(text).trim();
    let plain = !word.is_empty()
        && word.len() <= "keyword arguments".len()
        && word
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == ' ' || c == '-');
    plain.then_some(word)
}

fn names_arguments(word: &str) -> bool {
    ARG_HEADINGS.iter().any(|h| word.eq_ignore_ascii_case(h))
}

/// `- \`num_jobs\` -- lower bound on ...` under a `# Arguments`
/// heading.
///
/// A `*` bullet has already been eaten before this is reached: it is
/// indistinguishable from the `*` that decorates every continuation
/// line of a block comment, and stripping that is what lets a JSDoc
/// block be read at all. So a line that kept its bullet is one, and a
/// line that did not needs the backticked name Rust's own convention
/// writes — which the section's PROSE does not have.
fn bullet_name(text: &str) -> Option<Box<str>> {
    let rest = text
        .strip_prefix("- ")
        .or_else(|| text.strip_prefix("* "))
        .unwrap_or(text);
    let token = rest.split_whitespace().next()?;
    let quoted = token.starts_with('`') && token.trim_end_matches([':', ',']).ends_with('`');
    (rest.len() != text.len() || quoted).then(|| name_of(token))?
}

/// One entry of an indented block: Google's `name (str, optional): what
/// it is` and numpydoc's `x, y : int`.
///
/// The COLON is required. A description that wrapped onto its own line
/// carries none, and neither does the prose of a section this parser
/// failed to recognize as one — so a block that runs on names nothing
/// rather than inventing a parameter per sentence.
fn entry_names(text: &str) -> Vec<Box<str>> {
    let Some(head) = text.split(':').next().filter(|h| h.len() < text.len()) else {
        return Vec::new();
    };
    let head = head.split('(').next().unwrap_or(head);
    head.split(',').filter_map(name_of).collect()
}

/// The bare name inside a token, or None when the token is not one.
///
/// Everything decorative comes off: `$limit`, `*args`, `` `name` ``,
/// `[name=default]`, `&ref`, `**bold**`. A token carrying a DOT is
/// refused — `options.headerName` documents a member of a parameter,
/// and the parameter it belongs to is documented on its own line.
fn name_of(token: &str) -> Option<Box<str>> {
    let token = token.split('=').next().unwrap_or(token);
    let name = token.trim().trim_matches(|c| DECORATION.contains(&c));
    let shaped = !name.is_empty()
        && name.starts_with(|c: char| c.is_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !NOT_A_NAME.iter().any(|w| name.eq_ignore_ascii_case(w));
    shaped.then(|| name.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(doc: &str) -> Vec<String> {
        documented(doc, &[]).iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_jsdoc_tag_names_its_parameter_with_or_without_a_type() {
        // hono's bearerAuth, the shape that carries a type, a default
        // and a description in one line.
        assert_eq!(
            names("/**\n * @param {string} token - the bearer token\n */"),
            ["token"]
        );
        assert_eq!(
            names(
                "/**\n * @param {string | MessageFunction<E>} [options.message=\"nope\"]\n * @param options\n */"
            ),
            ["options"],
            "a nested property documents a MEMBER, not a parameter"
        );
        // junit5's Try.java, verbatim: a Javadoc tag carries no type.
        assert_eq!(
            names(" * @param action the action to try; must not be {@code null}"),
            ["action"]
        );
        // redis threads_mngr.h, verbatim: the tag opens one line and is
        // MENTIONED in the next.
        assert_eq!(
            names(" * @param tids  An array of threads.\n * The number of threads in @param tids."),
            ["tids"]
        );
        // Doxygen's direction annotation is not an optional parameter
        // called `out`.
        assert_eq!(names("/// @param[out] result the answer"), ["result"]);
        assert_eq!(names("/// \\param cur The cursor to test"), ["cur"]);
        // PHPDoc writes the type FIRST; the sigil says which is which.
        assert_eq!(names(" * @param string $limit how many"), ["limit"]);
        assert_eq!(
            names(" * @param array<string, mixed> $opts extras"),
            ["opts"]
        );
        // PHP-Parser's array shape runs over four lines and the name is
        // on none of them: nothing is claimed rather than `array`.
        assert_eq!(
            names(" * @param array{\n *   'p' => array()\n * } $subNodes x"),
            [""; 0]
        );
        // caffeine's Caffeine.java, verbatim: Javadoc spells a TYPE
        // parameter with the same tag and angle brackets.
        assert_eq!(
            names(" * @param builder the cache builder\n * @param <K> the key type"),
            ["builder"]
        );
        assert_eq!(names(" * @param $limit how many"), ["limit"]);
    }

    #[test]
    fn a_python_args_block_stops_at_the_next_section() {
        // rich/progress.py:737, verbatim. Without the terminator the
        // return description reads as parameters named Text and object.
        let doc = "\"\"\"Render the speed in iterations per second.\n\n        Args:\n            task (Task): A Task object.\n\n        Returns:\n            Text: Text object containing the task speed.\n        \"\"\"";
        assert_eq!(names(doc), ["task"]);
        // rich/console.py:1600, verbatim: four entries, each with a
        // parenthesized type and a default sentence.
        let rule = "\"\"\"Draw a line with optional centered title.\n\n        Args:\n            title (str, optional): Text to render over the rule. Defaults to \"\".\n            characters (str, optional): Character(s) to form the line. Defaults to \"─\".\n            style (str, optional): Style of line. Defaults to \"rule.line\".\n            align (str, optional): How to align the title. Defaults to \"center\".\n        \"\"\"";
        assert_eq!(names(rule), ["align", "characters", "style", "title"]);
        // A description that wraps is not a parameter.
        let wrapped = "\"\"\"Do it.\n\n    Args:\n        path: where to write, which may be\n            a directory or a file, and must exist\n        mode: how to open it\n    \"\"\"";
        assert_eq!(names(wrapped), ["mode", "path"]);
        // Splats keep their names and lose their stars.
        let splat = "\"\"\"Do it.\n\n    Args:\n        *args: positional\n        **kwargs: the rest\n    \"\"\"";
        assert_eq!(names(splat), ["args", "kwargs"]);
    }

    #[test]
    fn sphinx_and_numpydoc_name_parameters_their_own_way() {
        assert_eq!(
            names(
                "\"\"\"Fetch it.\n\n    :param url: where from\n    :param dict headers: what to send\n    :returns: the body\n    \"\"\""
            ),
            ["headers", "url"],
            "a Sphinx type sits before the name"
        );
        // numpydoc: an underline opens the block, entries are `name :
        // type`, and the next underlined heading closes it.
        let numpy = "\"\"\"Fit it.\n\n    Parameters\n    ----------\n    x, y : ndarray\n        the data\n    tol : float\n        the tolerance\n\n    Returns\n    -------\n    model : Model\n    \"\"\"";
        assert_eq!(names(numpy), ["tol", "x", "y"]);
    }

    #[test]
    fn the_remaining_four_conventions_read_their_own_syntax() {
        // Rust: rayon's sleep/mod.rs:209, verbatim — a Markdown
        // heading, a backticked bullet, and a wrapped continuation.
        let rust = "/// Signals that jobs were pushed.\n///\n/// # Parameters\n///\n/// - `num_jobs` -- lower bound on number of jobs available.\n///   We'll try to get at least one thread per job.\n";
        assert_eq!(names(rust), ["num_jobs"]);
        // A Rust section that is NOT about arguments contributes
        // nothing, and closes the one before it.
        let sections = "/// Does it.\n///\n/// # Arguments\n///\n/// * `path` - where to write\n///\n/// # Panics\n///\n/// * `never` - not a parameter\n";
        assert_eq!(names(sections), ["path"]);
        // C#: Dapper's DbGeographyHandler.cs:28, verbatim.
        let cs = "/// <summary>Configure it.</summary>\n/// <param name=\"parameter\">The parameter to configure.</param>\n/// <param name=\"value\">Parameter value.</param>\n/// <typeparam name=\"T\">not an argument</typeparam>\n";
        assert_eq!(names(cs), ["parameter", "value"]);
        // Perl POD: the SCALAR sigil is required, so Dancer2's
        // `=item %a` format-placeholder list stays out of it.
        assert_eq!(
            names(
                "=head1 NAME\n\n=over\n\n=item $path\n\nwhere to write\n\n=item $mode\n\n=back\n"
            ),
            ["mode", "path"]
        );
        assert_eq!(names("=over\n\n=item weak\n\n=item %a\n\n=back\n"), [""; 0]);
    }

    #[test]
    fn prose_that_is_not_a_convention_names_nothing() {
        assert_eq!(names("/// Returns the parsed value."), [""; 0]);
        assert_eq!(names("// the args are checked by the caller"), [""; 0]);
        // A TypeScript constructor property, whose modifiers the
        // signature carries and the doc does not.
        assert_eq!(
            names("/**\n * @param _ttlMs how long entries live\n */"),
            ["_ttlMs"],
            "a leading underscore is part of the name"
        );
        // An inherited doc claims nothing of its own.
        assert_eq!(names("/**\n * {@inheritDoc}\n */"), [""; 0]);
        assert_eq!(names("/// <inheritdoc />"), [""; 0]);
        // vscode's diff.ts, verbatim: the tag is there and the name is
        // not, so the description's first word stands where a name
        // would — and it is an article.
        assert_eq!(names(" * @param The left changes"), [""; 0]);
    }
}
