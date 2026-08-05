mod api;
mod cache;
mod calibrate;
mod ci;
mod config;
mod context;
mod coupling;
mod deps;
mod diff;
mod facts;
mod git;
mod graph;
mod helm;
mod history;
mod hotspots;
mod lang;
mod layers;
mod metrics;
mod near;
mod notebook;
mod ratchet;
#[cfg(test)]
mod recall;
mod render;
mod report;
mod rollup;
mod sem;
mod sfc;

use std::error::Error;
use std::path::PathBuf;

use rayon::prelude::*;
use tree_sitter::Parser;

use lang::{LANGS, Lang};
use report::Agg;

/// Directories that are never source: caches, vendored deps, build output.
const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    "node_modules",
    "target",
    "venv",
    ".venv",
    "dist",
    "vendor",
    "third_party",
];

struct Args {
    roots: Vec<PathBuf>,
    top: usize,
    json: bool,
    /// Derive per-language budgets from a gold corpus directory.
    calibrate: bool,
    /// `write` records the violation ledger; `check` fails on new ones.
    baseline: Option<String>,
    /// Git ref to diff against: findings only on changed units.
    diff: Option<String>,
    /// Git ref to compare the public surface against.
    api: Option<String>,
    explain: Option<(PathBuf, Option<u32>)>,
    /// Pack development aid: show parse ERROR/MISSING locations in one file.
    errors: Option<PathBuf>,
    /// Highest ladder rung that may fail a build. Teams start permissive
    /// and tighten; findings above it are reported either way.
    fail_on: u8,
    install_hook: bool,
    uninstall_hook: bool,
    /// Emit the repository's measured style for a writer to read FIRST.
    context: bool,
    /// SARIF 2.1.0, for code-scanning pipelines.
    sarif: bool,
    /// `record` appends one summary row; `show` renders the deltas.
    history: Option<String>,
    /// Rank complexity by how often it is actually edited.
    hotspots: bool,
    /// Roll findings up to directories: which part of the tree is in trouble.
    rollup: bool,
    /// The headline only: what kind of trouble, not which unit.
    brief: bool,
    /// Every section and every offender list, the pre-summary report.
    full: bool,
    /// Undeclared coupling and sole authorship, read from history.
    coupling: bool,
    /// Look inside the dependencies — the code you did not write.
    deps: bool,
    /// Configuration: values-overlay drift and credentials in YAML.
    helm: bool,
    /// Render each environment and measure the manifests that ship.
    render: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    if single_file_mode(&args)? {
        return Ok(());
    }

    standalone_modes(&args)?;

    let cfg = config::Config::load(&args.roots[0])?;

    compare_modes(&args, &cfg)?;

    let files = collect_files(&args.roots, &cfg);
    if files.is_empty() {
        println!("no supported source files found");
        return Ok(());
    }

    // Every mode that COUNTS across violations needs them all. Retaining
    // 64 per metric was silently undercounting whatever read them in
    // aggregate: `flag params` came out in 55 files where the truth was
    // 789, the worst directory was the wrong one, and tensions reported
    // 249 against a true 1107. Only `--brief` is exempt, and only because
    // it reads distributions and totals rather than the offenders.
    let complete = !args.brief;
    let layers = config::layers(&args.roots[0])?;
    // A reader of one table should know it is not one budget.
    match layers.count() {
        0 => {}
        1 => println!("1 package sets its own budgets\n"),
        n => println!("{n} packages set their own budgets\n"),
    }
    let mut agg = scan(&files, layers, complete);
    // A contract is a question about the whole graph, so it is asked
    // once the scan is done rather than per file.
    agg.graph.sort_by(|a, b| a.path.cmp(&b.path));
    agg.declared_layers = cfg.layers.len();
    agg.breaches = layers::breaches(&cfg.layers, &agg.graph);

    present(&args, &mut agg)
}

/// Modes that own the whole run: each walks what it needs, decides its
/// own exit code, and never comes back. Returns only when this
/// invocation is an ordinary scan.
fn standalone_modes(args: &Args) -> Result<(), Box<dyn Error>> {
    if args.calibrate {
        std::process::exit(calibrate::run(&args.roots)?);
    }
    if args.install_hook || args.uninstall_hook {
        hook(&args.roots[0], args.install_hook, args.fail_on)?;
        std::process::exit(0);
    }
    if args.coupling {
        std::process::exit(coupling::run(&args.roots[0], args.top)?);
    }
    if args.deps {
        std::process::exit(deps::run(&args.roots[0], args.top)?);
    }
    if args.helm {
        std::process::exit(helm::run(&args.roots[0])?);
    }
    if args.render {
        std::process::exit(render::run(&args.roots[0])?);
    }
    if args.context {
        std::process::exit(context::run(&args.roots)?);
    }
    Ok(())
}

/// Turn the measurements into whatever this invocation asked for. Modes
/// that decide a build's fate exit with their own code; the rest print.
fn present(args: &Args, agg: &mut Agg) -> Result<(), Box<dyn Error>> {
    let root = &args.roots[0];
    if let Some(mode) = &args.baseline {
        std::process::exit(ratchet::run(mode, agg, root, args.fail_on)?);
    }
    if let Some(mode) = &args.history {
        std::process::exit(history::run(mode, agg, root, args.top)?);
    }
    if args.hotspots {
        std::process::exit(hotspots::run(agg, root, args.top)?);
    }
    let rendered = if args.rollup {
        rollup::run(agg, args.top)
    } else if args.brief {
        report::render_brief(agg)
    } else if args.full {
        report::render_full(agg, args.top)
    } else if args.sarif {
        report::render_sarif(agg)
    } else if args.json {
        report::render_json(agg)
    } else {
        report::render(agg, report::ink::Ink::stdout())
    };
    print!("{rendered}");
    Ok(())
}

/// Modes that answer a question about a CHANGE rather than about the
/// tree: each reads git, each decides an exit code, and neither returns.
fn compare_modes(args: &Args, cfg: &config::Config) -> Result<(), Box<dyn Error>> {
    if let Some(reference) = &args.api {
        std::process::exit(api::run(reference, &args.roots[0])?);
    }
    if let Some(reference) = &args.diff {
        let code = diff::run(reference, &args.roots[0], cfg.budgets(), args.fail_on)?;
        std::process::exit(code);
    }
    Ok(())
}

/// Modes that read ONE file and never scan a tree. Returns whether one
/// ran, so main can stop.
fn single_file_mode(args: &Args) -> Result<bool, Box<dyn Error>> {
    if let Some((path, line)) = &args.explain {
        let source = std::fs::read_to_string(path)?;
        let (lang, text) = measurable(path, &source).ok_or("no code in this file")?;
        let pack = lang.pack();
        let f = facts::extract(pack, &mut pack.make_parser(), path, &text);
        print!("{}", report::render_explain(&f, *line));
        return Ok(true);
    }
    if let Some(path) = &args.errors {
        debug_errors(path)?;
        return Ok(true);
    }
    Ok(false)
}

/// Stack for each rayon worker.
///
/// The default 2 MiB is shared between rayon's own recursive splitter
/// and this crate's mutually recursive `walk`/`scan`. A worker that
/// steals while already deep in a split nests one split recursion inside
/// another, so by the time extraction starts the budget left is not
/// knowable from here — vscode crashed with the extractor only eighteen
/// levels down. Buying room is the cheap half of the fix; the depth
/// guard in `facts::extract` is the half that bounds the recursion.
const WORKER_STACK: usize = 32 * 1024 * 1024;

/// Parallel scan: parse, extract, measure, and merge — the whole pipeline.
fn scan(files: &[PathBuf], budgets: config::Layers, complete: bool) -> Agg {
    in_pool(|| scan_in_pool(files, budgets, complete))
}

/// Run parallel work on workers with stacks deep enough for the trees
/// real code contains. Every mode that walks a tree needs this, and
/// `--deps` needs it most: a minified bundle is the deepest tree there
/// is, and nobody minifies their own source.
fn in_pool<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(WORKER_STACK)
        .build()
        .expect("rayon pool");
    pool.install(work)
}

fn scan_in_pool(files: &[PathBuf], budgets: config::Layers, complete: bool) -> Agg {
    let make = || Agg::configured(budgets.clone(), complete);
    files
        .par_iter()
        .fold(
            || (make(), Parsers::default()),
            |(mut agg, mut parsers), path| {
                match std::fs::read_to_string(path) {
                    Ok(source) if config::is_generated(&source) => agg.generated += 1,
                    Ok(source) => match measurable(path, &source) {
                        Some((lang, text)) => {
                            let f = facts::extract(lang.pack(), parsers.get(lang), path, &text);
                            agg.add_file(&f);
                        }
                        // A container with no code in it (a Vue
                        // component that is only a template).
                        None => agg.skipped += 1,
                    },
                    Err(_) => agg.skipped += 1,
                }
                (agg, parsers)
            },
        )
        .map(|(agg, _)| agg)
        .reduce(make, Agg::merge)
}

/// The language a file should be measured as, and the source to
/// measure — which is not always the file's own text. A `.vue`
/// container yields its `<script>` blocks with everything else blanked
/// out, so the code is read as the TypeScript or JavaScript it is,
/// at line numbers that are already true.
fn measurable<'a>(
    path: &std::path::Path,
    source: &'a str,
) -> Option<(Lang, std::borrow::Cow<'a, str>)> {
    let Some(container) = ci::Container::of(path) else {
        return Lang::of_source(path, source)
            .map(|lang| (lang, std::borrow::Cow::Borrowed(source)));
    };
    let owned = |lang: Lang, text: String| (lang, std::borrow::Cow::Owned(text));
    match container {
        ci::Container::Component => sfc::script_of(source).map(|(lang, text)| owned(lang, text)),
        ci::Container::Workflow => {
            ci::shell_of_workflow(source).map(|text| owned(Lang::Shell, text))
        }
        ci::Container::Dockerfile => {
            ci::shell_of_dockerfile(source).map(|text| owned(Lang::Shell, text))
        }
        ci::Container::Notebook => notebook::code_of(source).map(|(lang, text)| owned(lang, text)),
    }
}

/// Is this file worth reading at all? True for every language's own
/// extension and for the containers that hold one.
fn analyzable(path: &std::path::Path) -> bool {
    Lang::from_path(path).is_some() || ci::Container::of(path).is_some()
}

/// One lazily-created parser per language per rayon task.
#[derive(Default)]
struct Parsers([Option<Parser>; LANGS.len()]);

impl Parsers {
    fn get(&mut self, lang: Lang) -> &mut Parser {
        self.0[lang as usize].get_or_insert_with(|| lang.pack().make_parser())
    }
}

/// Flags that answer and leave. They need no parsed state and no scan,
/// so they are handled apart from the arguments that build one.
///
/// `--version` exists because a released binary has to be able to say
/// which one it is, and this one updates itself through the Claude Code
/// plugin, so "which build is this" has a moving answer. Without it,
/// `elegance --version` scanned the working directory and replied "no
/// supported source files found" — a sentence, just not that one.
fn answers_and_exits(flag: &str) -> bool {
    match flag {
        "--help" | "-h" => println!("{USAGE}"),
        "--version" | "-V" => println!("elegance {}", env!("CARGO_PKG_VERSION")),
        _ => return false,
    }
    std::process::exit(0);
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args {
        roots: Vec::new(),
        top: 10,
        json: false,
        calibrate: false,
        baseline: None,
        diff: None,
        api: None,
        explain: None,
        errors: None,
        fail_on: ratchet::GATED_MAX_RUNG,
        install_hook: false,
        uninstall_hook: false,
        context: false,
        sarif: false,
        history: None,
        hotspots: false,
        rollup: false,
        brief: false,
        full: false,
        coupling: false,
        deps: false,
        helm: false,
        render: false,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let Some(flag) = arg.to_str() else {
            args.roots.push(PathBuf::from(&arg));
            continue;
        };
        if set_switch(&mut args, flag) {
            continue;
        }
        match flag {
            "calibrate" if args.roots.is_empty() && !args.calibrate => args.calibrate = true,
            "install-hook" if args.roots.is_empty() => args.install_hook = true,
            "uninstall-hook" if args.roots.is_empty() => args.uninstall_hook = true,
            _ if answers_and_exits(flag) => unreachable!("it exited"),
            _ if takes_value(flag) => {
                let v = it
                    .next()
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                set_valued(&mut args, flag, &v.to_string_lossy())?;
            }
            _ => args.roots.push(PathBuf::from(&arg)),
        }
    }
    if args.roots.is_empty() {
        args.roots.push(PathBuf::from("."));
    }
    Ok(args)
}

/// Flags that are simply on or off. Returns whether this was one.
fn set_switch(args: &mut Args, flag: &str) -> bool {
    let slot = match flag {
        "--json" => &mut args.json,
        "--context" => &mut args.context,
        "--sarif" => &mut args.sarif,
        "--hotspots" => &mut args.hotspots,
        "--by" => &mut args.rollup,
        "--brief" => &mut args.brief,
        "--full" => &mut args.full,
        "--coupling" => &mut args.coupling,
        "--deps" => &mut args.deps,
        "--helm" => &mut args.helm,
        "--render" => &mut args.render,
        _ => return false,
    };
    *slot = true;
    true
}

const USAGE: &str = "\
usage: elegance [paths...]                 report; defaults to .
  --version | -V                           which build this is
  --top N                                  offenders shown per metric
  --brief                                  the headline only: what kind of trouble, not which unit
  --full                                   every section and every offender list
  --json | --sarif                         machine output (schema 1 / SARIF 2.1.0)
  --explain file[:line]                    per-construct breakdown of one unit
  --context                                the repo's measured style, to read BEFORE writing
  --baseline write|check [--fail-on RUNG]  the CI ratchet
  --diff REF [--fail-on RUNG]              judge only what a change touched
  --api REF                                breaking changes to the public surface
  --history record|show                    trend ledger across runs
  --hotspots                               rank complexity by how often it is edited
  --by                                     roll findings up per directory, worst first
  --coupling                               undeclared co-change and sole authorship
  --deps                                   look inside the dependencies you did not write
  --helm                                   values-overlay drift and credentials in YAML
  --render                                 render each environment, measure what ships
  --errors FILE                            parse errors and pack drift (pack developers)

  elegance calibrate <gold-dirs...>        re-derive budgets, write calibration.toml
  elegance install-hook|uninstall-hook     pre-commit hook running --diff HEAD";

/// Flags whose meaning is the argument that follows them.
fn takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--baseline"
            | "--diff"
            | "--api"
            | "--errors"
            | "--fail-on"
            | "--top"
            | "--explain"
            | "--history"
    )
}

fn set_valued(args: &mut Args, flag: &str, value: &str) -> Result<(), Box<dyn Error>> {
    match flag {
        "--baseline" => args.baseline = Some(value.to_string()),
        "--history" => args.history = Some(value.to_string()),
        "--diff" => args.diff = Some(value.to_string()),
        "--api" => args.api = Some(value.to_string()),
        "--errors" => args.errors = Some(PathBuf::from(value)),
        "--fail-on" => args.fail_on = value.parse()?,
        "--top" => args.top = value.parse()?,
        // `file:line` where the suffix is digits, else a bare path — a
        // Windows drive letter or a colon in a filename stays a path.
        "--explain" => {
            args.explain = Some(match value.rsplit_once(':') {
                Some((path, line)) if line.chars().all(|c| c.is_ascii_digit()) => {
                    (PathBuf::from(path), Some(line.parse()?))
                }
                _ => (PathBuf::from(value), None),
            });
        }
        other => return Err(format!("unknown valued flag {other}").into()),
    }
    Ok(())
}

/// A quality tool's adoption is decided in its first five minutes, so
/// installing the gate is one command. The hook judges only what the
/// commit changed, which is the only review anyone acts on.
fn hook(root: &std::path::Path, install: bool, fail_on: u8) -> Result<(), Box<dyn Error>> {
    let path = root.join(".git/hooks/pre-commit");
    if !root.join(".git").is_dir() {
        return Err(format!("{} is not a git repository", root.display()).into());
    }
    if !install {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.contains("elegance --diff") {
            println!("no elegance hook installed at {}", path.display());
            return Ok(());
        }
        std::fs::remove_file(&path)?;
        println!("removed {}", path.display());
        return Ok(());
    }
    if path.exists() {
        return Err(format!(
            "{} already exists — chain elegance into it by hand:\n  elegance --diff HEAD --fail-on {fail_on} . || exit 1",
            path.display()
        )
        .into());
    }
    let bin = std::env::current_exe()?;
    let script = format!(
        "#!/bin/sh\n# Installed by `elegance install-hook`. Judges only what this\n# commit changes; rungs above {fail_on} are reported, not blocking.\nexec {} --diff HEAD --fail-on {fail_on} .\n",
        bin.display()
    );
    std::fs::create_dir_all(path.parent().expect("hook path has parent"))?;
    std::fs::write(&path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("installed {} (--fail-on {fail_on})", path.display());
    Ok(())
}

/// Print every parse ERROR/MISSING location with source context: the pack
/// developer's view of what a grammar cannot digest.
///
/// Reads the file the way the SCAN reads it — `measurable`, not the
/// extension — or the tool debugs a parse the scan never ran: a C++
/// header went through the C grammar here while the report counted its
/// errors under C++, and a container was "unsupported" outright.
fn debug_errors(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    use std::io::Write;
    let raw = std::fs::read_to_string(path)?;
    let (lang, text) = measurable(path, &raw).ok_or("unsupported file type")?;
    let source = text.into_owned();
    let lines: Vec<&str> = source.lines().collect();
    // Pack drift first: names the grammar no longer knows silently zero
    // whatever metric depended on them.
    for name in lang.pack().unresolved() {
        println!("{lang:?} pack drift: {name} does not exist in this grammar");
    }
    let tree = lang
        .pack()
        .make_parser()
        .parse(&source, None)
        .ok_or("parse returned nothing")?;
    let mut out = std::io::stdout().lock();
    let mut stack = vec![tree.root_node()];
    let mut count = 0;
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            count += 1;
            let row = node.start_position().row;
            let what = if node.is_missing() {
                "MISSING"
            } else {
                "ERROR"
            };
            let _ = writeln!(
                out,
                "{}:{}  {} [{}]\n    {}",
                path.display(),
                row + 1,
                what,
                node.kind(),
                lines.get(row).unwrap_or(&"").trim()
            );
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    let _ = writeln!(out, "{count} error nodes");
    Ok(())
}

/// Collect analyzable files, honoring .gitignore, built-in and configured
/// junk-directory names, and configured exclude globs.
fn collect_files(roots: &[PathBuf], cfg: &config::Config) -> Vec<PathBuf> {
    let mut walker = ignore::WalkBuilder::new(&roots[0]);
    for root in &roots[1..] {
        walker.add(root);
    }
    if !cfg.exclude.is_empty() {
        let mut overrides = ignore::overrides::OverrideBuilder::new(&roots[0]);
        for glob in &cfg.exclude {
            // Overrides whitelist by default; a leading ! makes them exclude.
            let _ = overrides.add(&format!("!{glob}"));
        }
        if let Ok(built) = overrides.build() {
            walker.overrides(built);
        }
    }
    let skip: Vec<String> = SKIP_DIRS
        .iter()
        .map(|s| s.to_string())
        .chain(cfg.skip_dirs.iter().cloned())
        .collect();
    walker.filter_entry(move |entry| {
        entry
            .file_name()
            .to_str()
            .is_none_or(|name| !skip.iter().any(|s| s == name))
    });
    let mut files: Vec<PathBuf> = walker
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|p| analyzable(p))
        .collect();
    // Overlapping roots (`elegance . src`) must not double-count files.
    files.sort_unstable();
    files.dedup();
    files
}
