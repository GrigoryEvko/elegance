//! Declared layer contracts: the one architecture claim that may gate.
//!
//! Every other architecture measurement is a description — cycle mass,
//! blast radius, depth — and descriptions are reported because a number
//! about a graph is not a verdict about a design. A CONTRACT is
//! different. When a repository declares that its products never import
//! each other, an import between them is not a heuristic finding at a
//! calibrated threshold. It is the stated rule, broken, and gating
//! needs certainty of that kind.
//!
//! This is the shape teams already enforce by hand. A monorepo with
//! isolated products checks it with a bespoke script that re-implements
//! import parsing badly; elegance has already resolved the import
//! graph, so the check costs a set lookup per edge.
//!
//! Unresolved imports are never judged. A path assembled at run time
//! resolves to nothing, and a contract check that guesses would be the
//! kind of claim the Helm tier refused to make.

use std::collections::BTreeSet;
use std::fmt::Write;

use serde::Deserialize;

use crate::graph::GraphFacts;

/// One named layer: which files belong to it and what it may reach.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// Path fragments, matched anywhere in a file's path. A file
    /// belongs to the layer with the LONGEST matching fragment, so
    /// `src/ui/widgets` can carve itself out of `src/ui`.
    pub paths: Vec<String>,
    /// Layers this one may import, by name. Absent means "none": an
    /// isolated product declares nothing and reaches nothing.
    #[serde(default)]
    pub may_import: Vec<String>,
}

/// One import that the contract forbids.
pub struct Breach {
    pub from: String,
    pub to: String,
    pub from_layer: String,
    pub to_layer: String,
}

/// Which layer a file belongs to: longest matching fragment wins, and
/// a file in no declared layer is unjudged rather than guessed at.
fn layer_of<'a>(
    layers: &'a std::collections::HashMap<String, Layer>,
    path: &std::path::Path,
) -> Option<&'a str> {
    let display = path.to_string_lossy().replace('\\', "/");
    layers
        .iter()
        .filter_map(|(name, layer)| {
            layer
                .paths
                .iter()
                .filter(|prefix| display.contains(prefix.as_str()))
                .map(|prefix| prefix.len())
                .max()
                .map(|len| (len, name.as_str()))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, name)| name)
}

/// Every import the declared contract forbids. Both endpoints must sit
/// in declared layers, and the edge must have RESOLVED. A target the
/// resolver could only guess at is not a breach.
pub fn breaches(
    layers: &std::collections::HashMap<String, Layer>,
    files: &[GraphFacts],
) -> Vec<Breach> {
    if layers.is_empty() {
        return Vec::new();
    }
    let (_, targets) = crate::graph::resolve_imports(files);
    let mut found = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let Some(from_layer) = layer_of(layers, &file.path) else {
            continue;
        };
        let allowed: BTreeSet<&str> = layers[from_layer]
            .may_import
            .iter()
            .map(String::as_str)
            .collect();
        for target in targets[i].iter().flatten() {
            let Some(to_layer) = layer_of(layers, &files[*target].path) else {
                continue;
            };
            // A layer always reaches itself.
            if to_layer == from_layer || allowed.contains(to_layer) {
                continue;
            }
            found.push(Breach {
                from: files[i].path.display().to_string(),
                to: files[*target].path.display().to_string(),
                from_layer: from_layer.to_string(),
                to_layer: to_layer.to_string(),
            });
        }
    }
    found.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    found
}

/// Breaches shown before the list stops being read.
const SHOW: usize = 12;

pub fn render(breaches: &[Breach], layers: usize, out: &mut String) {
    if layers == 0 {
        return;
    }
    if breaches.is_empty() {
        let _ = writeln!(out, "\nlayers — {layers} declared, contract holds");
        return;
    }
    let _ = writeln!(
        out,
        "\nlayers — {} import{} the contract forbids:",
        breaches.len(),
        match breaches.len() {
            1 => "",
            _ => "s",
        }
    );
    for b in breaches.iter().take(SHOW) {
        let _ = writeln!(
            out,
            "  {} ({}) -> {} ({})",
            b.from, b.from_layer, b.to, b.to_layer
        );
    }
    if breaches.len() > SHOW {
        let _ = writeln!(out, "  ... and {} more", breaches.len() - SHOW);
    }
    let _ = writeln!(
        out,
        "  A declared contract is the one architecture claim that is\n\
         \x20 certain rather than calibrated, which is why this gates."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::ImportFact;
    use crate::lang::Lang;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn file(path: &str, imports: &[&str]) -> GraphFacts {
        GraphFacts {
            path: PathBuf::from(path),
            lang: Lang::TypeScript,
            is_test: false,
            imports: imports
                .iter()
                .map(|t| ImportFact {
                    target: (*t).into(),
                    names: Vec::new(),
                    reach: crate::lang::Reach::Anywhere,
                })
                .collect(),
            exports: Vec::new(),
            receiver_units: Vec::new(),
            mass: 10,
            surface_cost: 1,
        }
    }

    fn contract() -> HashMap<String, Layer> {
        HashMap::from([
            (
                "gallery".to_string(),
                Layer {
                    paths: vec!["src/gallery".into()],
                    may_import: vec!["shared".into()],
                },
            ),
            (
                "playground".to_string(),
                Layer {
                    paths: vec!["src/playground".into()],
                    may_import: vec!["shared".into()],
                },
            ),
            (
                "shared".to_string(),
                Layer {
                    paths: vec!["src/shared".into()],
                    may_import: vec![],
                },
            ),
        ])
    }

    #[test]
    fn a_product_may_reach_shared_but_never_its_sibling() {
        let files = vec![
            file(
                "src/gallery/view.ts",
                ["../shared/http", "../playground/run"].as_ref(),
            ),
            file("src/playground/run.ts", ["../shared/http"].as_ref()),
            file("src/shared/http.ts", [].as_ref()),
        ];
        let found = breaches(&contract(), &files);
        assert_eq!(found.len(), 1, "only the sibling import is forbidden");
        assert_eq!(found[0].from_layer, "gallery");
        assert_eq!(found[0].to_layer, "playground");
    }

    #[test]
    fn shared_may_not_reach_back_and_undeclared_files_are_unjudged() {
        // The dependency runs one way: shared importing a product is a
        // breach even though the product may import shared.
        let files = vec![
            file("src/shared/http.ts", ["../gallery/view"].as_ref()),
            file("src/gallery/view.ts", [].as_ref()),
        ];
        assert_eq!(breaches(&contract(), &files).len(), 1);
        // A file in no declared layer is never judged: the contract
        // says nothing about it, so neither does this.
        let files = vec![
            file("scripts/build.ts", ["../src/gallery/view"].as_ref()),
            file("src/gallery/view.ts", ["../../scripts/build"].as_ref()),
        ];
        assert!(breaches(&contract(), &files).is_empty());
    }

    #[test]
    fn no_contract_means_no_claim() {
        let files = vec![
            file("src/gallery/view.ts", ["../playground/run"].as_ref()),
            file("src/playground/run.ts", [].as_ref()),
        ];
        assert!(breaches(&HashMap::new(), &files).is_empty());
    }
}
