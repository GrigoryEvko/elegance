//! `elegance tidy`: clang-tidy over C++ that its Clang does not read yet.
//!
//! clang-tidy parses with the Clang it was built with, and a Clang lags
//! the language the way the tree-sitter grammar does. A contract clause
//! that Clang cannot parse is an error, and the error takes the
//! declaration with it, so every check on that declaration is silent.
//!
//! This command removes, for clang-tidy only, the constructs that its
//! Clang does not read and whose removal keeps the meaning of the
//! program: a contract clause, `contract_assert`, an annotation, a class
//! property. The removal keeps every byte offset. A virtual file system
//! overlay puts the changed text at the original path, so each
//! diagnostic names the original file, line and column. No file on disk
//! changes.
//!
//! Some constructs have no such removal, because the program depends on
//! them: reflection, a consteval block, and an expansion statement for a
//! Clang before 23. The command names how many files hold each one, and
//! clang-tidy reports each use as an error.
//!
//! A fix is not available. clang-tidy computes a fix on the changed
//! text, and a fix that touches a removed span deletes a contract.
//!
//! A clang-tidy that runs in a container sees the working directory at
//! a different path. Every path this command writes is relative to the
//! working directory, and a diagnostic that names a path the host does
//! not have is mapped back to the host file that path ends with.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::lang;

/// The prefix of the work directory. It starts with a dot, so the walk
/// that finds C++ files skips it.
const WORK_PREFIX: &str = ".elegance-tidy-";

/// The extensions of the files that Clang can read as C, C++ or CUDA.
const CXX_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "cu", "cuh", "h", "hh", "hpp", "hxx", "h++", "inl", "ipp",
    "tpp", "ixx",
];

/// The clang-tidy options that ask for a fix to be written or exported.
const FIX_OPTIONS: &[&str] = &[
    "--fix",
    "-fix",
    "--fix-errors",
    "-fix-errors",
    "--fix-notes",
    "-fix-notes",
    "--export-fixes",
    "-export-fixes",
];

/// The compiler flags whose value is the next argument. The value is
/// not a flag, and the probe for unknown flags does not send it alone.
const FLAGS_WITH_A_VALUE: &[&str] = &[
    "-I",
    "-D",
    "-U",
    "-o",
    "-x",
    "-isystem",
    "-iquote",
    "-idirafter",
    "-include",
    "-imacros",
    "-MF",
    "-MT",
    "-MQ",
];

const USAGE: &str = "\
usage: elegance tidy [--clang-tidy COMMAND] [-p BUILD] [clang-tidy options] FILES... [-- FLAGS]

  --clang-tidy COMMAND   the clang-tidy to run (default: clang-tidy)
  -p BUILD               the directory of compile_commands.json; flags that
                         this clang-tidy does not know are removed from it
  -- FLAGS               compiler flags, when no compile database is used

Every other option goes to clang-tidy as written. --fix and --export-fixes
are not available, because a fix on the changed text can delete a contract.";

/// What `elegance tidy` reads for itself, and what it sends on.
struct Options {
    clang_tidy: String,
    database: Option<PathBuf>,
    tidy_args: Vec<String>,
    /// Relative to the root.
    files: Vec<PathBuf>,
    compiler_args: Option<Vec<String>>,
}

/// Read the arguments after `tidy`. `Ok(None)` when they ask for help.
///
/// An argument that names a file under the root is a file to check. Any
/// other argument goes to clang-tidy, which lets `--checks bugprone-*`
/// keep its value in the next argument.
fn parse_options(root: &Path, args: &[String]) -> Result<Option<Options>, String> {
    let mut options = Options {
        clang_tidy: "clang-tidy".into(),
        database: None,
        tidy_args: Vec::new(),
        files: Vec::new(),
        compiler_args: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            options.compiler_args = Some(it.cloned().collect());
            break;
        }
        if let Some(fix) = FIX_OPTIONS
            .iter()
            .find(|fix| arg == *fix || arg.starts_with(&format!("{fix}=")))
        {
            return Err(format!(
                "{fix} is not available. clang-tidy computes a fix on the changed text, and a fix \
                 that touches a removed span deletes a contract. Run clang-tidy on the original \
                 text to get fixes."
            ));
        }
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--clang-tidy" => {
                options.clang_tidy = it
                    .next()
                    .ok_or("--clang-tidy needs the command to run")?
                    .clone();
            }
            "-p" => {
                let dir = it
                    .next()
                    .ok_or("-p needs the directory of compile_commands.json")?;
                options.database = Some(PathBuf::from(dir));
            }
            _ if arg.starts_with("--clang-tidy=") => {
                options.clang_tidy = arg["--clang-tidy=".len()..].to_string();
            }
            _ if arg.starts_with("-p=") => options.database = Some(PathBuf::from(&arg[3..])),
            _ if arg.starts_with('-') => options.tidy_args.push(arg.clone()),
            _ if root.join(arg).is_file() => options.files.push(relative_to(root, Path::new(arg))),
            _ => options.tidy_args.push(arg.clone()),
        }
    }
    if options.files.is_empty() {
        return Err(format!("name at least one file to check.\n\n{USAGE}"));
    }
    Ok(Some(options))
}

/// The path relative to the root when it is under the root, and the
/// path as given when it is not.
fn relative_to(root: &Path, path: &Path) -> PathBuf {
    let clean = path.strip_prefix(".").unwrap_or(path);
    match (
        std::fs::canonicalize(root),
        std::fs::canonicalize(root.join(clean)),
    ) {
        (Ok(base), Ok(full)) => full
            .strip_prefix(&base)
            .map_or_else(|_| clean.to_path_buf(), Path::to_path_buf),
        _ => clean.to_path_buf(),
    }
}

/// The major version of the Clang that `command` runs.
fn clang_major(command: &str) -> Result<u32, String> {
    let output = Command::new(command)
        .arg("--version")
        .output()
        .map_err(|error| {
            format!("cannot run {command}: {error}. Name the clang-tidy to run with --clang-tidy.")
        })?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .skip_while(|word| *word != "version")
        .nth(1)
        .and_then(|version| version.split('.').next()?.parse().ok())
        .ok_or_else(|| format!("{command} --version names no version: {}", text.trim()))
}

/// A directory under the root that holds the changed text, the overlay
/// and the filtered compile database. It is removed when the value goes
/// out of scope, on each return path of `run`.
struct WorkDir {
    /// Relative to the root, which is how every tool receives it.
    name: PathBuf,
    full: PathBuf,
}

impl WorkDir {
    fn create(root: &Path) -> Result<WorkDir, String> {
        let name = PathBuf::from(format!("{WORK_PREFIX}{}", std::process::id()));
        let full = root.join(&name);
        std::fs::create_dir_all(&full)
            .map_err(|error| format!("cannot create {}: {error}", full.display()))?;
        Ok(WorkDir { name, full })
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.full);
    }
}

/// What the removal did across the tree.
#[derive(Default)]
struct Lowered {
    /// The files whose text changed, relative to the root.
    files: Vec<PathBuf>,
    /// How many times each removal fired.
    removed: BTreeMap<&'static str, usize>,
    /// How many files hold each construct that no removal keeps the
    /// meaning of.
    gaps: BTreeMap<&'static str, usize>,
}

/// Every C, C++ and CUDA file under the root, relative to it. The walk
/// obeys `.gitignore` and skips hidden directories.
///
/// Complexity: O(n) in the files under the root.
fn cxx_files(root: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| CXX_EXTENSIONS.contains(&ext))
        })
        .filter_map(|entry| entry.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .collect()
}

/// Remove, in a copy under the work directory, what this Clang does not
/// read, for every file under the root. A header needs the removal as
/// much as a source does, because any source can include it.
fn lower_tree(root: &Path, work: &WorkDir, major: u32) -> Result<Lowered, String> {
    let mut lowered = Lowered::default();
    for rel in cxx_files(root) {
        let Ok(src) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        for name in lang::clang_gaps(&src, major) {
            *lowered.gaps.entry(name).or_default() += 1;
        }
        let Some(done) = lang::lower_for_clang(&src, major) else {
            continue;
        };
        for (name, _) in &done.rules {
            *lowered.removed.entry(name).or_default() += 1;
        }
        let dest = work.full.join("files").join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        std::fs::write(&dest, done.text)
            .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
        lowered.files.push(rel);
    }
    Ok(lowered)
}

/// The overlay that puts each changed file at its original path.
///
/// Every name is relative to the working directory of clang-tidy, and
/// every content path is relative to the overlay file, so the one file
/// serves a clang-tidy on the host and one in a container that sees the
/// directory at another path. `root-relative` needs LLVM 17, and an
/// older clang-tidy receives absolute names, which serve the host only.
fn overlay(root: &Path, files: &[PathBuf], major: u32) -> Value {
    let base = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let roots: Vec<Value> = files
        .iter()
        .map(|rel| {
            let name = if major >= 17 {
                rel.display().to_string()
            } else {
                base.join(rel).display().to_string()
            };
            json!({
                "name": name,
                "type": "file",
                "external-contents": format!("files/{}", rel.display()),
            })
        })
        .collect();
    let mut value = json!({
        "version": 0,
        "use-external-names": false,
        "overlay-relative": true,
        "roots": roots,
    });
    if major >= 17 {
        value["root-relative"] = json!("cwd");
    }
    value
}

/// The flags that could be unknown to this Clang: every flag, but not
/// the value of a flag that takes one, and not a path or a define.
fn probe_candidates<'a>(args: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        if FLAGS_WITH_A_VALUE.contains(&arg.as_str()) {
            skip_next = true;
            continue;
        }
        let names_a_path = ["-I", "-D", "-U", "-o", "-std="]
            .iter()
            .any(|prefix| arg.starts_with(prefix));
        if arg.starts_with('-') && !names_a_path && !candidates.contains(arg) {
            candidates.push(arg.clone());
        }
    }
    candidates
}

/// The flags that clang-tidy names as unknown in its output, as in
/// `error: unknown argument: '-fcontracts'`.
fn unknown_arguments(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let rest = &line[line.find("unknown argument")?..];
            let open = rest.find('\'')? + 1;
            let close = open + rest[open..].find('\'')?;
            Some(rest[open..close].to_string())
        })
        .collect()
}

/// Ask this clang-tidy which of the flags it does not know. One run on
/// an empty file names all of them, because the driver reports every
/// unknown flag before it reads the file.
fn unknown_flags(
    command: &str,
    root: &Path,
    work: &WorkDir,
    candidates: &[String],
) -> Result<BTreeSet<String>, String> {
    if candidates.is_empty() {
        return Ok(BTreeSet::new());
    }
    let probe = work.name.join("probe.cpp");
    std::fs::write(root.join(&probe), "")
        .map_err(|error| format!("cannot write {}: {error}", probe.display()))?;
    // clang-tidy stops before the compiler reads a flag when no check is
    // on, so one check stays on. The probe file is empty, and the check
    // finds nothing in it.
    let output = Command::new(command)
        .current_dir(root)
        .arg("--checks=-*,readability-braces-around-statements")
        .arg(&probe)
        .arg("--")
        .args(candidates)
        .output()
        .map_err(|error| format!("cannot run {command}: {error}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(unknown_arguments(&text))
}

/// A shell command line split into its arguments, with single quotes,
/// double quotes and backslashes read the way a POSIX shell reads them.
fn split_command(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some('"'), '\\') => current.extend(chars.next()),
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                current.extend(chars.next());
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if std::mem::take(&mut in_word) {
                    args.push(std::mem::take(&mut current));
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        args.push(current);
    }
    args
}

/// The arguments of one entry of a compile database.
fn entry_args(entry: &Value) -> Vec<String> {
    if let Some(args) = entry["arguments"].as_array() {
        return args
            .iter()
            .filter_map(|arg| arg.as_str().map(str::to_string))
            .collect();
    }
    entry["command"]
        .as_str()
        .map(split_command)
        .unwrap_or_default()
}

/// Write a copy of the compile database with the flags this clang-tidy
/// does not know removed. A GCC build names `-fcontracts` and
/// `-freflection`, and clang-tidy reports each unknown flag as an error
/// on every file it checks.
fn filter_database(
    command: &str,
    root: &Path,
    work: &WorkDir,
    database: &Path,
) -> Result<BTreeSet<String>, String> {
    let path = root.join(database).join("compile_commands.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut entries: Vec<Value> = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not a compile database: {error}", path.display()))?;
    let every: Vec<String> = entries.iter().flat_map(entry_args).collect();
    let unknown = unknown_flags(command, root, work, &probe_candidates(&every))?;
    for entry in &mut entries {
        let args: Vec<String> = entry_args(entry)
            .into_iter()
            .filter(|arg| !unknown.contains(arg))
            .collect();
        if let Some(fields) = entry.as_object_mut() {
            fields.remove("command");
            fields.insert("arguments".into(), json!(args));
        }
    }
    let dest = work.full.join("compile_commands.json");
    let body = serde_json::to_string_pretty(&entries).map_err(|error| error.to_string())?;
    std::fs::write(&dest, body)
        .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
    Ok(unknown)
}

/// The line with its leading path mapped back to the host, when a tool
/// in a container printed a path that the host does not have. The part
/// of that path that exists under the root names the same file.
fn host_path(line: &str, root: &Path) -> String {
    let Some(colon) = line.starts_with('/').then(|| line.find(':')).flatten() else {
        return line.to_string();
    };
    let path = &line[..colon];
    if Path::new(path).exists() {
        return line.to_string();
    }
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    for skip in 1..parts.len() {
        let rest = parts[skip..].join("/");
        let candidate = root.join(&rest);
        if candidate.is_file() {
            let shown = std::fs::canonicalize(&candidate).unwrap_or(candidate);
            return format!("{}{}", shown.display(), &line[colon..]);
        }
    }
    line.to_string()
}

/// Copy the lines of a stream to a sink, with their paths mapped back to
/// the host.
fn relay(from: impl Read, to: &Mutex<Box<dyn Write + Send>>, root: &Path) {
    for line in BufReader::new(from).lines().map_while(Result::ok) {
        if let Ok(mut sink) = to.lock() {
            let _ = writeln!(sink, "{}", host_path(&line, root));
        }
    }
}

/// Where the output of `run` goes: the terminal, or a buffer in a test.
pub struct Sinks {
    pub out: Arc<Mutex<Box<dyn Write + Send>>>,
    pub err: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Sinks {
    fn say(&self, message: &str) {
        if let Ok(mut sink) = self.err.lock() {
            let _ = writeln!(sink, "elegance tidy: {message}");
        }
    }
}

/// State what the removal did, and what it could not do, before
/// clang-tidy prints anything.
fn report(sinks: &Sinks, lowered: &Lowered, major: u32) {
    if lowered.removed.is_empty() {
        sinks.say(&format!("clang-tidy {major} reads every file as written."));
    } else {
        let removed: Vec<String> = lowered
            .removed
            .iter()
            .map(|(name, count)| format!("{name} ({count})"))
            .collect();
        sinks.say(&format!(
            "removed from {}, because clang-tidy {major} does not read them: {}.",
            file_count(lowered.files.len()),
            removed.join(", ")
        ));
    }
    for (name, &count) in &lowered.gaps {
        let verb = if count == 1 { "uses" } else { "use" };
        sinks.say(&format!(
            "{} {verb} {}. No removal keeps its meaning, so clang-tidy reports each use as an \
             error.",
            file_count(count),
            with_article(name)
        ));
    }
}

/// `1 file`, or `n files` for any other count.
fn file_count(count: usize) -> String {
    if count == 1 {
        "1 file".into()
    } else {
        format!("{count} files")
    }
}

/// The name of a construct with the article that English puts before
/// it. `reflection` is a mass noun and takes none.
fn with_article(name: &str) -> String {
    match name {
        "reflection" => name.to_string(),
        _ if name.starts_with(|c: char| "aeiou".contains(c)) => format!("an {name}"),
        _ => format!("a {name}"),
    }
}

/// Run `elegance tidy` in `root` with the arguments after `tidy`, and
/// return the exit code of clang-tidy.
pub fn run(root: &Path, args: &[String], sinks: &Sinks) -> Result<i32, String> {
    let Some(options) = parse_options(root, args)? else {
        if let Ok(mut sink) = sinks.out.lock() {
            let _ = writeln!(sink, "{USAGE}");
        }
        return Ok(0);
    };
    let major = clang_major(&options.clang_tidy)?;
    let work = WorkDir::create(root)?;
    let lowered = lower_tree(root, &work, major)?;
    report(sinks, &lowered, major);

    let mut command = Command::new(&options.clang_tidy);
    command.current_dir(root);
    if !lowered.files.is_empty() {
        let body = serde_json::to_string_pretty(&overlay(root, &lowered.files, major))
            .map_err(|error| error.to_string())?;
        let path = work.name.join("overlay.yaml");
        std::fs::write(root.join(&path), body)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        command.arg(format!("--vfsoverlay={}", path.display()));
    }
    command.args(&options.tidy_args);
    if let Some(database) = &options.database {
        let unknown = filter_database(&options.clang_tidy, root, &work, database)?;
        if !unknown.is_empty() {
            let names: Vec<&str> = unknown.iter().map(String::as_str).collect();
            sinks.say(&format!(
                "removed flags that clang-tidy {major} does not know: {}.",
                names.join(" ")
            ));
        }
        command.arg("-p").arg(&work.name);
    }
    command.args(&options.files);
    if let Some(args) = &options.compiler_args {
        let unknown = unknown_flags(&options.clang_tidy, root, &work, &probe_candidates(args))?;
        if !unknown.is_empty() {
            let names: Vec<&str> = unknown.iter().map(String::as_str).collect();
            sinks.say(&format!(
                "removed flags that clang-tidy {major} does not know: {}.",
                names.join(" ")
            ));
        }
        command
            .arg("--")
            .args(args.iter().filter(|arg| !unknown.contains(*arg)));
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot run {}: {error}", options.clang_tidy))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("clang-tidy gave no output streams".into());
    };
    std::thread::scope(|scope| {
        scope.spawn(|| relay(stdout, &sinks.out, root));
        scope.spawn(|| relay(stderr, &sinks.err, root));
    });
    let status = child
        .wait()
        .map_err(|error| format!("clang-tidy did not finish: {error}"))?;
    Ok(status.code().unwrap_or(1))
}

/// The entry point for `elegance tidy ARGS...`: the terminal as the
/// sink, and the exit code of clang-tidy as the result.
pub fn main(args: &[String]) -> i32 {
    let sinks = Sinks {
        out: Arc::new(Mutex::new(Box::new(std::io::stdout()))),
        err: Arc::new(Mutex::new(Box::new(std::io::stderr()))),
    };
    match run(Path::new("."), args, &sinks) {
        Ok(code) => code,
        Err(message) => {
            sinks.say(&message);
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory under the system temporary directory, removed when
    /// the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Scratch {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "elegance-tidy-test-{label}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create a scratch directory");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A sink that a test can read back.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("the buffer lock")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("the buffer lock")).into_owned()
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn a_fix_is_refused_with_the_reason() {
        let scratch = Scratch::new("fix");
        std::fs::write(scratch.0.join("a.cpp"), "int f();\n").unwrap();
        for option in ["--fix", "-fix-errors", "--export-fixes=out.yaml"] {
            let Err(message) = parse_options(&scratch.0, &strings(&[option, "a.cpp"])) else {
                panic!("{option} was accepted");
            };
            assert!(message.contains("deletes a contract"), "{message}");
        }
    }

    #[test]
    fn files_options_and_compiler_flags_go_where_they_belong() {
        let scratch = Scratch::new("options");
        std::fs::write(scratch.0.join("a.cpp"), "int f();\n").unwrap();
        let options = parse_options(
            &scratch.0,
            &strings(&[
                "--clang-tidy",
                "clang-tidy-23",
                "--checks",
                "bugprone-*",
                "a.cpp",
                "--",
                "-std=c++26",
                "-fcontracts",
            ]),
        )
        .expect("the arguments read")
        .expect("no help was asked for");
        assert_eq!(options.clang_tidy, "clang-tidy-23");
        assert_eq!(options.tidy_args, ["--checks", "bugprone-*"]);
        assert_eq!(options.files, [PathBuf::from("a.cpp")]);
        assert_eq!(
            options.compiler_args.as_deref(),
            Some(&strings(&["-std=c++26", "-fcontracts"])[..])
        );
        assert!(
            parse_options(&scratch.0, &strings(&["--help"]))
                .unwrap()
                .is_none()
        );
        assert!(
            parse_options(&scratch.0, &strings(&["--checks=*"])).is_err(),
            "no file"
        );
    }

    #[test]
    fn a_shell_command_splits_the_way_a_shell_splits_it() {
        assert_eq!(
            split_command(r#"g++ -DNAME="a b" -I'dir with space' -c x.cpp -o x\ y.o"#),
            [
                "g++",
                "-DNAME=a b",
                "-Idir with space",
                "-c",
                "x.cpp",
                "-o",
                "x y.o"
            ]
        );
    }

    #[test]
    fn an_unknown_flag_is_read_from_both_spellings_of_the_message() {
        let text = "error: unknown argument: '-fcontracts' [clang-diagnostic-error]\n\
                    error: unknown argument '-freflecton'; did you mean '-freflection'?\n\
                    warning: argument unused during compilation: '-march=native'\n";
        let unknown = unknown_arguments(text);
        assert_eq!(
            unknown.into_iter().collect::<Vec<_>>(),
            ["-fcontracts", "-freflecton"]
        );
    }

    #[test]
    fn a_value_is_not_probed_as_a_flag() {
        let args = strings(&[
            "-I",
            "include",
            "-Iother",
            "-o",
            "x.o",
            "-fcontracts",
            "-DX",
            "-std=c++26",
            "-fcontracts",
        ]);
        assert_eq!(probe_candidates(&args), ["-fcontracts"]);
    }

    #[test]
    fn a_container_path_maps_back_to_the_host_file_it_ends_with() {
        let scratch = Scratch::new("paths");
        std::fs::create_dir_all(scratch.0.join("include/x")).unwrap();
        std::fs::write(scratch.0.join("include/x/a.h"), "int f();\n").unwrap();
        let line = "/w/include/x/a.h:12:3: warning: something [check]";
        let mapped = host_path(line, &scratch.0);
        assert!(
            mapped.ends_with("include/x/a.h:12:3: warning: something [check]"),
            "{mapped}"
        );
        assert!(!mapped.starts_with("/w/"), "{mapped}");
        // A line that names no path, or a path the host has, is left alone.
        assert_eq!(
            host_path("   12 |   int f();", &scratch.0),
            "   12 |   int f();"
        );
        assert_eq!(
            host_path("/proc/self:1:1: x", &scratch.0),
            "/proc/self:1:1: x"
        );
    }

    #[test]
    fn the_overlay_names_files_relative_to_the_working_directory() {
        let value = overlay(Path::new("."), &[PathBuf::from("include/a.h")], 23);
        assert_eq!(value["root-relative"], "cwd");
        assert_eq!(value["overlay-relative"], true);
        assert_eq!(value["use-external-names"], false);
        assert_eq!(value["roots"][0]["name"], "include/a.h");
        assert_eq!(value["roots"][0]["external-contents"], "files/include/a.h");
    }

    #[test]
    fn the_work_directory_goes_when_the_run_ends() {
        let scratch = Scratch::new("work");
        let path = {
            let work = WorkDir::create(&scratch.0).expect("create");
            std::fs::write(work.full.join("probe.cpp"), "").unwrap();
            work.full.clone()
        };
        assert!(!path.exists(), "the work directory stayed");
    }

    #[test]
    fn the_report_reads_as_english_for_one_file_and_for_many() {
        let err = Buffer::default();
        let sinks = Sinks {
            out: Arc::new(Mutex::new(Box::new(Buffer::default()))),
            err: Arc::new(Mutex::new(Box::new(err.clone()))),
        };
        let lowered = Lowered {
            files: vec![PathBuf::from("a.h")],
            removed: BTreeMap::from([("contract clause", 2)]),
            gaps: BTreeMap::from([
                ("reflection", 80),
                ("consteval block", 1),
                ("expansion statement", 3),
            ]),
        };
        report(&sinks, &lowered, 22);
        let text = err.text();
        for line in [
            "removed from 1 file, because clang-tidy 22 does not read them: contract clause (2).",
            "80 files use reflection.",
            "1 file uses a consteval block.",
            "3 files use an expansion statement.",
        ] {
            assert!(text.contains(line), "{line}\n---\n{text}");
        }
    }

    #[test]
    fn clang_tidy_checks_a_file_with_a_contract_and_names_the_original_line() {
        // This test needs a clang-tidy on the PATH, and it stops without
        // a result on a machine that has none.
        if Command::new("clang-tidy")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let scratch = Scratch::new("run");
        std::fs::write(
            scratch.0.join("inc.h"),
            "#pragma once\nint g(int n) pre(n > 0);\n",
        )
        .unwrap();
        std::fs::write(
            scratch.0.join("main.cpp"),
            "#include \"inc.h\"\nint f(int n) pre(n > 0)\n{\n    int unused = 7;\n    unused = 8;\n    return g(n);\n}\n",
        )
        .unwrap();
        let out = Buffer::default();
        let err = Buffer::default();
        let sinks = Sinks {
            out: Arc::new(Mutex::new(Box::new(out.clone()))),
            err: Arc::new(Mutex::new(Box::new(err.clone()))),
        };
        let args = strings(&[
            "--checks=-*,clang-analyzer-deadcode.DeadStores",
            "main.cpp",
            "--",
            "-std=c++2c",
            "-fcontracts",
        ]);
        run(&scratch.0, &args, &sinks).expect("the run finishes");
        let (out, err) = (out.text(), err.text());
        assert!(!out.contains("clang-diagnostic-error"), "{out}\n{err}");
        assert!(!err.contains("unknown argument"), "{out}\n{err}");
        assert!(err.contains("contract clause (2)"), "{err}");
        assert!(
            out.contains("main.cpp:5:"),
            "the dead store names line 5: {out}"
        );
        let left: Vec<_> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(WORK_PREFIX))
            .collect();
        assert!(left.is_empty(), "the work directory stayed");
    }
}
