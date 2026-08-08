//! What a change does to the public surface.
//!
//! The ratchet asks whether a change made the code worse. This asks a
//! different question the same evidence can answer: whether it made the
//! code *incompatible*. A removed export and a widened signature both
//! compile locally and both break somebody downstream, and neither is
//! visible in any complexity number.
//!
//! Only changed files are read. An untouched file cannot have moved its
//! own API, so a pull request costs a handful of `git show` calls rather
//! than a second scan of the tree.

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use std::process::Command;

use crate::facts::UnitFacts;
use crate::lang::Lang;

/// Findings shown before the list stops being read.
const SHOW: usize = 20;

/// What a caller depends on. Names are part of it: a keyword argument is
/// a call site in Python and TypeScript alike.
#[derive(PartialEq)]
struct Signature {
    params: Vec<Box<str>>,
    types: Vec<Box<str>>,
    /// Optional at the call site, per parameter. An ADDED optional
    /// breaks nobody, which is the most common non-breaking change.
    optional: Vec<bool>,
    returns: Box<str>,
}

enum Break {
    Removed,
    /// Callers that satisfied the old signature no longer satisfy this.
    Narrowed(String),
}

struct Finding {
    unit: String,
    path: String,
    what: Break,
}

/// Returns the process exit code: 1 when the surface broke.
pub fn run(reference: &str, root: &Path) -> Result<i32, Box<dyn Error>> {
    let changed = changed_paths(root, reference)?;
    let mut findings = Vec::new();
    let mut checked = 0u32;
    for path in &changed {
        if Lang::from_path(Path::new(path)).is_none() {
            continue;
        }
        checked += 1;
        // Each side is read as the language ITS OWN text is: a header
        // that gained C++ this commit changes surface in both dialects,
        // and judging the old text by the new dialect would invent one.
        let dialect =
            |src: &str| Lang::of_source(Path::new(path), src).expect("extension already accepted");
        let before = at_ref(root, reference, path).map(|src| surface(dialect(&src), path, &src));
        let after = std::fs::read_to_string(root.join(path))
            .ok()
            .map(|src| surface(dialect(&src), path, &src));
        compare(before, after, path, &mut findings);
    }
    report(reference, checked, &findings);
    Ok(i32::from(!findings.is_empty()))
}

/// A file that was deleted has no new surface; one that was added has no
/// old surface and cannot break anything.
fn compare(
    before: Option<HashMap<String, Signature>>,
    after: Option<HashMap<String, Signature>>,
    path: &str,
    findings: &mut Vec<Finding>,
) {
    let Some(before) = before else { return };
    let after = after.unwrap_or_default();
    for (unit, was) in before {
        let Some(what) = verdict(&was, after.get(&unit)) else {
            continue;
        };
        findings.push(Finding {
            unit,
            path: path.to_string(),
            what,
        });
    }
    findings.sort_by(by_location);
}

/// Findings read as a file-by-file list, so a reviewer walks the change
/// the way they would walk the diff.
fn by_location(a: &Finding, b: &Finding) -> std::cmp::Ordering {
    a.path.cmp(&b.path).then_with(|| a.unit.cmp(&b.unit))
}

/// What became of one export: gone, narrowed, or still compatible.
fn verdict(was: &Signature, now: Option<&Signature>) -> Option<Break> {
    let Some(now) = now else {
        return Some(Break::Removed);
    };
    if now == was {
        return None;
    }
    narrowing(was, now).map(Break::Narrowed)
}

/// Why a caller that satisfied the old signature no longer satisfies the
/// new one. `None` when the change only widens what is accepted.
fn narrowing(was: &Signature, now: &Signature) -> Option<String> {
    if now.params.len() > was.params.len() {
        // `def f(a, b=1)` from `def f(a)` breaks nobody; a REQUIRED
        // addition breaks every existing call site.
        let required: Vec<&str> = now.params[was.params.len()..]
            .iter()
            .zip(&now.optional[was.params.len()..])
            .filter(|(_, optional)| !**optional)
            .map(|(p, _)| &**p)
            .collect();
        if !required.is_empty() {
            return Some(format!(
                "gained required parameter(s) {}",
                required.join(", ")
            ));
        }
    }
    if now.params.len() < was.params.len() {
        let dropped = &was.params[now.params.len()..];
        let names: Vec<&str> = dropped.iter().map(|p| &**p).collect();
        return Some(format!("dropped parameter(s) {}", names.join(", ")));
    }
    // Shared prefix: a renamed parameter breaks keyword callers, a
    // changed type breaks everyone, and a parameter that WAS optional
    // becoming required breaks every caller who omitted it.
    for (i, (old_name, new_name)) in was.params.iter().zip(&now.params).enumerate() {
        let (old_ty, new_ty) = (&was.types[i], &now.types[i]);
        if old_ty != new_ty && !old_ty.is_empty() && !new_ty.is_empty() {
            return Some(format!("{old_name}: {old_ty} -> {new_ty}"));
        }
        if old_name != new_name {
            return Some(format!("parameter renamed {old_name} -> {new_name}"));
        }
        if was.optional[i] && !now.optional[i] {
            return Some(format!("parameter {new_name} became required"));
        }
    }
    (was.returns != now.returns && !was.returns.is_empty())
        .then(|| format!("returns {} -> {}", was.returns, now.returns))
}

/// The public units of one source text, by qualified name.
fn surface(lang: Lang, path: &str, source: &str) -> HashMap<String, Signature> {
    let pack = lang.pack();
    let facts = crate::facts::extract(pack, &mut pack.make_parser(), Path::new(path), source);
    if facts.low_confidence() {
        return HashMap::new();
    }
    facts
        .units
        .iter()
        .filter(|u| u.is_public && !u.is_module && !u.is_test)
        .map(|u| (u.qualname.to_string(), signature(u)))
        .collect()
}

fn signature(u: &UnitFacts) -> Signature {
    Signature {
        params: u.params.iter().map(|p| p.name.clone()).collect(),
        types: u.params.iter().map(|p| p.type_name.clone()).collect(),
        optional: u.params.iter().map(|p| p.optional).collect(),
        returns: u.returns.clone(),
    }
}

fn report(reference: &str, checked: u32, findings: &[Finding]) {
    if findings.is_empty() {
        println!("api vs {reference} — {checked} source files changed, surface intact");
        return;
    }
    println!(
        "api vs {reference} — {} breaking change(s) to the public surface:",
        findings.len()
    );
    for f in findings.iter().take(SHOW) {
        let what = match &f.what {
            Break::Removed => "removed".to_string(),
            Break::Narrowed(why) => why.clone(),
        };
        println!("  {}  {}  {what}", f.path, f.unit);
    }
    if findings.len() > SHOW {
        println!("  ... and {} more", findings.len() - SHOW);
    }
}

fn changed_paths(root: &Path, reference: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only", reference])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git diff {reference}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// One file's contents at a revision. Absent means the file did not
/// exist there, which is an addition rather than a break.
fn at_ref(root: &Path, reference: &str, path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &format!("{reference}:{path}")])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).ok())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(params: &[&str], types: &[&str], returns: &str) -> Signature {
        Signature {
            params: params.iter().map(|p| (*p).into()).collect(),
            types: types.iter().map(|t| (*t).into()).collect(),
            optional: params.iter().map(|_| false).collect(),
            returns: returns.into(),
        }
    }

    #[test]
    fn a_widened_signature_is_not_a_break_but_a_narrowed_one_is() {
        let two = sig(&["host", "port"], &["str", "int"], "Conn");
        // Losing a parameter breaks every caller that passed it.
        assert!(
            narrowing(&two, &sig(&["host"], &["str"], "Conn")).is_some(),
            "dropping a parameter breaks callers"
        );
        // Gaining a required one breaks every existing call site.
        let three = sig(&["host", "port", "tls"], &["str", "int", "bool"], "Conn");
        assert!(narrowing(&two, &three).is_some());
        // A renamed parameter breaks keyword callers even at same arity.
        let renamed = sig(&["hostname", "port"], &["str", "int"], "Conn");
        assert_eq!(
            narrowing(&two, &renamed),
            Some("parameter renamed host -> hostname".to_string())
        );
        // A changed type breaks everyone.
        let retyped = sig(&["host", "port"], &["str", "str"], "Conn");
        assert_eq!(
            narrowing(&two, &retyped),
            Some("port: int -> str".to_string())
        );
        // Identical is silence.
        assert!(narrowing(&two, &sig(&["host", "port"], &["str", "int"], "Conn")).is_none());
    }

    #[test]
    fn an_added_optional_parameter_breaks_nobody() {
        // The most common non-breaking API change there is, and the one
        // a surface check must stay silent about.
        let two = sig(&["host", "port"], &["str", "int"], "Conn");
        let mut widened = sig(&["host", "port", "tls"], &["str", "int", "bool"], "Conn");
        widened.optional[2] = true;
        assert!(
            narrowing(&two, &widened).is_none(),
            "an added default/optional accepts every old call"
        );
        // The reverse direction still breaks: dropping it strands the
        // callers who passed it...
        assert!(narrowing(&widened, &two).is_some());
        // ...and making an existing optional required strands the
        // callers who omitted it.
        let mut relaxed = two_with_optional_port();
        let strict = sig(&["host", "port"], &["str", "int"], "Conn");
        assert_eq!(
            narrowing(&relaxed, &strict),
            Some("parameter port became required".to_string())
        );
        // Optional-ward movement is a widening, and stays silent.
        relaxed = strict.clone_like();
        let mut now_optional = sig(&["host", "port"], &["str", "int"], "Conn");
        now_optional.optional[1] = true;
        assert!(narrowing(&relaxed, &now_optional).is_none());
    }

    fn two_with_optional_port() -> Signature {
        let mut s = sig(&["host", "port"], &["str", "int"], "Conn");
        s.optional[1] = true;
        s
    }

    impl Signature {
        fn clone_like(&self) -> Signature {
            Signature {
                params: self.params.clone(),
                types: self.types.clone(),
                optional: self.optional.clone(),
                returns: self.returns.clone(),
            }
        }
    }

    #[test]
    fn optionality_is_read_from_the_source_itself() {
        let found = surface(
            Lang::Python,
            "a.py",
            "def api(host, retries=3, *rest):\n    return host\n",
        );
        assert_eq!(
            found["api"].optional,
            [false, true, true],
            "defaults and splats are optional at every call site"
        );
        let ts = surface(
            Lang::TypeScript,
            "a.ts",
            "export function api(host: string, tls?: boolean, retries = 3) { return host; }\n",
        );
        assert_eq!(ts["api"].optional, [false, true, true]);
    }

    #[test]
    fn an_added_file_cannot_break_anything() {
        let mut findings = Vec::new();
        let after = HashMap::from([("f".to_string(), sig(&[], &[], ""))]);
        compare(None, Some(after), "new.py", &mut findings);
        assert!(findings.is_empty(), "no old surface, nothing to break");
    }

    #[test]
    fn a_deleted_export_is_reported_even_when_the_file_is_gone() {
        let mut findings = Vec::new();
        let before = HashMap::from([("gone".to_string(), sig(&[], &[], ""))]);
        compare(Some(before), None, "old.py", &mut findings);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].what, Break::Removed));
    }

    #[test]
    fn the_public_surface_excludes_what_callers_cannot_reach() {
        let src = "def api(host, port):\n    return 1\n\ndef _internal(x):\n    return x\n";
        let found = surface(Lang::Python, "a.py", src);
        let mut names: Vec<&str> = found.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["api"], "underscore names are not surface");
        assert_eq!(found["api"].params, ["host".into(), "port".into()]);
    }
}
