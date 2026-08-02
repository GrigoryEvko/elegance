//! Near-clones: the same code with edits in the middle.
//!
//! The Merkle detector in `facts` finds Type-2 clones — identical
//! structure, renamed identifiers. It is exact and it is blind to one
//! inserted line, which is what a copy-paste usually becomes within a
//! week. Type-3 detection has to survive insertions and deletions.
//!
//! Winnowing (Schleimer, Wilkerson & Aiken, SIGMOD 2003, the algorithm
//! behind MOSS): hash every k-gram of the normalized token stream, then
//! in each window of w consecutive hashes keep the smallest. That gives
//! a fingerprint set with two properties worth the trouble — density is
//! bounded, so a long unit costs proportionally little, and the choice
//! is position-independent, so inserting a line changes the fingerprints
//! near the insertion and leaves the rest identical.

use std::collections::HashMap;

/// Tokens per k-gram. Small enough that a short shared passage still
/// registers, large enough that a `for` loop header is not a match.
const K: usize = 5;

/// Winnowing window. Guarantees any shared passage of at least
/// K + W - 1 tokens contributes a fingerprint to both sides.
const W: usize = 4;

/// Units shorter than this are boilerplate — a getter matches every
/// other getter, and saying so is noise.
pub const MIN_TOKENS: usize = 40;

/// Share of the smaller unit's fingerprints that must be shared.
const MIN_OVERLAP: f64 = 0.6;

/// A fingerprint in more units than this is a language idiom, not a
/// clone. Skipping them also keeps the pair search near-linear: the
/// cost is the sum of squared posting-list lengths.
const MAX_POSTINGS: usize = 12;

/// The winnowed fingerprints of one token stream, sorted and unique.
pub fn fingerprints(tokens: &[u64]) -> Vec<u64> {
    if tokens.len() < K + W - 1 {
        return Vec::new();
    }
    let grams: Vec<u64> = tokens.windows(K).map(hash_gram).collect();
    let mut out = Vec::new();
    let mut previous = u64::MAX;
    for window in grams.windows(W) {
        // Rightmost minimum, per the paper: it makes overlapping windows
        // agree on their choice, so one fingerprint is recorded where
        // the naive rule would record several.
        let pick = window
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))
            .map(|(_, h)| *h)
            .expect("window is non-empty");
        if pick != previous {
            out.push(pick);
            previous = pick;
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// splitmix64 constants — the same combiner the Merkle hash uses, so a
/// gram's identity is derived the same way a subtree's is.
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;
const SCRAMBLE: u64 = 0xBF58_476D_1CE4_E5B9;
const SHIFT_HIGH: u32 = 30;
const SHIFT_FINAL: u32 = 31;

fn hash_gram(gram: &[u64]) -> u64 {
    gram.iter().fold(0u64, |h, t| {
        let mut z = h ^ t.wrapping_mul(GOLDEN);
        z = (z ^ (z >> SHIFT_HIGH)).wrapping_mul(SCRAMBLE);
        z ^ (z >> SHIFT_FINAL)
    })
}

/// One unit's fingerprints, with somewhere to point a reader.
pub struct Print {
    pub label: String,
    pub prints: Vec<u64>,
}

pub struct NearPair {
    pub a: String,
    pub b: String,
    pub overlap: f64,
}

/// What the pair search found AND what it refused to look at. The cap
/// exists because pairing cost is the sum of squared posting lengths —
/// but fifteen near-identical handlers share every core fingerprint at
/// posting length fifteen, so the WORST duplication is exactly what the
/// cap suppresses. Suppression without a count is a silent cap, and
/// this tool's own doctrine forbids those.
pub struct NearStats {
    pub pairs: Vec<NearPair>,
    /// Fingerprint cores shared by more units than the idiom cap,
    /// dropped from pairing for cost and counted here for honesty.
    pub suppressed_cores: u32,
    /// The widest such core: how many units share one dropped print.
    pub widest_core: u32,
}

/// Pairs of units sharing most of their fingerprints, strongest first.
///
/// The search is over an inverted index rather than all pairs: a
/// fingerprint's posting list names every unit carrying it, and only
/// units that co-occur somewhere are ever compared.
pub fn pairs(units: &[Print], show: usize) -> NearStats {
    let mut postings: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, u) in units.iter().enumerate() {
        for print in &u.prints {
            postings.entry(*print).or_default().push(i);
        }
    }
    let mut shared: HashMap<(usize, usize), u32> = HashMap::new();
    let mut suppressed_cores = 0u32;
    let mut widest_core = 0u32;
    for list in postings.values() {
        if list.len() > MAX_POSTINGS {
            // An idiom — or mass duplication; either way, counted.
            suppressed_cores += 1;
            widest_core = widest_core.max(list.len() as u32);
            continue;
        }
        for (n, &a) in list.iter().enumerate() {
            for &b in &list[n + 1..] {
                *shared.entry((a, b)).or_insert(0) += 1;
            }
        }
    }
    let mut found: Vec<NearPair> = shared
        .into_iter()
        .filter_map(|((a, b), n)| {
            let floor = units[a].prints.len().min(units[b].prints.len());
            let overlap = n as f64 / floor.max(1) as f64;
            // Ordered by LABEL, not by index: which unit got the lower
            // index is rayon's merge order, and a pair must read the
            // same way whichever thread saw it first.
            let (first, second) = if units[a].label <= units[b].label {
                (a, b)
            } else {
                (b, a)
            };
            (overlap >= MIN_OVERLAP).then(|| NearPair {
                a: units[first].label.clone(),
                b: units[second].label.clone(),
                overlap,
            })
        })
        .collect();
    found.sort_by(strongest_first);
    found.truncate(show);
    NearStats {
        pairs: found,
        suppressed_cores,
        widest_core,
    }
}

fn strongest_first(x: &NearPair, y: &NearPair) -> std::cmp::Ordering {
    y.overlap
        .total_cmp(&x.overlap)
        .then_with(|| x.a.cmp(&y.a))
        .then_with(|| x.b.cmp(&y.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token stream standing in for a unit's normalized shape.
    fn stream(from: u64, len: usize) -> Vec<u64> {
        (0..len as u64).map(|i| from + (i * 7) % 23).collect()
    }

    #[test]
    fn an_inserted_passage_leaves_most_fingerprints_untouched() {
        // This is the whole point: the Merkle detector sees a different
        // tree and reports nothing, while winnowing sees the rest match.
        let original = stream(0, 200);
        let mut edited = original.clone();
        edited.splice(90..90, stream(500, 12));

        let a = fingerprints(&original);
        let b = fingerprints(&edited);
        let shared = a.iter().filter(|f| b.contains(f)).count();
        let floor = a.len().min(b.len());
        assert!(
            shared as f64 / floor as f64 > 0.8,
            "an edit in the middle moved {shared} of {floor} fingerprints"
        );
    }

    /// A stream with a different period, so the two share no long run.
    fn other(len: usize) -> Vec<u64> {
        (0..len as u64).map(|i| 1000 + (i * 13) % 31).collect()
    }

    #[test]
    fn unrelated_streams_do_not_match() {
        let a = fingerprints(&stream(0, 200));
        let b = fingerprints(&other(200));
        let shared = a.iter().filter(|f| b.contains(f)).count();
        assert!(shared * 2 < a.len().min(b.len()), "{shared} shared");
    }

    #[test]
    fn a_stream_too_short_to_winnow_yields_nothing() {
        assert!(fingerprints(&stream(0, K + W - 2)).is_empty());
        assert!(!fingerprints(&stream(0, K + W - 1)).is_empty());
    }

    #[test]
    fn pairs_report_the_overlap_and_skip_the_unrelated() {
        let twin = |label: &str, from: u64| Print {
            label: label.to_string(),
            prints: fingerprints(&stream(from, 200)),
        };
        let mut edited = stream(0, 200);
        edited.splice(90..90, stream(500, 12));
        let units = vec![
            twin("a.rs:1 alpha", 0),
            Print {
                label: "b.rs:1 beta".to_string(),
                prints: fingerprints(&edited),
            },
            Print {
                label: "c.rs:1 gamma".to_string(),
                prints: fingerprints(&other(200)),
            },
        ];
        let found = pairs(&units, 10);
        assert_eq!(found.pairs.len(), 1, "only the edited twin pairs");
        assert_eq!(found.pairs[0].a, "a.rs:1 alpha");
        assert_eq!(found.pairs[0].b, "b.rs:1 beta");
        assert!(found.pairs[0].overlap > 0.8);
        assert_eq!(found.suppressed_cores, 0, "three units cap nothing");
    }

    #[test]
    fn mass_duplication_above_the_cap_is_counted_not_silenced() {
        // Fifteen near-copies share every core fingerprint at posting
        // length fifteen — above MAX_POSTINGS, so no pairs form. That
        // suppression used to be invisible: fifteen copies reported
        // LESS than five did.
        let many: Vec<Print> = (0..15)
            .map(|i| {
                let mut tokens = stream(0, 200);
                tokens.extend(stream(700 + i * 40, (i as usize) * 3 + 8));
                Print {
                    label: format!("m.rs:{i} handler{i}"),
                    prints: fingerprints(&tokens),
                }
            })
            .collect();
        let found = pairs(&many, 50);
        assert!(
            found.suppressed_cores > 0,
            "the shared core must be counted"
        );
        assert!(found.widest_core >= 13, "widest {}", found.widest_core);
    }
}
