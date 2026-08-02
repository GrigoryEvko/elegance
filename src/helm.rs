//! Configuration, where it is decidable.
//!
//! A Helm chart looks like an import graph — `values.yaml` declares
//! keys, templates reference them — and treating it as one is a
//! category error. Measured on a real chart, the naive analysis
//! produced 48 findings and ZERO were true: `.Values.deployments.spot`
//! is reached through `range $name, $deploy`, `.Values.generate.resources`
//! through `index .root.Values .svc`, `.Values.depth.enabled` appears
//! only inside a comment, and `.Values.ingress.albName` is guarded by
//! `| default`. Helm is a template language with aliasing, computed
//! access and helper indirection — the same "the text stops predicting
//! the run" property `Sem::Spooky` names in code.
//!
//! So this tier reports only what needs no template evaluation at all:
//!
//! - OVERLAY DRIFT: which keys each environment sets, by set
//!   comparison. A key set in one region and not the others is a fact,
//!   whether or not the fallback was intentional — which is exactly
//!   why it reports and never gates.
//! - SECRETS in YAML scalars, where a credential actually leaks: a
//!   ConfigMap or an env block, not a template expression.
//!
//! What a template RENDERS to is measured separately, by `--render`,
//! which runs helm and reads the output — the same call the C pack
//! makes about macros: measure what you can actually read.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::Write;
use std::path::Path;

use yaml_rust2::{Yaml, YamlLoader};

/// Environments shown before the table stops being read.
const SHOW: usize = 12;

/// Every dotted path in a values tree, interior nodes included.
fn paths(node: &Yaml, prefix: &str, into: &mut BTreeSet<String>) {
    let Yaml::Hash(map) = node else { return };
    for (key, value) in map {
        let Some(key) = key.as_str() else { continue };
        let path = match prefix.is_empty() {
            true => key.to_string(),
            false => format!("{prefix}.{key}"),
        };
        paths(value, &path, into);
        into.insert(path);
    }
}

fn load_paths(file: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(text) = std::fs::read_to_string(file) else {
        return out;
    };
    let Ok(docs) = YamlLoader::load_from_str(&text) else {
        return out;
    };
    for doc in &docs {
        paths(doc, "", &mut out);
    }
    out
}

/// A chart: its base values and one key set per environment overlay.
struct Chart {
    name: String,
    base: BTreeSet<String>,
    overlays: Vec<(String, BTreeSet<String>)>,
}

/// Charts under a root: a directory holding Chart.yaml and values.yaml.
fn charts(root: &Path) -> Vec<Chart> {
    let mut found = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
    {
        if entry.file_name() != "Chart.yaml" {
            continue;
        }
        let Some(dir) = entry.path().parent() else {
            continue;
        };
        let values = dir.join("values.yaml");
        if !values.is_file() {
            continue;
        }
        let mut overlays: Vec<(String, BTreeSet<String>)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| is_overlay(p.as_path()))
            .map(|p| (environment_of(&p), load_paths(&p)))
            .collect();
        overlays.sort_by(|a, b| a.0.cmp(&b.0));
        found.push(Chart {
            name: dir.display().to_string(),
            base: load_paths(&values),
            overlays,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn is_overlay(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("values-") && n.ends_with(".yaml"))
}

/// `values-prod-us-east-1.yaml` names the environment `prod-us-east-1`.
fn environment_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("values-"))
        .unwrap_or("?")
        .to_string()
}

pub fn run(root: &Path) -> Result<i32, Box<dyn Error>> {
    let charts = charts(root);
    if charts.is_empty() {
        println!(
            "helm — no chart (a directory with Chart.yaml and values.yaml) under {}",
            root.display()
        );
        return Ok(0);
    }
    let mut out = String::new();
    for chart in &charts {
        render_chart(chart, &mut out);
    }
    render_secrets(root, &mut out);
    print!("{out}");
    Ok(0)
}

fn render_chart(chart: &Chart, out: &mut String) {
    let _ = writeln!(
        out,
        "\nchart {} — {} value paths, {} environment overlays",
        chart.name,
        chart.base.len(),
        chart.overlays.len()
    );
    if chart.overlays.len() < 2 {
        return;
    }
    let sets: Vec<&BTreeSet<String>> = chart.overlays.iter().map(|(_, s)| s).collect();
    let everywhere: BTreeSet<String> = sets[1..].iter().fold(sets[0].clone(), |acc, s| {
        acc.intersection(s).cloned().collect()
    });
    let anywhere: BTreeSet<String> = sets.iter().fold(BTreeSet::new(), |mut acc, s| {
        acc.extend(s.iter().cloned());
        acc
    });
    let _ = writeln!(
        out,
        "  {} keys set in every environment, {} in at least one\n",
        everywhere.len(),
        anywhere.len()
    );
    let _ = writeln!(out, "  {:>5} {:>9}  environment", "keys", "elsewhere");
    for (env, keys) in chart.overlays.iter().take(SHOW) {
        let _ = writeln!(
            out,
            "  {:>5} {:>9}  {env}",
            keys.len(),
            anywhere.difference(keys).count()
        );
    }
    let partial: Vec<&String> = anywhere.difference(&everywhere).collect();
    if partial.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n  set in SOME environments but not all ({}):",
        partial.len()
    );
    for key in partial.iter().take(SHOW) {
        let where_set: Vec<&str> = chart
            .overlays
            .iter()
            .filter(|(_, s)| s.contains(*key))
            .map(|(e, _)| e.as_str())
            .collect();
        let _ = writeln!(out, "    {:<44} {}", key, where_set.join(", "));
    }
    if partial.len() > SHOW {
        let _ = writeln!(out, "    ... and {} more", partial.len() - SHOW);
    }
    let _ = writeln!(
        out,
        "\n  A key set in one environment and not another may be an\n\
         \x20 intentional fallback to the base chart. That is judgment; this\n\
         \x20 is the fact, which is why it reports and never gates."
    );
}

/// Credentials in YAML scalars — a ConfigMap's data, an env block's
/// value. Scanned textually rather than through the parser: `key:
/// value` on one line carries its own line number, which is what a
/// finding needs, and the entropy rules are the extractor's own.
fn render_secrets(root: &Path, out: &mut String) {
    let mut found: Vec<String> = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            continue;
        }
        // A chart's own test fixtures authenticate nothing.
        let shown = path.display().to_string();
        if shown.contains("/test") || shown.contains("/ci/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if let Some(key) = leaked_key(line) {
                found.push(format!("  {shown}:{}  {key}", n + 1));
            }
        }
    }
    let _ = writeln!(
        out,
        "\nsecrets in configuration — {} literal credentials",
        found.len()
    );
    for site in found.iter().take(SHOW) {
        let _ = writeln!(out, "{site}");
    }
    if found.len() > SHOW {
        let _ = writeln!(out, "  ... and {} more", found.len() - SHOW);
    }
}

/// The key of a `key: value` line whose value is a hardcoded
/// credential. A Helm template expression (`{{ ... }}`) is a reference
/// to a secret, not one.
fn leaked_key(line: &str) -> Option<&str> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim().trim_matches(['"', '\'', '-', ' ']);
    let value = value.trim().trim_matches(['"', '\'']);
    if value.contains("{{") || value.is_empty() {
        return None;
    }
    crate::facts::is_leaked_credential(key, value).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_drift_is_a_set_comparison_needing_no_template() {
        let dir = std::env::temp_dir().join(format!("elegance-helm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let chart = dir.join("chart");
        std::fs::create_dir_all(&chart).unwrap();
        std::fs::write(chart.join("Chart.yaml"), "name: demo\n").unwrap();
        std::fs::write(
            chart.join("values.yaml"),
            "image:\n  tag: latest\nreplicas: 1\nresources:\n  limits:\n    cpu: 1\n",
        )
        .unwrap();
        // east sets resources; west does not — the asymmetry IS the
        // finding, whether or not the fallback was intended.
        std::fs::write(
            chart.join("values-prod-east.yaml"),
            "replicas: 5\nresources:\n  limits:\n    cpu: 4\n",
        )
        .unwrap();
        std::fs::write(chart.join("values-prod-west.yaml"), "replicas: 2\n").unwrap();

        let found = charts(&dir);
        assert_eq!(found.len(), 1);
        let c = &found[0];
        assert!(c.base.contains("resources.limits.cpu"), "nested path");
        assert_eq!(c.overlays.len(), 2);
        let mut out = String::new();
        render_chart(c, &mut out);
        assert!(
            out.contains("prod-east") && out.contains("prod-west"),
            "{out}"
        );
        assert!(
            out.contains("resources.limits.cpu"),
            "the asymmetric key is named:\n{out}"
        );
        assert!(
            !out.contains("replicas"),
            "set everywhere, not drift:\n{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_credential_in_yaml_fires_and_a_reference_to_one_does_not() {
        assert_eq!(
            leaked_key("  api_key: \"sk9f3kd0asdf8812jjd\""),
            Some("api_key")
        );
        // The shapes of config that READS a secret rather than holding it.
        assert_eq!(leaked_key("  apiKey: {{ .Values.apiKey }}"), None);
        assert_eq!(leaked_key("  api_key: \"\""), None);
        assert_eq!(leaked_key("  passwordField: password"), None);
        assert_eq!(leaked_key("  image: nginx:1.25"), None);
        // Vendor formats need no credential-shaped key at all. Built at
        // run time so no source line carries a complete provider-shaped
        // token — same reasoning as the extractor's own fixtures.
        let line = format!(
            "  token: {}{}",
            "ghp_", "16C7e42F292c6912E7710c838347Ae178B4a"
        );
        assert_eq!(leaked_key(&line), Some("token"));
    }
}
