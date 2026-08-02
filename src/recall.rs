//! Recall floors: seeded smells the detectors must keep finding.
//!
//! Precision has two gates — gold-corpus rates and the policy audit —
//! and recall had none: a detector that decays toward silence passes
//! every precision check by finding nothing. Each fixture under
//! `fixtures/recall/` is realistic code with COUNTED smells planted in
//! it, and this suite asserts the totals never drop.
//!
//! Scope: the count-shaped detectors (swallowed, secrets, casts, ...).
//! Distribution metrics (cognitive, length, depth) are pinned by the
//! cross-language conformance suite instead — their recall is the
//! extractor's correctness, not a detector's.
//!
//! Convention: a new detector adds a seed here AND a restraint entry in
//! the metrics RESTRAINT table — one test for firing, one for silence.
//! Fixtures end in `.seed` so no scan ever reads them as source.

use std::path::Path;

use crate::facts::extract;
use crate::lang::Lang;
use crate::metrics::{self, METRICS};

pub(crate) const FIXTURES: &[(&str, Lang, &str)] = &[
    (
        "service.py",
        Lang::Python,
        include_str!("../fixtures/recall/service.py.seed"),
    ),
    (
        "tests/test_service.py",
        Lang::Python,
        include_str!("../fixtures/recall/test_service.py.seed"),
    ),
    (
        "worker.rs",
        Lang::Rust,
        include_str!("../fixtures/recall/worker.rs.seed"),
    ),
    (
        "handler.ts",
        Lang::TypeScript,
        include_str!("../fixtures/recall/handler.ts.seed"),
    ),
    (
        "widget.js",
        Lang::JavaScript,
        include_str!("../fixtures/recall/widget.js.seed"),
    ),
    (
        "store.go",
        Lang::Go,
        include_str!("../fixtures/recall/store.go.seed"),
    ),
    (
        "store_test.go",
        Lang::Go,
        include_str!("../fixtures/recall/store_test.go.seed"),
    ),
    (
        "pool.c",
        Lang::C,
        include_str!("../fixtures/recall/pool.c.seed"),
    ),
    (
        "ring.zig",
        Lang::Zig,
        include_str!("../fixtures/recall/ring.zig.seed"),
    ),
    (
        "base.ml",
        Lang::OCaml,
        include_str!("../fixtures/recall/base.ml.seed"),
    ),
    (
        "useAccount.ts",
        Lang::TypeScript,
        include_str!("../fixtures/recall/useAccount.ts.seed"),
    ),
    (
        "deploy.sh",
        Lang::Shell,
        include_str!("../fixtures/recall/deploy.sh.seed"),
    ),
];

/// The manifest: (fixture, metric, minimum total planted). Sums may
/// legitimately GROW as detectors sharpen; they must never shrink.
/// Together with the parity matrix in lang::conformance, this doubles
/// as liveness evidence: a (metric, language) pair seeded here is
/// PROVEN alive on every test run.
pub(crate) const SEEDS: &[(&str, &str, f32)] = &[
    ("service.py", "blocking async", 2.0),
    ("service.py", "spooky", 2.0),
    ("service.py", "swallowed", 2.0),
    ("service.py", "broad catch", 1.0),
    ("service.py", "magic numbers", 2.0),
    ("service.py", "negations", 2.0),
    ("service.py", "demeter", 1.0),
    ("service.py", "secrets", 1.0),
    ("service.py", "casts", 1.0),
    ("service.py", "suppressions", 1.0),
    ("service.py", "unmanaged", 1.0),
    ("service.py", "kw opacity", 1.0),
    ("service.py", "flag params", 1.0),
    ("service.py", "loose types", 1.0),
    ("service.py", "untyped params", 3.0),
    ("service.py", "lying name", 1.0),
    ("service.py", "wildcard match", 1.0),
    ("service.py", "lost context", 1.0),
    ("tests/test_service.py", "vacuous asserts", 1.0),
    ("tests/test_service.py", "test asserts", 2.0),
    ("worker.rs", "blocking async", 2.0),
    ("worker.rs", "unwraps", 2.0),
    ("worker.rs", "casts", 2.0),
    ("worker.rs", "secrets", 1.0),
    ("worker.rs", "spooky", 1.0),
    ("worker.rs", "dropped tasks", 1.0),
    ("worker.rs", "wildcard match", 1.0),
    ("worker.rs", "negations", 1.0),
    ("worker.rs", "demeter", 1.0),
    ("worker.rs", "flag params", 3.0),
    ("worker.rs", "lying name", 1.0),
    ("worker.rs", "vacuous asserts", 1.0),
    ("worker.rs", "test asserts", 1.0),
    ("handler.ts", "blocking async", 1.0),
    ("handler.ts", "suppressions", 1.0),
    ("handler.ts", "casts", 2.0),
    ("handler.ts", "lost context", 1.0),
    ("handler.ts", "swallowed", 1.0),
    ("handler.ts", "loose types", 1.0),
    ("handler.ts", "wildcard match", 1.0),
    ("handler.ts", "untyped params", 1.0),
    ("handler.ts", "dropped tasks", 1.0),
    ("handler.ts", "negations", 1.0),
    ("handler.ts", "demeter", 1.0),
    ("handler.ts", "flag params", 2.0),
    ("handler.ts", "lying name", 1.0),
    ("handler.ts", "secrets", 1.0),
    ("handler.ts", "spooky", 1.0),
    ("handler.ts", "vacuous asserts", 1.0),
    ("handler.ts", "test asserts", 1.0),
    ("widget.js", "blocking async", 1.0),
    ("widget.js", "suppressions", 1.0),
    ("widget.js", "spooky", 1.0),
    ("widget.js", "swallowed", 1.0),
    ("widget.js", "lost context", 1.0),
    ("widget.js", "secrets", 1.0),
    ("widget.js", "negations", 1.0),
    ("widget.js", "demeter", 1.0),
    ("widget.js", "flag params", 1.0),
    ("widget.js", "dropped tasks", 1.0),
    ("widget.js", "wildcard match", 1.0),
    ("widget.js", "vacuous asserts", 1.0),
    ("widget.js", "test asserts", 1.0),
    ("store.go", "swallowed", 2.0),
    ("store.go", "casts", 1.0),
    ("store.go", "loose types", 1.0),
    ("store.go", "wildcard match", 1.0),
    ("store.go", "secrets", 1.0),
    ("store.go", "lying name", 1.0),
    ("store.go", "negations", 1.0),
    ("store.go", "demeter", 1.0),
    ("store.go", "flag params", 2.0),
    ("store.go", "unwraps", 1.0),
    ("store_test.go", "vacuous asserts", 1.0),
    ("store_test.go", "test asserts", 2.0),
    ("pool.c", "casts", 2.0),
    ("pool.c", "magic numbers", 1.0),
    ("pool.c", "secrets", 1.0),
    ("pool.c", "lying name", 1.0),
    ("pool.c", "negations", 1.0),
    ("pool.c", "unwraps", 1.0),
    ("pool.c", "spooky", 1.0),
    ("pool.c", "wildcard match", 1.0),
    ("pool.c", "demeter", 1.0),
    ("pool.c", "flag params", 2.0),
    ("pool.c", "loose types", 1.0),
    ("ring.zig", "secrets", 1.0),
    ("ring.zig", "casts", 1.0),
    ("ring.zig", "unwraps", 1.0),
    ("ring.zig", "wildcard match", 1.0),
    ("ring.zig", "lying name", 1.0),
    ("ring.zig", "demeter", 1.0),
    ("ring.zig", "flag params", 1.0),
    ("ring.zig", "loose types", 1.0),
    ("ring.zig", "vacuous asserts", 1.0),
    ("ring.zig", "test asserts", 1.0),
    ("base.ml", "untyped params", 2.0),
    ("base.ml", "unwraps", 1.0),
    ("base.ml", "wildcard match", 1.0),
    ("useAccount.ts", "conditional hook", 1.0),
    ("widget.js", "conditional hook", 1.0),
    ("deploy.sh", "secrets", 2.0),
    ("deploy.sh", "spooky", 1.0),
    ("deploy.sh", "negations", 1.0),
    ("deploy.sh", "magic numbers", 1.0),
    ("deploy.sh", "wildcard match", 1.0),
    ("service.py", "repurposed", 1.0),
    ("service.py", "unawaited coroutine", 1.0),
    ("worker.rs", "unawaited coroutine", 1.0),
    ("worker.rs", "repurposed", 1.0),
    ("handler.ts", "repurposed", 1.0),
    ("widget.js", "repurposed", 1.0),
    ("store.go", "repurposed", 1.0),
    ("pool.c", "repurposed", 1.0),
    ("deploy.sh", "repurposed", 1.0),
    ("ring.zig", "repurposed", 1.0),
];

#[test]
fn seeded_smells_stay_found() {
    for (file, lang, src) in FIXTURES {
        let pack = lang.pack();
        let mut parser = pack.make_parser();
        let f = extract(pack, &mut parser, Path::new(file), src);
        assert!(!f.low_confidence(), "{file}: fixture must parse cleanly");
        let mut sums = vec![0f32; METRICS.len()];
        metrics::for_each(&f, |m, v, _, _| sums[m] += v);
        for (seed_file, metric, min) in SEEDS {
            if seed_file != file {
                continue;
            }
            let m = METRICS
                .iter()
                .position(|d| d.name == *metric)
                .expect("seed names a live metric");
            assert!(
                sums[m] >= *min,
                "{file}: {metric} found {} of {min} seeded — recall decayed",
                sums[m]
            );
        }
    }
    for (seed_file, ..) in SEEDS {
        assert!(
            FIXTURES.iter().any(|(f, ..)| f == seed_file),
            "seed names unknown fixture {seed_file}"
        );
    }
}
