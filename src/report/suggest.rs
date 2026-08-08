//! From finding to edit. Every remedy here is a named refactoring from
//! the literature, instantiated with facts we already hold. Nothing here
//! is invented advice. Metrics whose remedy depends on intent the tool
//! cannot see (comment ratio, test assertions) get no suggestion at all:
//! filler advice is worse than silence, because it trains readers to
//! skip the line.

use crate::facts::UnitFacts;
use crate::metrics;

/// Remedies that depend only on WHICH metric fired. A table, not a
/// match: a dispatch of twenty arms is a lookup wearing control flow.
/// Metrics absent here are deliberately unsuggested: comment ratio,
/// echo comments, spooky and test assertions all need intent we cannot
/// read, and filler advice trains readers to skip the line.
#[rustfmt::skip]
const REMEDIES: &[(usize, &str)] = &[
    (metrics::COGNITIVE,     "Extract Function: split the decision groups"),
    (metrics::CYCLOMATIC,    "Extract Function: split the decision groups"),
    (metrics::DEPTH,         "Replace Nested Conditional with Guard Clauses"),
    (metrics::LENGTH,        "Extract Function: the body holds several stories"),
    (metrics::EXPR_DEPTH,    "Extract Variable: name the intermediate results"),
    (metrics::DEMETER,       "Hide Delegate: ask the neighbour, don't reach through it"),
    (metrics::NEGATIONS,     "Say what you mean: invert the condition or the name"),
    (metrics::MAGIC_NUMBERS, "Replace Magic Literal with Named Constant"),
    (metrics::PASSTHROUGH,   "Remove Middle Man: this layer forwards without abstracting"),
    (metrics::KW_OPACITY,    "Name the parameters: a public **kwargs states no contract"),
    (metrics::AND_NAME,      "Split function: the name confesses two responsibilities"),
    (metrics::GENERIC_NAME,  "Rename: the name is drawn entirely from junk vocabulary"),
    (metrics::SWALLOWED,     "Handle or propagate: an empty handler makes the error vanish"),
    (metrics::BROAD_CATCH,   "Catch the error you can handle, not every error"),
    (metrics::UNWRAPS,       "Propagate the error instead of panicking"),
    (metrics::LYING_NAME,    "Rename or fix the contract the name promises"),
    (metrics::CONFUSABLE,    "Introduce a newtype per role: adjacent same-typed parameters swap silently"),
    (metrics::PUBLIC_DOCS,   "Document the interface: a public unit's contract is part of it"),
    (metrics::ASSERTS,       "State the invariants: complex logic should assert what it assumes"),
];

/// The refactoring a metric's violation calls for, instantiated from the
/// unit that violated it. `None` where the remedy is not mechanical.
pub fn for_metric(m: usize, u: &UnitFacts) -> Option<String> {
    match m {
        metrics::PARAMS => Some(params_remedy(u)),
        metrics::FLAG_PARAMS => Some(flag_remedy(u)),
        metrics::LIVE_SPAN => Some(live_span_remedy(u)),
        metrics::FEATURE_ENVY => Some(envy_remedy(u)),
        _ => REMEDIES
            .iter()
            .find(|(metric, _)| *metric == m)
            .map(|(_, remedy)| (*remedy).to_string()),
    }
}

/// Parameter names shown in a remedy before it gets unreadable itself.
const NAMES_SHOWN: usize = 6;

fn params_remedy(u: &UnitFacts) -> String {
    let mut names = Vec::new();
    for p in u.params.iter().take(NAMES_SHOWN) {
        names.push(&*p.name);
    }
    let listed = names.join(", ");
    let n = u.params.len();
    format!("Introduce Parameter Object over ({listed}) — {n} parameters is a struct in hiding")
}

fn flag_remedy(u: &UnitFacts) -> String {
    let flags: Vec<&str> = u
        .params
        .iter()
        .filter(|p| p.boolish)
        .map(|p| &*p.name)
        .collect();
    format!(
        "Split function per value of {}: a flag parameter is two functions in one",
        flags.join(", ")
    )
}

fn live_span_remedy(u: &UnitFacts) -> String {
    format!(
        "Split Variable or narrow the scope of `{}`: it stays live for {} lines",
        u.max_live_var, u.max_live_span
    )
}

fn envy_remedy(u: &UnitFacts) -> String {
    format!(
        "Move Method toward `{}`: this unit spends its time in that object's data",
        u.envy_object
    )
}

/// Remedies for the recurrence findings, which are about a SET of sites
/// rather than one unit.
pub fn for_clump(names: &[String], count: u32) -> String {
    format!(
        "Introduce Parameter Object: ({}) travel together through {count} signatures",
        names.join(", ")
    )
}

pub fn for_dispatch(labels: &[String], count: u32) -> String {
    format!(
        "Replace Conditional with Polymorphism: [{}] is dispatched in {count} places, so every new variant is {count} edits",
        labels.join(" | ")
    )
}

pub fn for_clone(sites: usize, mass: u32) -> String {
    format!("Extract Function: {sites} sites share {mass} nodes of identical logic")
}

/// Sanity: a metric that HAS a remedy must name a real refactoring, and
/// the deliberately-silent ones must stay silent.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{ParamFact, UnitFacts};

    fn unit() -> UnitFacts {
        let mut u = UnitFacts::for_test("connect");
        u.params = ["host", "port", "verbose"]
            .iter()
            .map(|n| ParamFact {
                name: (*n).into(),
                boolish: *n == "verbose",
                kw_splat: false,
                optional: false,
                typed: true,
                loose: false,
                type_name: "str".into(),
                destructured: false,
                splat: false,
            })
            .collect();
        u.max_live_var = "conn".into();
        u.max_live_span = 90;
        u.envy_object = "session".into();
        u
    }

    #[test]
    fn mechanical_remedies_name_the_refactoring_and_its_subject() {
        let u = unit();
        assert!(
            for_metric(metrics::PARAMS, &u)
                .unwrap()
                .contains("host, port")
        );
        assert!(
            for_metric(metrics::FLAG_PARAMS, &u)
                .unwrap()
                .contains("verbose")
        );
        assert!(
            for_metric(metrics::LIVE_SPAN, &u)
                .unwrap()
                .contains("`conn`")
        );
        assert!(
            for_metric(metrics::FEATURE_ENVY, &u)
                .unwrap()
                .contains("`session`")
        );
        assert!(
            for_metric(metrics::DEPTH, &u)
                .unwrap()
                .contains("Guard Clauses"),
            "classical name, not a paraphrase"
        );
    }

    #[test]
    fn metrics_whose_remedy_needs_intent_stay_silent() {
        let u = unit();
        for m in [
            metrics::COMMENT_RATIO,
            metrics::ECHO_COMMENTS,
            metrics::SPOOKY,
            metrics::TEST_ASSERTS,
        ] {
            assert!(
                for_metric(m, &u).is_none(),
                "{} must not invent advice",
                crate::metrics::METRICS[m].name
            );
        }
    }
}
