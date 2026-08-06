//! The repository's measured style, as a briefing.
//!
//! Every other mode judges code that already exists. This one is read
//! BEFORE writing: an agent or a new contributor who knows the budgets
//! in force and where this codebase actually sits inside them does not
//! produce the violations in the first place. Prevention costs one
//! prompt; rejection costs a round trip.
//!
//! It must fit in a prompt, so it states positions and budgets and
//! nothing else — no offender lists, no architecture, no prose.

use std::error::Error;
use std::fmt::Write;
use std::path::PathBuf;

use crate::lang::{LANGS, Lang};
use crate::metrics::{self, METRICS};
use crate::report::{Agg, P50, P90};

/// Metrics worth briefing on: the shape rules a writer controls while
/// typing. Reports and recurrence findings are for review, not for
/// writing.
const BRIEFED: &[usize] = &[
    metrics::COGNITIVE,
    metrics::CYCLOMATIC,
    metrics::DEPTH,
    metrics::LENGTH,
    metrics::PARAMS,
    metrics::LIVE_SPAN,
    metrics::EXPR_DEPTH,
    metrics::MAGIC_NUMBERS,
];

pub fn run(roots: &[PathBuf]) -> Result<i32, Box<dyn Error>> {
    let cfg = crate::config::Config::load(&roots[0])?;
    let files = crate::collect_files(roots, &cfg);
    if files.is_empty() {
        println!("no supported source files found");
        return Ok(0);
    }
    let budgets = cfg.budgets();
    let whole = crate::scan(
        &files,
        crate::config::Layers::flat(budgets),
        false,
        crate::report::Wants::NONE,
    );
    // Percentiles must be per language or the table lies: a repository's
    // Python p90 says nothing about how to write its Rust. One scan per
    // language present — this mode runs once, not per commit.
    // One dialect decision per file — `of` reads C-family files to
    // decide, so deciding inside a per-language filter re-read them
    // once per language present.
    let mut by_lang: Vec<Vec<PathBuf>> = vec![Vec::new(); LANGS.len()];
    for path in &files {
        if let Some(lang) = Lang::of(path) {
            by_lang[lang as usize].push(path.clone());
        }
    }
    let per_lang: Vec<(Lang, Agg)> = LANGS
        .iter()
        .copied()
        .filter(|l| whole.files_of(*l) > 0)
        .map(|lang| {
            let subset = std::mem::take(&mut by_lang[lang as usize]);
            (
                lang,
                crate::scan(
                    &subset,
                    crate::config::Layers::flat(budgets),
                    false,
                    crate::report::Wants::NONE,
                ),
            )
        })
        .collect();
    print!("{}", render(&whole, per_lang, &cfg));
    Ok(0)
}

fn render(whole: &Agg, mut per_lang: Vec<(Lang, Agg)>, cfg: &crate::config::Config) -> String {
    let agg = whole;
    let mut out = String::new();
    let named: Vec<&str> = per_lang.iter().map(|(l, _)| l.name()).collect();
    let _ = writeln!(
        out,
        "# House style, measured\n\n\
         {} files, {} units, {} lines ({}). {:.0}% of units are tests.\n\n\
         Write code that lands inside these numbers. The budget column is the\n\
         CI gate; the p50 column is what this codebase actually reads like, and\n\
         is the target — code at the budget is already in the worst 1%.\n",
        agg.files,
        agg.units,
        agg.lines,
        named.join(", "),
        100.0 * agg.test_units as f64 / agg.units.max(1) as f64,
    );

    let budgets = cfg.budgets();
    for (lang, lang_agg) in &mut per_lang {
        let _ = writeln!(out, "\n## {}", lang.name());
        let _ = writeln!(out, "{:<15} {:>6} {:>6}   gate", "metric", "p50", "p90");
        for &m in BRIEFED {
            let dist = lang_agg.sorted_dist(m);
            if dist.is_empty() {
                continue;
            }
            let q = |p: f64| crate::report::quantile_of(dist, p);
            let gate = budgets.for_lang(*lang).label(m);
            let _ = writeln!(
                out,
                "{:<15} {:>6} {:>6}   {}",
                METRICS[m].name,
                q(P50),
                q(P90),
                gate
            );
        }
    }

    let _ = writeln!(
        out,
        "\n## Also enforced\n\n\
         - Every number above is measured from THIS repository, per language.\n\
         - No boolean flag parameters: split the function per value.\n\
         - Public units carry a contract comment; internal ones need not.\n\
         - Errors propagate: no empty handlers, no catch-all, no unwrap in\n  production paths.\n\
         - Assertions are expected where the logic is hard, and never count\n  toward complexity.\n\
         - Duplication is measured across the whole tree, so copy-paste is\n  found even between distant files.\n\n\
         Run `elegance --diff HEAD` before committing; it judges only what you\n\
         changed and names the refactoring for anything it flags."
    );
    out
}
