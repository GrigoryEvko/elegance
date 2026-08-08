//! SARIF 2.1.0: the interchange format GitHub code scanning, the VS Code
//! SARIF viewer and most enterprise pipelines already read. Emitting it
//! makes PR annotations a drop-in instead of glue each consumer writes.
//!
//! The ladder survives translation: rungs 0-2 map to `error`, 3-4 to
//! `warning`, 5+ to `note`. Rule ids are stable across versions because
//! consumers key suppressions off them.

use serde::Serialize;

use super::{Agg, select_clones, select_clumps, select_switches, suggest, worse};
use crate::metrics::METRICS;

const SCHEMA: &str = "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json";
const VERSION: &str = "2.1.0";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sarif {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<Run>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    tool: Tool,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct Tool {
    driver: Driver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Driver {
    name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    information_uri: Option<&'static str>,
    rules: Vec<Rule>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    id: String,
    name: String,
    short_description: Text,
    full_description: Text,
    default_configuration: Config,
    properties: RuleProps,
}

#[derive(Serialize)]
struct RuleProps {
    /// The ladder rung, so consumers can reconstruct the verdict class.
    rung: u8,
    tags: Vec<&'static str>,
}

#[derive(Serialize)]
struct Config {
    level: &'static str,
}

#[derive(Serialize)]
struct Text {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
    rule_id: String,
    level: &'static str,
    message: Text,
    locations: Vec<Location>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    related_locations: Vec<Location>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Location {
    physical_location: Physical,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Physical {
    artifact_location: Artifact,
    region: Region,
}

#[derive(Serialize)]
struct Artifact {
    uri: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Region {
    start_line: u32,
}

/// Rungs 0-2 gate a build, 3-4 are suspicions, 5+ describe. SARIF has
/// exactly three severities and they line up.
fn level_for(rung: u8) -> &'static str {
    match rung {
        0..=2 => "error",
        3..=4 => "warning",
        _ => "note",
    }
}

fn location(path: &str, line: u32) -> Location {
    Location {
        physical_location: Physical {
            artifact_location: Artifact {
                uri: path.trim_start_matches("./").to_string(),
            },
            region: Region {
                start_line: line.max(1),
            },
        },
    }
}

/// Rule id for a metric. Stable forever: consumers key suppressions and
/// historical trends off these strings.
fn rule_id(name: &str) -> String {
    format!("elegance/{}", name.replace(' ', "-"))
}

pub fn render(agg: &mut Agg) -> String {
    let mut results = recurrence_results(agg);
    results.extend(violation_results(agg));
    let sarif = Sarif {
        schema: SCHEMA,
        version: VERSION,
        runs: vec![Run {
            tool: Tool {
                driver: Driver {
                    name: "elegance",
                    // The rules are documented in the tool itself, so no
                    // URL is emitted; one that 404s would be worse.
                    information_uri: None,
                    rules: rules(),
                },
            },
            results,
        }],
    };
    let mut out = serde_json::to_string_pretty(&sarif).expect("sarif serialization");
    out.push('\n');
    out
}

/// One rule per metric plus the recurrence findings, which are about a
/// set of sites rather than a single measurement.
fn rules() -> Vec<Rule> {
    let described = |id: &str, text: String, rung: u8, recurrence: bool| Rule {
        id: rule_id(id),
        name: id.replace(' ', "-"),
        short_description: Text {
            text: id.to_string(),
        },
        full_description: Text { text },
        default_configuration: Config {
            level: level_for(rung),
        },
        properties: RuleProps {
            rung,
            tags: if recurrence {
                vec!["quality", "recurrence"]
            } else {
                vec!["quality"]
            },
        },
    };
    let mut rules: Vec<Rule> = METRICS
        .iter()
        .map(|def| {
            let text = format!(
                "Ladder rung {}: {}. Budgets are per language, pinned to gold-corpus percentiles.",
                def.rung,
                verdict_class(def.rung)
            );
            described(def.name, text, def.rung, false)
        })
        .collect();
    for (id, description) in RECURRENCE_RULES {
        rules.push(described(id, (*description).to_string(), 4, true));
    }
    rules
}

fn violation_results(agg: &mut Agg) -> Vec<SarifResult> {
    for heap in agg.offenders.iter_mut() {
        heap.sort_unstable_by(worse);
    }
    agg.violations()
        .map(|v| {
            let def = &METRICS[v.metric];
            let subject = if v.unit.is_empty() {
                String::new()
            } else {
                format!(" in {}", v.unit)
            };
            SarifResult {
                rule_id: rule_id(def.name),
                level: level_for(def.rung),
                message: Text {
                    text: format!("{} {:.0}{subject}", def.name, v.value),
                },
                locations: vec![location(v.path, v.line)],
                related_locations: Vec::new(),
            }
        })
        .collect()
}

fn verdict_class(rung: u8) -> &'static str {
    match rung {
        0..=2 => "a violation, gateable in CI",
        3..=4 => "a suspicion worth a human look",
        _ => "a distributional report, never a gate",
    }
}

const RECURRENCE_RULES: &[(&str, &str)] = &[
    (
        "clones",
        "Type-2 structural duplication: the same logic with identifiers and literals renamed. Every site must change together.",
    ),
    (
        "near-clones",
        "Edited copies: units sharing most of their winnowed shape. The Merkle detector stops seeing a copy the moment a line is inserted; this does not.",
    ),
    (
        "param clumps",
        "The same parameter names travelling together through many signatures — a type the language was never told about (Fowler's Data Clumps).",
    ),
    (
        "repeated dispatch",
        "The same case-label set switched on in several places: every new variant costs N edits (Fowler: Replace Conditional with Polymorphism).",
    ),
];

/// Exact clone classes, as findings about a SET of sites: the first
/// site is the result location, the rest are relatedLocations so a
/// viewer can walk them.
fn clone_results(agg: &mut Agg) -> Vec<SarifResult> {
    let (clones, _) = select_clones(agg);
    clones
        .into_iter()
        .map(|class| {
            let (first, rest) = class.sites.split_first().expect("classes have >=2 sites");
            SarifResult {
                rule_id: rule_id("clones"),
                level: "warning",
                message: Text {
                    text: suggest::for_clone(class.sites.len(), class.mass),
                },
                locations: vec![location(&first.path, first.line)],
                related_locations: rest.iter().map(|s| location(&s.path, s.line)).collect(),
            }
        })
        .collect()
}

fn recurrence_results(agg: &mut Agg) -> Vec<SarifResult> {
    let mut out = Vec::new();
    for pair in crate::near::pairs(agg.prints.read(), usize::MAX).pairs {
        let sites: Vec<(String, u32)> = [&pair.a, &pair.b]
            .iter()
            .filter_map(|s| parse_site(s))
            .collect();
        let Some((first, rest)) = sites.split_first() else {
            continue;
        };
        out.push(SarifResult {
            rule_id: rule_id("near-clones"),
            level: "warning",
            message: Text {
                text: format!(
                    "{:.0}% shared shape with {} (edited copy)",
                    pair.overlap * 100.0,
                    pair.b
                ),
            },
            locations: vec![location(&first.0, first.1)],
            related_locations: rest.iter().map(|(p, l)| location(p, *l)).collect(),
        });
    }
    out.extend(clone_results(agg));
    for (id, groups, message) in [
        (
            "param clumps",
            select_clumps(agg),
            suggest::for_clump as fn(&[String], u32) -> String,
        ),
        (
            "repeated dispatch",
            select_switches(agg),
            suggest::for_dispatch,
        ),
    ] {
        for g in groups {
            let sites: Vec<(String, u32)> = g.sites.iter().filter_map(|s| parse_site(s)).collect();
            let Some((first, rest)) = sites.split_first() else {
                continue;
            };
            out.push(SarifResult {
                rule_id: rule_id(id),
                level: "warning",
                message: Text {
                    text: message(&g.names, g.count),
                },
                locations: vec![location(&first.0, first.1)],
                related_locations: rest.iter().map(|(p, l)| location(p, *l)).collect(),
            });
        }
    }
    out
}

/// Recurrence sites are rendered as "path:line  unit" strings; SARIF
/// needs the path and line back.
fn parse_site(site: &str) -> Option<(String, u32)> {
    let head = site.split_whitespace().next()?;
    let (path, line) = head.rsplit_once(':')?;
    Some((path.to_string(), line.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::extract;
    use crate::lang::Lang;
    use std::path::Path;

    #[test]
    fn sarif_is_well_formed_and_preserves_the_ladder() {
        let src = "def NAME(xs):\n    t = 0\n    for x in xs:\n        if x > 0:\n            if x > 1:\n                if x > 2:\n                    if x > 3:\n                        if x > 4:\n                            t += x\n    return t\n";
        let pack = Lang::Python.pack();
        let mut parser = pack.make_parser();
        let mut agg = Agg::complete();
        for name in ["f0", "f1"] {
            agg.add_file(&extract(
                pack,
                &mut parser,
                Path::new(&format!("{name}.py")),
                &src.replace("NAME", name),
            ));
        }

        let v: serde_json::Value = serde_json::from_str(&render(&mut agg)).expect("valid json");
        assert_eq!(v["version"], "2.1.0");
        let run = &v["runs"][0];
        assert_eq!(run["tool"]["driver"]["name"], "elegance");

        // Every result names a rule the driver declared.
        let ids: Vec<&str> = run["tool"]["driver"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        let results = run["results"].as_array().unwrap();
        assert!(!results.is_empty());
        for r in results {
            let id = r["ruleId"].as_str().unwrap();
            assert!(ids.contains(&id), "undeclared rule {id}");
            assert!(r["locations"][0]["physicalLocation"]["region"]["startLine"].is_number());
        }

        // The ladder survives: cognitive is rung 2, so it must be an error.
        let cognitive = results
            .iter()
            .find(|r| r["ruleId"] == "elegance/cognitive")
            .expect("cognitive result");
        assert_eq!(cognitive["level"], "error");

        // Clone classes carry their other sites as related locations.
        let clone = results
            .iter()
            .find(|r| r["ruleId"] == "elegance/clones")
            .expect("clone result");
        assert!(!clone["relatedLocations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn levels_follow_the_rung() {
        assert_eq!(level_for(0), "error");
        assert_eq!(level_for(2), "error");
        assert_eq!(level_for(3), "warning");
        assert_eq!(level_for(5), "note");
    }
}
