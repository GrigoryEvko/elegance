//! Machine-readable report. The schema is versioned and deliberately
//! decoupled from internal types: internals may evolve, a published
//! schema may only be added to under a new version.

use serde::Serialize;

use super::{Agg, quantile, select_clones, select_clumps, select_switches, worse};
use crate::lang::LANGS;
use crate::metrics::METRICS;

/// 2 added per-language units and per-language violation counts per
/// metric. Before it, `languages` carried file counts alone — and every
/// metric in this tool is per-unit, so no correct per-language rate
/// could be derived from what this printed.
pub const SCHEMA_VERSION: u32 = 2;

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u32,
    files: u32,
    units: u64,
    lines: u64,
    skipped_files: u32,
    generated_files: u32,
    parse_error_files: u32,
    /// Files whose parse quality disqualified their metrics (excluded from
    /// distributions, violations, and clones).
    low_confidence_files: Vec<String>,
    /// Per language: files, units, and what each metric measured and
    /// violated there. A rate compared across corpora needs a
    /// per-language denominator on BOTH sides, and these are it.
    languages: Vec<LangOut>,
    metrics: Vec<Metric>,
    violations: Vec<Violation<'a>>,
    clones: Clones,
    /// Recurring parameter groups (Fowler's Data Clumps).
    clumps: Vec<ClumpOut>,
    /// Case-label sets switched on in >=3 places (repeated dispatch).
    repeated_dispatch: Vec<ClumpOut>,
    /// Anonymous record shapes built in >=3 places — a type nobody
    /// declared, so nothing checks a typo in its keys.
    undeclared_shapes: Vec<ClumpOut>,
    /// Over-budget units no test mentions (name association, not coverage).
    untested_complexity: Vec<Untested>,
    /// Rung-5 dependency-structure report (absent when no internal edges).
    #[serde(skip_serializing_if = "Option::is_none")]
    architecture: Option<ArchitectureOut>,
    /// Step-down reading order per language (report-only; ecosystems
    /// legitimately differ).
    narrative: Vec<NarrativeOut>,
    /// Coverage rates (public docs, asserts) beside gold's own rate —
    /// the claims demoted from per-unit suspicions.
    coverage_rates: Vec<RateOut>,
    /// Edited copies the Merkle detector cannot see (winnowed overlap).
    near_clones: NearOut,
    /// Counts per verdict class — deliberately not a score.
    summary: SummaryOut,
}

#[derive(Serialize)]
struct NearOut {
    pairs: Vec<NearPairOut>,
    /// Fingerprint cores in more units than the idiom cap — unpaired
    /// for cost, counted for honesty (possible mass duplication).
    suppressed_cores: u32,
    widest_core: u32,
}

#[derive(Serialize)]
struct NearPairOut {
    a: String,
    b: String,
    overlap_pct: f64,
}

#[derive(Serialize)]
struct RateOut {
    metric: &'static str,
    lang: &'static str,
    covered: u64,
    total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    gold_rate: Option<f32>,
}

#[derive(Serialize)]
struct SummaryOut {
    gate_violations: u64,
    suspicions: u64,
    gate_drivers: Vec<DriverOut>,
    suspicion_drivers: Vec<DriverOut>,
    duplicated_pct: f64,
    test_unit_share_pct: f64,
}

#[derive(Serialize)]
struct DriverOut {
    metric: &'static str,
    violations: u64,
}

#[derive(Serialize)]
struct NarrativeOut {
    lang: &'static str,
    step_down_pct: f64,
    intra_file_refs: u64,
    public_first_pct: f64,
    public_private_pairs: u64,
}

#[derive(Serialize)]
struct ArchitectureOut {
    modules: u32,
    edges: u32,
    imports_internal: u32,
    imports_external: u32,
    imports_unresolved: u32,
    cycle_mass_pct: f64,
    largest_cycle_size: u32,
    largest_cycle: Vec<String>,
    dir_cycle_mass_pct: f64,
    largest_dir_cycle_size: u32,
    largest_dir_cycle: Vec<String>,
    depth_p50: u32,
    depth_p90: u32,
    depth_max: u32,
    deletable_pct: f64,
    load_bearing: Vec<LoadBearing>,
    orphan_count: u32,
    /// Capped sample; orphan_count is the truth.
    orphans: Vec<String>,
    /// Median mass-per-surface-unit over exporting modules.
    interface_median_depth: u32,
    shallow_modules: Vec<ShallowModule>,
    fat_surfaces: Vec<FatSurface>,
    leak_count: u32,
    leaks: Vec<String>,
    /// Exports no other file mentions. A library's public API is
    /// legitimately unreferenced here, so this is a report, never a gate.
    dead_export_count: u32,
    /// Capped sample; dead_export_count is the truth.
    dead_exports: Vec<String>,
}

#[derive(Serialize)]
struct ShallowModule {
    path: String,
    depth: u32,
    mass: u32,
    surface_cost: u32,
}

#[derive(Serialize)]
struct FatSurface {
    path: String,
    exports_imported: u32,
    exports: u32,
}

#[derive(Serialize)]
struct LoadBearing {
    path: String,
    dependents: u32,
}

#[derive(Serialize)]
struct Untested {
    unit: String,
    cyclomatic: u32,
}

#[derive(Serialize)]
struct ClumpOut {
    names: Vec<String>,
    count: u32,
    sites: Vec<String>,
}

#[derive(Serialize)]
struct LangOut {
    lang: &'static str,
    files: u32,
    /// Units measured in this language; the top-level `units` is their
    /// sum. Module scopes are excluded, exactly as they are there.
    units: u64,
    /// Metrics this language measured at least once, in registry order.
    metrics: Vec<LangMetric>,
}

/// What one metric did in one language. `measured` is the honest
/// denominator: it counts the measurements that metric actually made
/// there, which for most metrics is narrower than the language's unit
/// count — module scopes, test bodies and untyped languages are skipped
/// by different metrics for different reasons.
#[derive(Serialize)]
struct LangMetric {
    name: &'static str,
    measured: u64,
    violations: u64,
}

#[derive(Serialize)]
struct Metric {
    name: &'static str,
    rung: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    lo: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hi: Option<f32>,
    /// `"pinned"` when this budget rests on a percentile of the gold
    /// corpus, `"default"` when it rests on the compiled-in constant
    /// because the corpus was too thin, the metric is a policy, or its
    /// gold p99 was zero.
    budget_source: &'static str,
    n: usize,
    p50: f32,
    p90: f32,
    p99: f32,
    max: f32,
    violate_pct: f64,
}

#[derive(Serialize)]
struct Violation<'a> {
    metric: &'static str,
    path: &'a str,
    line: u32,
    unit: &'a str,
    value: f32,
}

#[derive(Serialize)]
struct Clones {
    duplicated_pct: f64,
    /// Duplication outside test files, where it actually costs.
    production_duplicated_pct: f64,
    /// Classes with sites on both sides: production logic copied into a
    /// test, so the test no longer exercises the original.
    straddling_classes: u32,
    classes: Vec<CloneClassOut>,
}

#[derive(Serialize)]
struct CloneClassOut {
    mass: u32,
    sites: Vec<Site>,
}

#[derive(Serialize)]
struct Site {
    path: String,
    line: u32,
    end_line: u32,
}

fn architecture_out(a: crate::graph::metrics::Architecture) -> ArchitectureOut {
    let iface = a.interfaces;
    ArchitectureOut {
        modules: a.modules,
        edges: a.edges,
        imports_internal: a.resolution.internal,
        imports_external: a.resolution.external,
        imports_unresolved: a.resolution.unresolved,
        cycle_mass_pct: a.cycle_mass_pct,
        largest_cycle_size: a.largest_cycle_size,
        largest_cycle: a.largest_cycle,
        dir_cycle_mass_pct: a.dir_cycle_mass_pct,
        largest_dir_cycle_size: a.largest_dir_cycle_size,
        largest_dir_cycle: a.largest_dir_cycle,
        depth_p50: a.depth_p50,
        depth_p90: a.depth_p90,
        depth_max: a.depth_max,
        deletable_pct: a.deletable_pct,
        load_bearing: a
            .load_bearing
            .into_iter()
            .map(|(path, dependents)| LoadBearing { path, dependents })
            .collect(),
        orphan_count: a.orphan_count,
        orphans: a.orphans,
        interface_median_depth: iface.median_depth,
        shallow_modules: iface
            .shallow
            .into_iter()
            .map(|(path, depth, mass, surface_cost)| ShallowModule {
                path,
                depth,
                mass,
                surface_cost,
            })
            .collect(),
        fat_surfaces: iface
            .fat
            .into_iter()
            .map(|(path, exports_imported, exports)| FatSurface {
                path,
                exports_imported,
                exports,
            })
            .collect(),
        leak_count: iface.leak_count,
        leaks: iface.leaks,
        dead_export_count: iface.dead_count,
        dead_exports: iface.dead,
    }
}

/// Clone classes with the duplication split that goes beside them.
fn clone_section(agg: &mut Agg) -> (Clones, super::Duplication) {
    let (kept, dup) = select_clones(agg);
    let classes = kept
        .into_iter()
        .map(|c| CloneClassOut {
            mass: c.mass,
            sites: c
                .sites
                .into_iter()
                .map(|s| Site {
                    path: s.path.to_string(),
                    line: s.line,
                    end_line: s.end_line,
                })
                .collect(),
        })
        .collect();
    let clones = Clones {
        duplicated_pct: dup.all_pct,
        production_duplicated_pct: dup.production_pct,
        straddling_classes: dup.straddling,
        classes,
    };
    (clones, dup)
}

/// Per-metric distribution rows, in registry order, skipping metrics
/// this run never measured.
fn metric_rows(agg: &mut Agg) -> Vec<Metric> {
    let mut metrics = Vec::new();
    for (m, def) in METRICS.iter().enumerate() {
        if agg.dists[m].is_empty() {
            continue;
        }
        agg.dists[m].sort_unstable_by(f32::total_cmp);
        let dist = &agg.dists[m];
        // Budgets are per-language; emit the band only when uniform across
        // the languages present in this run.
        let (lo, hi) = agg.uniform_budget(m).unwrap_or((None, None));
        metrics.push(Metric {
            name: def.name,
            rung: def.rung,
            lo,
            hi,
            budget_source: agg.budget_source(m),
            n: dist.len(),
            p50: quantile(dist, super::P50),
            p90: quantile(dist, super::P90),
            p99: quantile(dist, super::P99),
            max: *dist.last().expect("non-empty"),
            violate_pct: 100.0 * agg.violations_n[m] as f64 / dist.len() as f64,
        });
    }
    metrics
}

/// The recurrence sections: clumps, repeated dispatch, undeclared shapes.
fn recurrences_out(agg: &Agg) -> (Vec<ClumpOut>, Vec<ClumpOut>, Vec<ClumpOut>) {
    let as_out = |c: super::SelectedClump| ClumpOut {
        names: c.names,
        count: c.count,
        sites: c.sites,
    };
    (
        select_clumps(agg).into_iter().map(as_out).collect(),
        select_switches(agg).into_iter().map(as_out).collect(),
        super::select_shapes(agg).into_iter().map(as_out).collect(),
    )
}

fn summary_out(agg: &Agg, dup: super::Duplication) -> SummaryOut {
    let v = super::verdict(agg);
    let drivers = |ds: Vec<(&'static str, u64)>| {
        ds.into_iter()
            .map(|(metric, violations)| DriverOut { metric, violations })
            .collect()
    };
    SummaryOut {
        gate_violations: v.gates,
        suspicions: v.suspicions,
        gate_drivers: drivers(v.gate_drivers),
        suspicion_drivers: drivers(v.suspicion_drivers),
        duplicated_pct: dup.all_pct,
        test_unit_share_pct: 100.0 * agg.test_units as f64 / agg.units.max(1) as f64,
    }
}

/// One row per language present, carrying the denominators a
/// per-language rate needs: files, units, and per metric the count it
/// measured beside the count it violated.
fn language_rows(agg: &Agg) -> Vec<LangOut> {
    LANGS
        .iter()
        .filter(|l| agg.files_by_lang[**l as usize] > 0)
        .map(|l| {
            let row = &agg.metric_by_lang[*l as usize];
            LangOut {
                lang: l.name(),
                files: agg.files_by_lang[*l as usize],
                units: agg.units_by_lang[*l as usize],
                metrics: METRICS
                    .iter()
                    .enumerate()
                    .filter(|(m, _)| row[*m].measured > 0)
                    .map(|(m, def)| LangMetric {
                        name: def.name,
                        measured: row[m].measured,
                        violations: row[m].violated,
                    })
                    .collect(),
            }
        })
        .collect()
}

fn rates_out(agg: &Agg) -> Vec<RateOut> {
    super::rate_rows(agg)
        .into_iter()
        .map(|r| RateOut {
            metric: r.metric,
            lang: r.lang,
            covered: r.covered,
            total: r.total,
            gold_rate: r.gold,
        })
        .collect()
}

pub fn render_json(agg: &mut Agg) -> String {
    let (clones, dup) = clone_section(agg);

    // Owns its output; must precede `violations`, which borrows agg.
    agg.graph.read_mut().sort_by(|a, b| a.path.cmp(&b.path));
    let architecture =
        crate::graph::analyze(agg.graph.read(), agg.mentions.read()).map(architecture_out);

    let (clumps, repeated_dispatch, undeclared_shapes) = recurrences_out(agg);
    let untested_complexity = super::select_untested(agg)
        .into_iter()
        .map(|(unit, cyclomatic)| Untested { unit, cyclomatic })
        .collect();
    let summary = summary_out(agg, dup);
    let coverage_rates = rates_out(agg);
    let near = crate::near::pairs(agg.prints.read(), usize::MAX);
    let near_clones = NearOut {
        pairs: near
            .pairs
            .into_iter()
            .map(|p| NearPairOut {
                a: p.a,
                b: p.b,
                overlap_pct: 100.0 * p.overlap,
            })
            .collect(),
        suppressed_cores: near.suppressed_cores,
        widest_core: near.widest_core,
    };
    let narrative = super::narrative_rows(agg)
        .into_iter()
        .map(
            |(lang, step_down_pct, refs, public_first_pct, pairs)| NarrativeOut {
                lang,
                step_down_pct,
                intra_file_refs: refs,
                public_first_pct,
                public_private_pairs: pairs,
            },
        )
        .collect();

    let metrics = metric_rows(agg);
    for heap in agg.offenders.iter_mut() {
        heap.sort_unstable_by(worse);
    }
    let violations: Vec<Violation> = agg
        .violations()
        .map(|v| Violation {
            metric: METRICS[v.metric].name,
            path: v.path,
            line: v.line,
            unit: v.unit,
            value: v.value,
        })
        .collect();

    let languages = language_rows(agg);

    let mut low_confidence_files = agg.low_confidence.clone();
    low_confidence_files.sort_unstable();

    let report = Report {
        schema_version: SCHEMA_VERSION,
        files: agg.files,
        units: agg.units,
        lines: agg.lines,
        skipped_files: agg.skipped,
        generated_files: agg.generated,
        parse_error_files: agg.error_files,
        low_confidence_files,
        languages,
        metrics,
        violations,
        clones,
        clumps,
        repeated_dispatch,
        undeclared_shapes,
        untested_complexity,
        architecture,
        narrative,
        coverage_rates,
        near_clones,
        summary,
    };
    let mut out = serde_json::to_string_pretty(&report).expect("report serialization");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::extract;
    use crate::lang::Lang;
    use std::path::Path;

    #[test]
    fn json_is_valid_versioned_and_complete() {
        let src = "def NAME(xs):\n    t = 0\n    for x in xs:\n        if x > 0:\n            if x > 1:\n                if x > 2:\n                    if x > 3:\n                        if x > 4:\n                            t += x\n    return t\n";
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let mut agg = Agg::complete();
        agg.add_file(&extract(
            pack,
            &mut parser,
            Path::new("a.py"),
            &src.replace("NAME", "f0"),
        ));
        agg.add_file(&extract(
            pack,
            &mut parser,
            Path::new("b.py"),
            &src.replace("NAME", "f1"),
        ));

        let out = render_json(&mut agg);
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(v["schema_version"], 2);
        assert_eq!(v["files"], 2);
        assert_eq!(v["languages"][0]["lang"], "py");
        // The denominators a per-language rate is computed from. One
        // language here, so each has to equal its pooled total, and the
        // per-metric counts have to agree with the pooled metric rows.
        assert_eq!(v["languages"][0]["units"], v["units"]);
        let by_lang = |name: &str, field: &str| {
            v["languages"][0]["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["name"] == name)
                .unwrap_or_else(|| panic!("{name} measured in py"))[field]
                .as_u64()
                .expect("count")
        };
        for m in v["metrics"].as_array().unwrap() {
            let name = m["name"].as_str().unwrap();
            assert_eq!(
                by_lang(name, "measured"),
                m["n"].as_u64().unwrap(),
                "{name}"
            );
        }
        assert!(by_lang("cognitive", "violations") > 0);
        let cognitive = v["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "cognitive")
            .expect("cognitive metric present");
        assert_eq!(cognitive["rung"], 2);
        let violations = v["violations"].as_array().unwrap();
        assert!(
            violations
                .iter()
                .any(|x| x["path"] == "a.py" && x["metric"] == "cognitive")
        );
        assert!(!v["clones"]["classes"].as_array().unwrap().is_empty());
    }
}
