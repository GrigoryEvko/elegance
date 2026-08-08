//! Derive per-language budgets from the gold corpus: budgets stop being
//! hand-set constants and become percentile pins against admired code.
//! One-sided budgets take gold p99; bands take p05/p95; policy metrics
//! (flag params, asserts) are taste and never calibrated.

use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::lang::{LANGS, Lang};
use crate::metrics::{Calib, Fmt, LangBudgets, METRICS};
use crate::report::Agg;

/// Below this many samples a percentile is noise; defaults stay.
const MIN_SAMPLES: usize = 200;

/// Where the corpus pins each budget: one-sided budgets take the gold
/// tail, bands take both flanks.
const GOLD_PIN: f64 = 0.99;
const BAND_LO: f64 = 0.05;
const BAND_HI: f64 = 0.95;

/// Languages that collect INCIDENTALLY: their files count wherever they
/// are found, whatever the checkout was fetched for. Shell is the only
/// one, and it is a fact about shell rather than a convenience: 207 of
/// its 342 corpus files are build and CI scripts living inside the
/// other checkouts, and gold.toml defends that as how shell genuinely
/// appears in the world.
const INCIDENTAL: &[Lang] = &[Lang::Shell];

/// May this file speak for this language? A corpus laid out as
/// `<root>/<lang>/<repo>` DECLARES what each repository was fetched
/// for, and calibration has to respect the declaration or a repo chosen
/// for one language quietly rewrites another's budgets. Unscoped, five
/// CUDA repositories move 46 budgets in four other languages — Python's
/// length p99 from 80 to 252 — because a CUDA repository is a Python
/// and C++ monorepo with kernels inside.
///
/// It also enforces gold.toml's four-per-language rule. Unscoped, tsx
/// meets that rule on paper only: two repos of its own, the rest of its
/// evidence borrowed from the ts checkouts.
///
/// Only when the layout is recognisable. Someone calibrating their own
/// tree has no such directories and keeps the pooled behaviour, which
/// is the right answer there: in a real repository every file is the
/// project's own.
fn scoped(roots: &[PathBuf], path: &Path, lang: Lang, declared: bool) -> bool {
    if !declared || INCIDENTAL.contains(&lang) {
        return true;
    }
    roots.iter().any(|root| {
        path.strip_prefix(root)
            .ok()
            .and_then(|rest| rest.components().next())
            .is_some_and(|first| first.as_os_str() == lang.corpus_dir())
    })
}

/// Does this corpus name its languages? Is there a `py/` or a `cpp/`
/// directly under a root?
fn declares_languages(roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| names_a_language(root))
}

fn names_a_language(root: &Path) -> bool {
    LANGS.iter().any(|l| root.join(l.corpus_dir()).is_dir())
}

/// May this run replace the committed snapshot?
///
/// A corpus that measured nothing is a corpus that was not there.
/// Writing the snapshot anyway replaces every budget with silence: the
/// file still parses, the build still passes, and all 22 languages fall
/// back to defaults nobody chose. The usual cause is a path that does
/// not exist yet: /tmp is cleared on reboot and takes the corpus with
/// it.
///
/// A PARTIAL corpus is the same failure wearing fewer clothes: half a
/// fetch narrows the table quietly, and the languages that dropped out
/// revert to defaults while the ones that remain look freshly measured.
/// Removing a language is a deliberate act, so it is spelled out rather
/// than inferred.
fn may_overwrite(fresh: &str, roots: &[PathBuf]) -> Result<(), Box<dyn Error>> {
    let measured = section_count(fresh);
    if measured == 0 {
        let where_ = roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "no measurable source under {where_} — calibration.toml left alone. \
             Fetch the corpus first: ./gold-fetch.sh <dir>"
        )
        .into());
    }
    let held = std::fs::read_to_string("calibration.toml").unwrap_or_default();
    let before = section_count(&held);
    if measured < before {
        return Err(format!(
            "corpus covers {measured} languages, calibration.toml holds {before} — \
             left alone. A partial corpus reverts the missing languages to \
             defaults without saying so. Fetch the rest, or delete \
             calibration.toml to accept the narrowing."
        )
        .into());
    }
    Ok(())
}

/// `[py]`-style language headers in a snapshot: how much of the table
/// a run actually measured.
fn section_count(toml: &str) -> usize {
    toml.lines()
        .filter(|l| l.starts_with('[') && l.ends_with(']'))
        .count()
}

/// Scans the corpus, prints the derived table, and writes
/// calibration.toml next to the current directory. The snapshot is baked
/// in at the NEXT build, so commit it.
pub fn run(roots: &[PathBuf]) -> Result<i32, Box<dyn Error>> {
    let cfg = crate::config::Config::default();
    let files = crate::collect_files(roots, &cfg);
    let mut unpoliced: Vec<String> = Vec::new();
    let mut out = String::from(
        "# Generated by `elegance calibrate` from the gold.toml corpus — do not\n\
         # hand-edit. Regenerate: ./gold-fetch.sh DIR && elegance calibrate DIR\n\
         # then rebuild (the snapshot is compiled in).\n",
    );

    let declared = declares_languages(roots);
    // `of`, not `from_path`: a C++ header is a `.h`, and pooling it into
    // the `[c]` section would calibrate one language on another's code.
    // Partitioned ONCE, because `of` reads a C-family file to decide its
    // dialect, and deciding inside a per-language filter would read each
    // of them twelve times over: 27,000 reads of musl alone.
    let mut by_lang: Vec<Vec<PathBuf>> = vec![Vec::new(); LANGS.len()];
    for path in &files {
        if let Some(lang) = Lang::of(path) {
            by_lang[lang as usize].push(path.clone());
        }
    }
    for lang in LANGS {
        let subset: Vec<PathBuf> = std::mem::take(&mut by_lang[lang as usize])
            .into_iter()
            .filter(|p| scoped(roots, p, lang, declared))
            .collect();
        if subset.is_empty() {
            continue;
        }
        // Calibration reads distributions and nothing else, so none of
        // the corpus-sized accumulators are built. On the gold corpus
        // that is a 777,638-entry clone map not allocated.
        let mut agg = crate::scan(
            &subset,
            crate::config::Layers::flat(LangBudgets::defaults()),
            false,
            crate::report::Wants::NONE,
        );
        let section = lang_section(&mut agg, lang);
        if !section.is_empty() {
            let _ = write!(out, "\n[{}]\n{section}", lang.name());
        }
        audit_policy(&agg, lang, &mut unpoliced);
        report_comment_roles(&agg, lang);
    }

    may_overwrite(&out, roots)?;
    std::fs::write("calibration.toml", &out)?;
    println!("\nwrote calibration.toml — rebuild to bake it in");
    // A policy the admired corpus fails is a wrong policy, the argument
    // that moved `demeter` off Calib::Policy. Reported, not gated:
    // calibration itself succeeded, and only the ratchet and diff modes
    // may fail a build.
    if !unpoliced.is_empty() {
        println!(
            "\nPOLICY DEBT — gating budgets the gold corpus itself violates (over {:.0}%):",
            POLICY_CEILING * 100.0
        );
        for line in &unpoliced {
            println!("  {line}");
        }
        println!("  each is a detector to fix, an exemption to add, or a rung to demote.");
    }
    Ok(0)
}

/// One language's calibration entries: pinned budgets first, then the
/// coverage rates for the rate metrics, each echoed to the console.
fn lang_section(agg: &mut Agg, lang: Lang) -> String {
    let mut section = String::new();
    for (m, def) in METRICS.iter().enumerate() {
        // Doc length is pinned per ROLE, with a pooled fallback the
        // thin cells inherit. A loop over one metric at a time cannot
        // see the pool.
        if crate::metrics::DOC_LENGTH.contains(&m) {
            continue;
        }
        let dist = agg.sorted_dist(m);
        let runs = dist.len();
        let Some(pin) = pinned(def, dist) else {
            continue;
        };
        if let Some(note) = pin.note {
            let _ = writeln!(section, "{note}");
        }
        let _ = writeln!(section, "\"{}\" = {{ {} }}", def.name, pin.entry);
        println!(
            "{:<5} {:<15} n={runs:<7} gold {}",
            lang.name(),
            def.name,
            pin.shown
        );
    }
    section.push_str(&doc_entries(agg, lang));
    section.push_str(&rate_entries(agg, lang));
    section
}

/// What the corpus says about one metric: the budget, whatever has to
/// be said above it, and the value to echo.
struct Pin {
    note: Option<&'static str>,
    entry: String,
    shown: String,
}

/// One metric's budget as the corpus sets it, or None where the corpus
/// cannot speak for it: fewer samples than a percentile needs, a
/// POLICY no percentile may legitimize, or a p99 of zero. A p99 of zero
/// means the fact is not extracted for this language (Zig live spans
/// without def sites), and pinning hi=0 would gate everything the day
/// it is.
fn pinned(def: &crate::metrics::MetricDef, dist: &[f32]) -> Option<Pin> {
    if dist.len() < MIN_SAMPLES || def.calib == Calib::Policy {
        return None;
    }
    let q = |p: f64| crate::report::quantile(dist, p) as f64;
    // A floor of zero can never fire, so the flank is vacuous by the
    // corpus's own verdict, and the snapshot says so above the entry.
    let vacuous = |text| (q(BAND_LO) == 0.0).then_some(text);
    match def.calib {
        Calib::P99 if q(GOLD_PIN) == 0.0 => None,
        Calib::P99 => Some(Pin {
            note: None,
            entry: format!("hi = {:.1}", q(GOLD_PIN).ceil()),
            shown: format!("{:.0}", q(GOLD_PIN)),
        }),
        Calib::Band => Some(Pin {
            note: vacuous("# gold p05 is zero: the low flank is vacuous by the corpus's verdict"),
            entry: format!("lo = {:.2}, hi = {:.2}", q(BAND_LO), q(BAND_HI)),
            shown: match def.fmt {
                Fmt::Int => format!("{:.0}-{:.0}", q(BAND_LO), q(BAND_HI)),
                Fmt::Pct => format!("{:.0}%-{:.0}%", q(BAND_LO) * 100.0, q(BAND_HI) * 100.0),
            },
        }),
        Calib::Policy => None,
    }
}

/// Doc-length budgets, one per comment role, with the thin cells
/// marked.
///
/// A role too thin to hold a percentile INHERITS this language's
/// pooled doc p99: Solidity writes 44 module headers in all of gold,
/// two orders below what a p99 needs. The entry says so on the line
/// above it, because a silently borrowed budget reads like a measured
/// one.
///
/// Where even the POOL is thin nothing is written at all and the
/// compiled default stands, which `is_pinned` already renders as a
/// trailing `.` on the budget.
fn doc_entries(agg: &mut Agg, lang: Lang) -> String {
    let mut pooled: Vec<f32> = Vec::new();
    let mut own: Vec<(usize, Option<f32>)> = Vec::new();
    for m in crate::metrics::DOC_LENGTH {
        let dist = agg.sorted_dist(m);
        // `None` IS the borrow: a role with too few runs has no
        // percentile of its own, and asking for one of an empty
        // distribution is a panic rather than a zero.
        let measured = (dist.len() >= MIN_SAMPLES).then(|| crate::report::quantile(dist, GOLD_PIN));
        own.push((dist.len(), measured));
        pooled.extend_from_slice(dist);
    }
    if pooled.len() < MIN_SAMPLES {
        return String::new();
    }
    pooled.sort_unstable_by(f32::total_cmp);
    let inherited = crate::report::quantile(&pooled, GOLD_PIN);
    let mut section = String::new();
    for (m, (runs, measured)) in crate::metrics::DOC_LENGTH.into_iter().zip(own) {
        let borrowed = measured.is_none();
        let hi = (measured.unwrap_or(inherited) as f64).ceil();
        // A zero p99 pins a budget nothing can satisfy, the same trap
        // the unpinned-fact rule avoids for every other metric.
        if hi == 0.0 {
            continue;
        }
        if borrowed {
            let _ = writeln!(
                section,
                "# {runs} runs is under the {MIN_SAMPLES} a percentile needs: \
                 borrowed from {}'s pooled doc p99",
                lang.name()
            );
        }
        let _ = writeln!(section, "\"{}\" = {{ hi = {hi:.1} }}", METRICS[m].name);
        println!(
            "{:<5} {:<15} n={:<7} gold {hi:.0}{}",
            lang.name(),
            METRICS[m].name,
            runs,
            if borrowed { " (borrowed)" } else { "" },
        );
    }
    section
}

/// Gold's coverage share for the rate metrics, baked so that a repo's
/// own rate renders beside the admired reference. Separate from the
/// budget loop because it answers a different question: how much of
/// admired code bothers, rather than how much is too much.
fn rate_entries(agg: &mut Agg, lang: Lang) -> String {
    let mut section = String::new();
    for m in crate::metrics::RATE_METRICS {
        let dist = agg.sorted_dist(m);
        if dist.len() < MIN_SAMPLES {
            continue;
        }
        let covered = dist.iter().filter(|v| **v > 0.0).count();
        let rate = covered as f64 / dist.len() as f64;
        let _ = writeln!(section, "\"{}\" = {{ rate = {rate:.2} }}", METRICS[m].name);
        println!(
            "{:<5} {:<15} n={:<7} gold covered {:.0}%",
            lang.name(),
            METRICS[m].name,
            dist.len(),
            rate * 100.0
        );
    }
    section
}

/// What each comment ROLE looks like in this corpus, printed and not
/// pinned.
///
/// Nothing is calibrated from these tallies yet; doc length and ground
/// density are the metrics that will be. The distribution has to be
/// readable before a budget can be argued about, and a corpus scan is
/// the only place it can be read. Roles with too few runs to mean
/// anything stay quiet.
fn report_comment_roles(agg: &Agg, lang: Lang) {
    for (role, tally) in crate::facts::CommentRole::ALL
        .iter()
        .zip(agg.comment_roles(lang))
    {
        if (tally.runs as usize) < MIN_SAMPLES {
            continue;
        }
        println!(
            "{:<5} comment:{:<8} n={:<7} words {:.1}  sentences {:.1}  grounds {:.2}  purposes {:.2}",
            lang.name(),
            role.name(),
            tally.runs,
            tally.per_run(tally.words),
            tally.per_run(tally.sentences),
            tally.per_run(tally.grounds),
            tally.per_run(tally.purposes),
        );
    }
}

/// Percentiles never touch `Calib::Policy` budgets, so nothing catches a
/// policy that over-fires. Measure them anyway: a gating policy metric
/// that admired code fails is measuring taste, not quality.
const POLICY_CEILING: f64 = 0.01;

/// A suspicion may cry louder than a gate, but a rung-3/4 metric that
/// admired code fails this often is still measuring taste, the same
/// argument that turned `asserts` and `public docs` into rates.
const SUSPICION_CEILING: f64 = 0.05;

fn audit_policy(agg: &Agg, lang: Lang, unpoliced: &mut Vec<String>) {
    for (m, def) in METRICS.iter().enumerate() {
        let rate = agg.violation_rate(m);
        let ceiling = match def.rung {
            0..=2 => POLICY_CEILING,
            3..=4 => SUSPICION_CEILING,
            _ => continue,
        };
        if def.calib != Calib::Policy || rate <= ceiling {
            continue;
        }
        unpoliced.push(format!(
            "{:<5} {:<15} rung {}  gold violates {:.1}% (ceiling {:.0}%)",
            lang.name(),
            def.name,
            def.rung,
            rate * 100.0,
            ceiling * 100.0
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::section_count;

    #[test]
    fn a_snapshot_is_measured_by_the_languages_it_names() {
        // The guard that decides whether a calibration run may overwrite
        // the committed budgets, so it has to count what a real snapshot
        // looks like rather than what a tidy one does.
        assert_eq!(section_count(""), 0);
        assert_eq!(section_count("# only a header\n"), 0);
        assert_eq!(
            section_count(
                "# header\n\n[py]\n\"cognitive\" = { hi = 18.0 }\n\n[rs]\n\"length\" = { hi = 93.0 }\n"
            ),
            2,
        );
        // A commented-out section is prose, and an entry is not a header.
        assert_eq!(section_count("# [py]\n\"cognitive\" = { hi = 1.0 }\n"), 0);
    }
}
