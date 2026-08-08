//! Jupyter notebooks: code cells read at TRUE file lines.
//!
//! A `.ipynb` is JSON, and the padding trick the other containers use
//! still works, because nbformat stores a cell's `source` as an ARRAY
//! OF LINES and every writer in the ecosystem pretty-prints it one
//! element per line:
//!
//! ```text
//!    "source": [
//!     "import os\n",
//!     "import sys\n"
//!    ]
//! ```
//!
//! So each Python line already occupies exactly one line of the file,
//! and the container can emit it there. Findings point at real lines in
//! the real file: `--diff` decides what a commit touched by intersecting
//! unit line ranges with git's changed-line ranges, and a synthesized
//! buffer would make every notebook finding either always or never in
//! range.
//!
//! The alternative design — concatenate the cells and report "cell 3,
//! line 8" — was rejected for that reason. It would have been fine for
//! the ratchet, which keys on qualname and treats lines as not
//! identity, and useless for everything reading git.
//!
//! Placement is verified rather than assumed. Cells come from
//! `serde_json`, positions come from a scan of the raw text, and the two
//! must agree on every line or the file is refused and counted as
//! skipped. A notebook that stores `source` as one string instead of an
//! array is the case that cannot be placed; it is refused visibly rather
//! than read at invented line numbers.

use serde_json::Value;

use crate::lang::Lang;

/// The code cells of a notebook, as a buffer of the notebook's own
/// length with everything that is not code blanked out. `None` when
/// this is not a readable notebook, when its kernel is not one of the
/// languages here, or when its lines cannot be placed at the file lines
/// that hold them.
pub fn code_of(source: &str) -> Option<(Lang, String)> {
    let doc: Value = serde_json::from_str(source).ok()?;
    let lang = kernel_language(&doc)?;
    let cells = doc.get("cells")?.as_array()?;
    // Cell sources in document order, and whether each cell is code.
    // Markdown carries no code and its lines are left blank, but it
    // still owns a `"source"` key, so it has to be walked past rather
    // than skipped.
    let mut wanted: Vec<Option<Vec<String>>> = Vec::with_capacity(cells.len());
    for cell in cells {
        let is_code = cell.get("cell_type").and_then(Value::as_str) == Some("code");
        let source = cell.get("source")?;
        match (is_code, source) {
            (true, Value::Array(lines)) => {
                let text: Option<Vec<String>> = lines
                    .iter()
                    .map(|l| l.as_str().map(str::to_owned))
                    .collect();
                wanted.push(Some(text?));
            }
            // A code cell holding ONE string cannot be placed: its
            // newlines are escaped inside a single line of the file.
            (true, _) => return None,
            (false, _) => wanted.push(None),
        }
    }
    place(source, lang, &wanted)
}

/// Walk the raw text once, emitting each code line at the file line
/// that holds it. Returns `None` unless every expected line was placed,
/// so a notebook written by something that does not pretty-print is
/// refused rather than measured at the wrong lines.
fn place(source: &str, lang: Lang, wanted: &[Option<Vec<String>>]) -> Option<(Lang, String)> {
    let mut scan = Scan {
        wanted,
        cell: 0,
        expecting: None,
    };
    let mut out = String::with_capacity(source.len());
    // Every line of the notebook gets a line of the buffer, blank
    // unless it holds code: the padding the other containers use, and
    // the reason no column moves. Separators go before each line after
    // the first and one terminator closes the buffer, because a
    // notebook's last line is `}` and pads to blank: joined with
    // separators alone, that final blank would vanish and the buffer
    // would be one line shorter than the file it describes.
    for (i, line) in source.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match scan.step(line.trim_start()) {
            Step::Code(python) => out.push_str(&python),
            Step::Blank => {}
            Step::Refuse => return None,
        }
    }
    out.push('\n');
    // Every cell has to have been reached, or the scan stopped early
    // and the tail of the notebook went unread.
    (scan.cell >= wanted.len()).then_some((lang, out))
}

/// What one raw line of the notebook contributes to the buffer.
enum Step {
    /// Scaffolding, prose, an output, or a magic: the line pads.
    Blank,
    /// Python, to be written at exactly this line.
    Code(String),
    /// The layout is not what this reads, and measuring it now would
    /// mean inventing line numbers.
    Refuse,
}

/// Where the scan is: which cell it is walking, and — when inside that
/// cell's `source` array — which line of it comes next. nbformat writes
/// `outputs` BEFORE `source`, so without the array gate a notebook that
/// prints code would place an output line.
struct Scan<'a> {
    wanted: &'a [Option<Vec<String>>],
    cell: usize,
    expecting: Option<usize>,
}

impl Scan<'_> {
    fn step(&mut self, trimmed: &str) -> Step {
        match self.expecting {
            None => self.open(trimmed),
            Some(_) if trimmed.starts_with(']') => self.close(),
            Some(at) => self.accept(trimmed, at),
        }
    }

    /// Outside an array, only a `"source"` key changes anything. An
    /// empty cell writes `"source": []`, opening and closing on one
    /// line; read as an opening brace it runs the scan a whole cell
    /// behind for the rest of the file. Ten of the eleven notebooks
    /// lost in measurement failed that way.
    fn open(&mut self, trimmed: &str) -> Step {
        if !trimmed.starts_with("\"source\"") {
            return Step::Blank;
        }
        if !trimmed.contains(']') {
            self.expecting = Some(0);
            return Step::Blank;
        }
        match self.wanted.get(self.cell) {
            Some(Some(lines)) if !lines.is_empty() => Step::Refuse,
            Some(_) => self.advance(),
            None => Step::Refuse,
        }
    }

    /// The `]` ending a cell's array: what was placed must be what the
    /// parse said the cell holds, or the two disagree about the file.
    fn close(&mut self) -> Step {
        let placed = self.expecting.take().unwrap_or_default();
        match self.wanted.get(self.cell) {
            Some(Some(lines)) if lines.len() != placed => Step::Refuse,
            Some(_) => self.advance(),
            None => Step::Refuse,
        }
    }

    /// One element of a source array, decoded and checked against the
    /// parse. A markdown cell is walked past rather than emitted. A
    /// magic counts as placed, so the verification still holds, and
    /// contributes nothing.
    fn accept(&mut self, trimmed: &str, at: usize) -> Step {
        let Some(text) = json_string(trimmed) else {
            return Step::Blank;
        };
        self.expecting = Some(at + 1);
        let Some(Some(lines)) = self.wanted.get(self.cell) else {
            return Step::Blank;
        };
        if lines.get(at).map(String::as_str) != Some(&*text) {
            return Step::Refuse;
        }
        match python_line(&text) {
            Some(python) => Step::Code(python.to_owned()),
            None => Step::Blank,
        }
    }

    fn advance(&mut self) -> Step {
        self.cell += 1;
        Step::Blank
    }
}

/// The Python in one cell line, if it is Python at all. `%%time`,
/// `%matplotlib inline` and `!pip install x` are IPython's, rewritten
/// by the front-end before anything executes, and no line of Python may
/// begin with either character, so blanking them is safe in the
/// direction that matters. Seven of the thirty-eight measured failed the
/// confidence bar and every one of them failed on a magic.
///
/// A `%` or `!` opening a line INSIDE a triple-quoted string is blanked
/// too, which breaks that string. Such a file fails to parse whether or
/// not the line is blanked, so the blanking costs nothing.
fn python_line(text: &str) -> Option<&str> {
    let line = text.trim_end_matches('\n');
    match line.trim_start().starts_with(['%', '!']) {
        true => None,
        false => Some(line),
    }
}

/// One raw line of a JSON array of strings, decoded. The trailing comma
/// is the array's, not the string's.
fn json_string(trimmed: &str) -> Option<String> {
    let body = trimmed.strip_suffix(',').unwrap_or(trimmed);
    serde_json::from_str::<String>(body).ok()
}

/// A notebook declares its language in the kernelspec. Only Python is
/// mapped: an R or Julia notebook is a real thing and reading it as
/// Python would be worse than not reading it.
fn kernel_language(doc: &Value) -> Option<Lang> {
    let meta = doc.get("metadata")?;
    let named = meta
        .get("kernelspec")
        .and_then(|k| k.get("language"))
        .and_then(Value::as_str)
        .or_else(|| {
            meta.get("language_info")
                .and_then(|l| l.get("name"))
                .and_then(Value::as_str)
        })?;
    matches!(named, "python").then_some(Lang::Python)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB: &str = r##"{
 "cells": [
  {
   "cell_type": "markdown",
   "metadata": {},
   "source": [
    "# Setup\n",
    "Loads the model."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 1,
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "text": [
      "import os\n"
     ]
    }
   ],
   "source": [
    "import os\n",
    "\n",
    "def load(path):\n",
    "    return open(path).read()"
   ]
  }
 ],
 "metadata": {
  "kernelspec": {
   "display_name": "Python 3",
   "language": "python",
   "name": "python3"
  }
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;

    #[test]
    fn code_lands_on_the_lines_that_hold_it() {
        let (lang, buf) = code_of(NB).expect("a readable notebook");
        assert_eq!(lang, Lang::Python);
        assert_eq!(
            buf.lines().count(),
            NB.lines().count(),
            "the buffer is the notebook's own length, so no column moves"
        );
        let lines: Vec<(usize, &str)> = buf
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.is_empty())
            .map(|(i, l)| (i + 1, l))
            .collect();
        // The blank line between them is cell source too, and stays blank.
        assert_eq!(
            lines,
            [
                (24, "import os"),
                (26, "def load(path):"),
                (27, "    return open(path).read()"),
            ]
        );
        // Not just self-consistent: each emitted line must be on the
        // raw line that literally spells it.
        for (row, text) in &lines {
            let raw = NB.lines().nth(row - 1).expect("row exists");
            assert!(
                raw.contains(&text.replace('\\', "\\\\")),
                "line {row} emitted {text:?} but the notebook holds {raw:?}"
            );
        }
    }

    #[test]
    fn markdown_and_outputs_are_walked_past_not_read() {
        let (_, buf) = code_of(NB).expect("a readable notebook");
        assert!(
            !buf.contains("# Setup"),
            "prose is not code: markdown cells stay blank"
        );
        // `import os` appears in an output BEFORE it appears in the
        // source; only the source copy may be placed, and its line is
        // the one asserted above.
        assert_eq!(buf.matches("import os").count(), 1, "outputs are not code");
    }

    #[test]
    fn magics_are_blanked_and_empty_cells_do_not_shift_the_rest() {
        // `"source": []` opens and closes on one line, and reading it as
        // an opening brace ran the scan a cell behind for the whole
        // file: it refused ten of the eleven notebooks that first
        // measurement lost. Magics are not Python and were the sole
        // cause of all seven parse failures that survived.
        let nb = NB.replace(
            "    \"import os\\n\",\n    \"\\n\",",
            "    \"%%time\\n\",\n    \"!pip install torch\\n\",",
        );
        let nb = nb.replace(
            "  {\n   \"cell_type\": \"markdown\",",
            "  {\n   \"cell_type\": \"code\",\n   \"source\": []\n  },\n  {\n   \"cell_type\": \"markdown\",",
        );
        let (_, buf) = code_of(&nb).expect("an empty cell must not refuse the file");
        assert_eq!(buf.lines().count(), nb.lines().count());
        assert!(
            !buf.contains("%%time") && !buf.contains("pip install"),
            "IPython magics are not Python and are blanked, not read"
        );
        // The code after them still lands on its own line, which is the
        // property the empty cell would have broken.
        let (row, text) = buf
            .lines()
            .enumerate()
            .map(|(i, l)| (i + 1, l))
            .find(|(_, l)| l.starts_with("def load"))
            .expect("the function survives");
        assert!(
            nb.lines()
                .nth(row - 1)
                .is_some_and(|raw| raw.contains(text)),
            "line {row} must still be the line that spells it"
        );
    }

    #[test]
    fn what_cannot_be_placed_is_refused_rather_than_invented() {
        // `source` as one string: the newlines are escaped inside a
        // single line of the file, so no line number is true.
        let flat = NB.replace(
            "\"source\": [\n    \"import os\\n\",\n    \"\\n\",\n    \"def load(path):\\n\",\n    \"    return open(path).read()\"\n   ]",
            "\"source\": \"import os\\ndef load(path):\\n    return open(path).read()\"",
        );
        assert!(
            flat.contains("\"source\": \"import os"),
            "the fixture edit applied"
        );
        assert!(
            code_of(&flat).is_none(),
            "a cell that cannot be placed refuses the file"
        );
        // A non-Python kernel is a real notebook this cannot read.
        let r = NB.replace("\"language\": \"python\"", "\"language\": \"R\"");
        assert!(code_of(&r).is_none(), "an R notebook is not Python");
        assert!(code_of("not json at all").is_none());
    }
}
