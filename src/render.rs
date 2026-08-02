//! Measure the render, not the template.
//!
//! `--helm` reports what a chart says without evaluating it, because
//! evaluating it statically is impossible: aliasing, computed access
//! and helper indirection defeat every static reading (48 findings on
//! a real chart, zero true — see the helm module).
//!
//! The way past that is not a cleverer analysis. It is to stop
//! analysing the template and read what it PRODUCES: `helm template`
//! resolves every indirection by running them, and hands back plain
//! manifests where a resource's shape is simply visible. It is the
//! same call the C pack makes about macros — measure what you can
//! actually read, and say so when you cannot.
//!
//! What that makes decidable, and nothing else does:
//!
//! - CROSS-ENVIRONMENT DRIFT in the artifacts that actually ship. Not
//!   which values a file sets, but which resources each environment
//!   ends up with, and which of them differ.
//! - CREDENTIALS that a template injected from its values, invisible
//!   in both the template and the values file on their own.
//! - RESOURCE SHAPE: what a rendered container declares.
//!
//! Report-only, always. Nothing here gates and nothing enters language
//! calibration: a manifest is not code, and letting it move a
//! cognitive budget would be a category error of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write;
use std::path::Path;
use std::process::Command;

/// Resources shown before the list stops being read.
const SHOW: usize = 12;

/// One rendered resource, keyed the way Kubernetes itself keys them.
struct Resource {
    kind: String,
    name: String,
    /// Every `key: value` scalar, for comparing environments.
    fields: BTreeMap<String, String>,
}

impl Resource {
    fn id(&self) -> String {
        format!("{}/{}", self.kind, self.name)
    }
}

/// A chart directory and the environment value files beside it.
struct Chart {
    dir: std::path::PathBuf,
    environments: Vec<(String, std::path::PathBuf)>,
}

pub fn run(root: &Path) -> Result<i32, Box<dyn Error>> {
    if !helm_present() {
        println!(
            "render — helm is not on PATH, so there is nothing to render.\n\
             Install helm, or use --helm for the analysis that needs no rendering."
        );
        return Ok(0);
    }
    let charts = charts(root);
    if charts.is_empty() {
        println!(
            "render — no chart with values files under {}",
            root.display()
        );
        return Ok(0);
    }
    let mut out = String::new();
    for chart in &charts {
        render_chart(chart, &mut out);
    }
    print!("{out}");
    Ok(0)
}

fn helm_present() -> bool {
    Command::new("helm")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success())
}

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
        let mut environments: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter_map(|p| Some((environment_of(&p)?, p)))
            .collect();
        environments.sort_by(|a, b| a.0.cmp(&b.0));
        if environments.is_empty() {
            continue;
        }
        found.push(Chart {
            dir: dir.to_path_buf(),
            environments,
        });
    }
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    found
}

fn environment_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".yaml")?;
    Some(stem.strip_prefix("values-")?.to_string())
}

/// `helm template` for one environment. A chart that will not render
/// is itself worth one line: whatever ships from here ships from a
/// template somebody cannot evaluate either.
fn render_one(chart: &Chart, values: &Path) -> Result<Vec<Resource>, String> {
    let mut helm = Command::new("helm");
    helm.arg("template").arg(&chart.dir).arg("-f").arg(values);
    let out = helm.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or("render failed");
        return Err(first.to_string());
    }
    let manifests = String::from_utf8_lossy(&out.stdout);
    Ok(parse_manifests(&manifests))
}

/// Rendered manifests, split on document boundaries. A hand-rolled
/// scalar reader rather than a YAML parse: what this needs is a
/// resource's identity and its flat scalars, and reading them by line
/// keeps the credential scan working on the text that actually shipped.
fn parse_manifests(text: &str) -> Vec<Resource> {
    let mut found = Vec::new();
    for doc in text.split("\n---") {
        let mut kind = String::new();
        let mut name = String::new();
        let mut fields = BTreeMap::new();
        for line in doc.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let depth = key.len() - key.trim_start().len();
            let key = key.trim().trim_matches(['"', '\'', '-', ' ']);
            let value = value.trim().trim_matches(['"', '\'']);
            if value.is_empty() {
                continue;
            }
            match key {
                "kind" if depth == 0 => kind = value.to_string(),
                // The metadata name, not a nested template's.
                "name" if depth <= 2 && name.is_empty() => name = value.to_string(),
                _ => {}
            }
            fields.insert(format!("{key}@{depth}"), value.to_string());
        }
        if !kind.is_empty() {
            found.push(Resource { kind, name, fields });
        }
    }
    found
}

fn render_chart(chart: &Chart, out: &mut String) {
    let _ = writeln!(
        out,
        "\nchart {} — rendering {} environments",
        chart.dir.display(),
        chart.environments.len()
    );
    let mut rendered: Vec<(String, Vec<Resource>)> = Vec::new();
    for (env, values) in &chart.environments {
        match render_one(chart, values) {
            Ok(resources) => rendered.push((env.clone(), resources)),
            Err(why) => {
                let _ = writeln!(out, "  {env}: WILL NOT RENDER — {why}");
            }
        }
    }
    if rendered.is_empty() {
        return;
    }
    for (env, resources) in &rendered {
        let kinds = resources.iter().fold(BTreeMap::new(), |mut acc, r| {
            *acc.entry(r.kind.as_str()).or_insert(0) += 1;
            acc
        });
        let shape: Vec<String> = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect();
        let _ = writeln!(
            out,
            "  {:<22} {:>3} resources: {}",
            env,
            resources.len(),
            shape.join(", ")
        );
    }
    render_drift(&rendered, out);
    render_secrets(&rendered, out);
}

/// Which resources each environment ends up with — the artifacts that
/// actually ship, not the values that were set. A resource present in
/// one environment and missing from another is a difference no values
/// diff shows, because it may come from a conditional in a template.
fn render_drift(rendered: &[(String, Vec<Resource>)], out: &mut String) {
    if rendered.len() < 2 {
        return;
    }
    let sets: Vec<(&String, BTreeSet<String>)> = rendered
        .iter()
        .map(|(env, rs)| (env, rs.iter().map(Resource::id).collect()))
        .collect();
    let everywhere: BTreeSet<String> = sets[1..].iter().fold(sets[0].1.clone(), |acc, (_, s)| {
        acc.intersection(s).cloned().collect()
    });
    let anywhere: BTreeSet<String> = sets.iter().fold(BTreeSet::new(), |mut acc, (_, s)| {
        acc.extend(s.iter().cloned());
        acc
    });
    let partial: Vec<&String> = anywhere.difference(&everywhere).collect();
    if partial.is_empty() {
        let _ = writeln!(
            out,
            "  every environment renders the same {} resources",
            everywhere.len()
        );
        return;
    }
    let _ = writeln!(
        out,
        "\n  resources NOT rendered everywhere ({} of {}):",
        partial.len(),
        anywhere.len()
    );
    for id in partial.iter().take(SHOW) {
        let present: Vec<&str> = sets
            .iter()
            .filter(|(_, s)| s.contains(*id))
            .map(|(env, _)| env.as_str())
            .collect();
        let _ = writeln!(out, "    {:<40} {}", id, present.join(", "));
    }
    if partial.len() > SHOW {
        let _ = writeln!(out, "    ... and {} more", partial.len() - SHOW);
    }
}

/// The field names of one resource holding a literal credential. The
/// depth suffix `parse_manifests` adds for uniqueness is not part of
/// the name the reader needs.
fn leaked_keys(r: &Resource) -> Vec<&str> {
    r.fields
        .iter()
        .map(|(key, value)| (key.split('@').next().unwrap_or(key), value))
        .filter(|(key, value)| crate::facts::is_leaked_credential(key, value))
        .map(|(key, _)| key)
        .collect()
}

/// Credentials the TEMPLATE injected. Invisible in the template (it
/// holds a reference) and invisible in the values file (it holds a
/// fragment) — visible only here, in what shipped.
fn render_secrets(rendered: &[(String, Vec<Resource>)], out: &mut String) {
    let mut found: BTreeSet<String> = BTreeSet::new();
    for (env, resources) in rendered {
        for r in resources {
            let id = r.id();
            for key in leaked_keys(r) {
                found.insert(format!("  {env}  {id}  {key}"));
            }
        }
    }
    if found.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n  credentials in RENDERED output ({}) — injected by a template,\n\
         \x20 so neither the template nor the values file shows them alone:",
        found.len()
    );
    for site in found.iter().take(SHOW) {
        let _ = writeln!(out, "{site}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "---\n\
        apiVersion: apps/v1\n\
        kind: Deployment\n\
        metadata:\n  name: web\n\
        spec:\n  replicas: 3\n\
        ---\n\
        apiVersion: v1\n\
        kind: ConfigMap\n\
        metadata:\n  name: settings\n\
        data:\n  api_key: sk9f3kd0asdf8812jjd\n";

    #[test]
    fn manifests_split_into_identified_resources() {
        let found = parse_manifests(MANIFEST);
        let ids: Vec<String> = found.iter().map(Resource::id).collect();
        assert_eq!(ids, ["Deployment/web", "ConfigMap/settings"]);
    }

    #[test]
    fn drift_names_what_one_environment_ships_and_another_does_not() {
        let east = parse_manifests(MANIFEST);
        let west =
            parse_manifests("---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n");
        let mut out = String::new();
        render_drift(&[("east".into(), east), ("west".into(), west)], &mut out);
        assert!(out.contains("ConfigMap/settings"), "{out}");
        assert!(out.contains("east"), "names where it renders:\n{out}");
        assert!(!out.contains("Deployment/web"), "shared, not drift:\n{out}");
    }

    #[test]
    fn a_credential_that_only_the_render_reveals() {
        let mut out = String::new();
        render_secrets(&[("east".into(), parse_manifests(MANIFEST))], &mut out);
        assert!(out.contains("ConfigMap/settings"), "{out}");
        assert!(out.contains("api_key"), "{out}");
        // An identical chart without the injected value stays silent.
        let mut quiet = String::new();
        render_secrets(
            &[(
                "east".into(),
                parse_manifests(
                    "---\nkind: ConfigMap\nmetadata:\n  name: c\ndata:\n  api_key: \"\"\n",
                ),
            )],
            &mut quiet,
        );
        assert!(quiet.is_empty(), "{quiet}");
    }
}
