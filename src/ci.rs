//! The shell that provisions production, hiding inside other formats.
//!
//! A GitHub Actions `run:` block and a Dockerfile `RUN` line are shell
//! scripts. They deploy, they build, they hold credentials, and they
//! are reviewed less than any source file, because the file they live
//! in is "configuration".
//!
//! Same container contract the Vue pack established: blank everything
//! that is not shell, keep the rest where it was, and every line
//! number stays true with no offset bookkeeping anywhere downstream.
//! No YAML parser is involved: a block scalar is defined by
//! indentation, which is a line-shaped fact, and the structural parse
//! would throw away the line numbers a finding needs.
//!
//! Scope:
//! - `.github/workflows/` ONLY. A Helm chart is also YAML and belongs
//!   to `--helm`, which measures it as configuration rather than code.
//! - Dockerfile `RUN` in SHELL form. The exec form (`RUN ["a", "b"]`)
//!   is a JSON array that never reaches a shell.
//! - `ENV`/`ARG` keep their `NAME=value` bodies, which are already
//!   shell assignments, so the credential detector reads a
//!   Dockerfile's baked-in secrets for free.

/// A file whose code is written in another file's format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Container {
    /// A single-file component (Vue or Svelte) whose code lives in
    /// `<script>` blocks inside markup.
    Component,
    /// A GitHub Actions workflow: shell inside `run:` blocks.
    Workflow,
    /// A Dockerfile: shell inside `RUN`, assignments inside `ENV`.
    Dockerfile,
    /// A Jupyter notebook: code cells inside JSON.
    Notebook,
}

impl Container {
    /// What kind of container this path is, if it is one.
    pub fn of(path: &std::path::Path) -> Option<Container> {
        if path
            .extension()
            .is_some_and(|e| e == "vue" || e == "svelte")
        {
            return Some(Container::Component);
        }
        if path.extension().is_some_and(|e| e == "ipynb") {
            return Some(Container::Notebook);
        }
        let name = path.file_name()?.to_str()?;
        if name == "Dockerfile" || name.starts_with("Dockerfile.") {
            return Some(Container::Dockerfile);
        }
        let yaml = path.extension().is_some_and(|e| e == "yml" || e == "yaml");
        let display = path.to_string_lossy().replace('\\', "/");
        match yaml && display.contains(".github/workflows/") {
            true => Some(Container::Workflow),
            false => None,
        }
    }
}

/// The shell inside a workflow's `run:` blocks, everything else blank.
/// `None` when the workflow runs no shell at all: a purely
/// `uses:`-driven pipeline has nothing here to measure.
pub fn shell_of_workflow(source: &str) -> Option<String> {
    let mut kept = false;
    let mut out = String::with_capacity(source.len());
    // Indentation of the `run:` key whose block we are inside.
    let mut block: Option<usize> = None;
    for line in source.lines() {
        let indent = indent_of(line);
        let body = line.trim_start();
        // A block ends at the first line indented no deeper than the
        // key that opened it. Blank lines belong to the block.
        if block.is_some_and(|key| !body.is_empty() && indent <= key) {
            block = None;
        }
        if let Some(_key) = block {
            kept |= !body.is_empty();
            out.push_str(&without_expressions(line));
            out.push('\n');
            continue;
        }
        match run_command(body) {
            // `run: |` opens a block; the command lives below.
            Some("") => {
                block = Some(indent);
                out.push('\n');
            }
            Some(inline) => {
                kept = true;
                out.push_str(&without_expressions(inline));
                out.push('\n');
            }
            None => out.push('\n'),
        }
    }
    kept.then_some(out)
}

/// The command a `run:` key carries: `""` when it opens a block
/// scalar, the command itself when written inline, `None` when this
/// line is not a `run:` key at all.
fn run_command(body: &str) -> Option<&str> {
    let rest = body
        .strip_prefix("- run:")
        .or_else(|| body.strip_prefix("run:"))?
        .trim();
    // `|`, `|-`, `>`, `>-` and friends all open a block scalar.
    match rest.starts_with(['|', '>']) || rest.is_empty() {
        true => Some(""),
        false => Some(rest),
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// GitHub's `${{ matrix.target }}` is interpolated BEFORE a shell ever
/// sees the script, and it is not shell: bash reads `${` then `{` and
/// gives up. Left in, it makes this repository's own release workflow
/// unparseable, along with every workflow that uses a matrix, a secret
/// or an env expression, which is most of them.
///
/// Each expression becomes an underscore run of the SAME LENGTH: a
/// valid shell word, so the surrounding script parses, and no column
/// moves.
fn without_expressions(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find("${{") {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find("}}") {
            Some(close) => {
                out.extend(std::iter::repeat_n('_', close + 2));
                rest = &after[close + 2..];
            }
            // An unterminated expression is not an expression.
            None => {
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A Dockerfile's shell: `RUN` bodies in shell form, plus `ENV`/`ARG`
/// bodies, which are already `NAME=value` shell assignments.
pub fn shell_of_dockerfile(source: &str) -> Option<String> {
    let mut kept = false;
    let mut out = String::with_capacity(source.len());
    // A `\` at end of line continues the instruction onto the next.
    let mut continued = false;
    for line in source.lines() {
        let body = line.trim_start();
        let carry = continued;
        continued = line.trim_end().ends_with('\\');
        let command = match carry {
            true => Some(line),
            false => instruction_body(body),
        };
        match command {
            Some(text) => {
                kept = true;
                out.push_str(text);
                out.push('\n');
            }
            None => out.push('\n'),
        }
    }
    kept.then_some(out)
}

/// The shell body of one Dockerfile instruction, if it has one.
fn instruction_body(body: &str) -> Option<&str> {
    let (keyword, rest) = body.split_once(char::is_whitespace)?;
    match keyword.to_ascii_uppercase().as_str() {
        // The exec form is a JSON array and never reaches a shell.
        "RUN" if !rest.trim_start().starts_with('[') => Some(rest),
        "ENV" | "ARG" if rest.contains('=') => Some(rest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const WORKFLOW: &str = "name: ci\non: [push]\njobs:\n  gate:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - name: Build\n        run: |\n          REGION=\"us-east-1\"\n          deploy \"$REGION\"\n      - name: Lint\n        run: cargo clippy -- -D warnings\n";

    #[test]
    fn a_workflows_run_blocks_keep_their_true_lines() {
        let shell = shell_of_workflow(WORKFLOW).expect("has run blocks");
        // `deploy "$REGION"` is on line 11 of the real file.
        let at = shell
            .lines()
            .position(|l| l.contains("deploy"))
            .expect("kept");
        assert_eq!(at + 1, 11);
        // The inline form is kept too, at its own line.
        let at = shell
            .lines()
            .position(|l| l.contains("clippy"))
            .expect("kept");
        assert_eq!(at + 1, 13);
        // YAML keys are gone, not merely ignored.
        assert!(!shell.contains("runs-on"));
        assert!(!shell.contains("uses:"));
        assert_eq!(shell.lines().count(), WORKFLOW.lines().count());
    }

    #[test]
    fn the_shell_pack_reads_a_workflow_as_a_script() {
        let shell = shell_of_workflow(WORKFLOW).unwrap();
        let pack = crate::lang::Lang::Shell.pack();
        let mut parser = pack.make_parser();
        let f = crate::facts::extract(pack, &mut parser, Path::new("ci.yml"), &shell);
        assert!(!f.low_confidence(), "a blanked workflow still parses");
        assert!(
            f.units[0].repurposed == 0 && f.lines > 0,
            "the module scope is measured"
        );
    }

    #[test]
    fn a_github_expression_is_not_shell_and_must_not_break_the_parse() {
        // `${{ }}` is interpolated before a shell exists. Left in, it
        // makes the script unparseable: this repository's own release
        // workflow reads low-confidence, and so does every workflow
        // using a matrix, a secret or an env.
        let wf = "jobs:\n  b:\n    steps:\n      - run: |\n          BIN=target/${{ matrix.target }}/release/elegance\n          strip \"$BIN\"\n";
        let shell = shell_of_workflow(wf).expect("has a run block");
        assert!(!shell.contains("${{"), "the expression is gone");
        // Same length, so no column moves.
        assert_eq!(shell.lines().count(), wf.lines().count());
        assert_eq!(
            shell.lines().nth(4).unwrap().len(),
            wf.lines().nth(4).unwrap().len()
        );
        // The remainder is shell the pack can read.
        let pack = crate::lang::Lang::Shell.pack();
        let mut parser = pack.make_parser();
        let f = crate::facts::extract(pack, &mut parser, Path::new("ci.yml"), &shell);
        assert!(!f.low_confidence(), "the script parses now");
    }

    #[test]
    fn a_workflow_without_shell_and_a_chart_that_is_not_one() {
        assert!(
            shell_of_workflow("name: ci\njobs:\n  a:\n    steps:\n      - uses: x@v1\n").is_none(),
            "a uses-only pipeline runs no shell"
        );
        // Only .github/workflows; a Helm chart is --helm's business.
        assert_eq!(
            Container::of(Path::new("chart/values-prod.yaml")),
            None,
            "configuration is not a shell container"
        );
        assert_eq!(
            Container::of(Path::new(".github/workflows/ci.yml")),
            Some(Container::Workflow)
        );
        assert_eq!(
            Container::of(Path::new("Dockerfile.prod")),
            Some(Container::Dockerfile)
        );
    }

    #[test]
    fn a_dockerfile_keeps_run_bodies_and_env_assignments() {
        let dockerfile = "FROM rust:1 AS build\nARG VERSION=1.2.3\nENV DB_PASSWORD=r7Kq2mVx9Ttz\nRUN apt-get update \\\n && apt-get install -y curl\nRUN [\"cargo\", \"build\"]\nCOPY . /src\n";
        let shell = shell_of_dockerfile(dockerfile).expect("has RUN lines");
        assert_eq!(shell.lines().count(), dockerfile.lines().count());
        // FROM and COPY are not shell; the exec form never reaches one.
        assert!(!shell.contains("FROM"));
        assert!(!shell.contains("COPY"));
        assert!(!shell.contains("cargo"));
        // The continuation line rides along with its instruction.
        let at = shell.lines().position(|l| l.contains("install")).unwrap();
        assert_eq!(at + 1, 5);
        // ENV bodies are shell assignments, so the credential detector
        // reads a Dockerfile's baked-in secrets for free.
        let pack = crate::lang::Lang::Shell.pack();
        let mut parser = pack.make_parser();
        let f = crate::facts::extract(pack, &mut parser, Path::new("Dockerfile"), &shell);
        assert_eq!(f.secrets, vec![3], "the ENV password, at its true line");
    }
}
