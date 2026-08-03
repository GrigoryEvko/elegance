# elegance

Per-function code metrics for twelve languages. Budgets pinned to
gold-corpus p99: the tool reports what deviates from what admired code
does. Tree-sitter parse, Rust, 6.39M lines in 34s.

## Install

```sh
cargo install --git https://github.com/GrigoryEvko/elegance

# Or a released binary (static musl, no glibc dependency):
curl -fsSLO https://github.com/GrigoryEvko/elegance/releases/latest/download/elegance-x86_64-linux-musl
chmod +x elegance-x86_64-linux-musl && sudo mv elegance-x86_64-linux-musl /usr/local/bin/elegance
```

Eight binaries per release, each built natively:

| | Linux (gnu / musl) | macOS | Windows |
| :--- | :--- | :--- | :--- |
| **x86_64** | `x86_64-linux-gnu` · `x86_64-linux-musl` | `x86_64-macos` | `x86_64-windows.exe` |
| **arm64** | `aarch64-linux-gnu` · `aarch64-linux-musl` | `aarch64-macos` | `aarch64-windows.exe` |

Each carries a `.sha256` sidecar and is smoke-tested on this repository
before publishing.

## Claude Code plugin

```
/plugin marketplace add GrigoryEvko/elegance
/plugin install elegance-nudge@elegance
```

Reports what an edit introduced, one line, as it happens. Advisory,
deduplicated per session, silent on clean files.
See [plugins/elegance-nudge](plugins/elegance-nudge/README.md).

## Usage

```sh
elegance [paths...] [--top N]
elegance --json [paths...]
elegance --explain file[:line]
elegance --baseline write
elegance --baseline check
elegance --diff HEAD
elegance --diff 'origin/main...'
elegance --fail-on RUNG
elegance install-hook
elegance --sarif [paths...]
elegance --history record|show
elegance --hotspots
elegance --by
elegance --coupling
elegance --deps
elegance --helm
elegance --render
elegance --context [paths...]
elegance calibrate <gold-dirs>
```

Identity is (metric, path, qualified unit). Line numbers are deliberately
not part of the key. `--baseline check` fails on new or worsened
violations; old sludge is tolerated until touched. Only rungs 0-2 gate
by default; `--fail-on 0` tightens, `--fail-on 4` loosens.

Three dots matter on a pull request. `--diff origin/main` compares
against the branch tip, so every merge into main since you branched reads
as your change. `origin/main...HEAD` compares against the merge base.
`--diff HEAD` is what a pre-commit hook wants.

```yaml
# .github/workflows/quality.yml
- run: elegance --baseline check .
- run: elegance --diff 'origin/main...HEAD' .
- run: elegance --sarif . > elegance.sarif
```

Output reports tail distributions and lists only budget violations:

```
metric             p50     p90     p99     max   budget     violate
cognitive            0       7      34    1088   <=15          3.7%
...
clones — 37 classes, ~4.1% of code duplicated:
  3 sites × mass 412:
      src/a.py:10-52  src/b.py:88-130  ...

worst — cognitive (budget <=15):
    1088  site-packages/.../fastjsonschema_validations.py:157  validate_...
```

`--explain` breaks every score into line-by-line components:

```
gnarly  sample.py:17  cognitive 31  cyclomatic 14  depth 6
  L19    loop       cognitive +1  cyclomatic +1
  L24    if         cognitive +5  cyclomatic +1  (1 + nesting 4)
  ...
```

## Configuration

`.elegance.toml` at the scan root:

```toml
exclude = ["migrations/**", "*_pb2.py"]
skip_dirs = ["fixtures"]
[budgets]
length = { hi = 100 }
"comment ratio" = { lo = 0.05, hi = 0.5 }
```

Files with generated-code markers (`@generated`, `DO NOT EDIT`) are
skipped. `vendor/`, `third_party/`, `node_modules/` and friends always
skipped.

## Metrics

Rung determines verdict: 0-2 gate CI, 3-4 warn, 5-6 report, 7 paired
evidence.

**Rung 0 — token hygiene.** `magic numbers`: unnamed non-trivial
literals outside constant contexts. Test bodies exempt.

**Rung 1 — expression shape.** `expr depth` (tallest single-line
expression tree; multi-line formatting rewarded), `demeter` (attribute
chains >= 3 data links; fluent calls exempt, `self` forgives one),
`negations` (double negatives, negated negative-polarity names, De
Morgan candidates).

**Rung 2 — function shape.** `cognitive` (SonarSource: structural
+1+nesting, elif/else flat 1, boolean sequences 1, recursion 1),
`cyclomatic` (McCabe), `depth`, `length`, `params`, `live span`
(McConnell ch. 13), `swallowed` (handler silencing the error).

`built query`: SQL assembled by interpolation. Judged by where the hole
lands — comparison slot or identifier slot. `IN (${placeholders})` and
`VALUES ${rows}` stay silent: structure whose values travel separately.
Two findings in 6.39M lines, both real.

`conditional hook`: a React hook reached through a branch. React
identifies hooks by call order, so a conditional one renumbers every
hook after it when the condition flips. Fires only inside a component or
custom hook, in a file importing React. Solid exempt (run-time dependency
tracking).

**Rung 3 — interface shape.** `returns` (tuple width; unwraps one
generic level, so `Result<(A,B,C), E>` counts 3 and `Vec<(A,B)>` counts
1; Python takes the wider of annotation and shipped tuples; a suspicion
because TS annotations are optional), `interface width` (methods per
declared interface; Go gold median: 1), `repurposed` (Fowler's Split
Variable; compound operators, conditional overrides, loop refills, swaps,
member writes, let-shadowing exempt), `flag params`, `kw opacity`,
`pass-through` (Ousterhout's shallow wrapper), `generic name`,
`lying name` (`is_`/`has_` must return bool; `get_` must not mutate),
`broad catch`, `unwraps`, `spooky` (eval, computed attribute access,
metaclasses, transmute), `echo comments`, `comment ratio`, `test
asserts`, `lazy test name`. Type hygiene: `untyped params`, `loose
types`, `casts`, `suppressions`.

**Rung 4 — class and module.** `cohesion` (LCOM4: disconnected groups
among a class's methods; one group is cohesive; methods touching no
member excluded), `and name` (conjunction confesses two
responsibilities), `feature envy`.

**Layer contracts.** Declared in `.elegance.toml`, not calibrated:

```toml
[layers.gallery]
paths = ["src/gallery"]
may_import = ["shared"]

[layers.playground]
paths = ["src/playground"]
may_import = ["shared"]

[layers.shared]
paths = ["src/shared"]
```

Not ratcheted. A rule the repository wrote down is wrong on the first
day and on the thousandth.

**Rung 7 — tensions.** Co-occurrence: a unit over budget, untested, in
a heavily-imported file, holding duplicated logic. Three independent
facts must align. Correlated metrics (cognitive, cyclomatic, length,
live span) count as one. No composite score.

**Rung 5 — graph and rates.** Cycle mass, dependency depth, deletability,
blast radius, orphans, interface depth, fat surfaces, encapsulation
leaks, `dead exports`, step-down ordering. `clones` (Type-2 via
normalized Merkle hashing; only logic, not data tables), `near-clones`
(winnowed fingerprints), `param clumps`, `repeated dispatch`, `untested
complexity`. Coverage rates (`public docs`, `asserts`) render as shares
beside gold: "63% of 103 public units documented; admired rs: 84%".

**Rung 6 — evolution.** `--hotspots` (churn x complexity), `--history`,
`--by`.

**Budget notation.** `<=33` is pinned to a gold-corpus percentile.
`<=12.` (trailing dot) rests on a compiled-in default: the corpus held
fewer than 200 samples, or the metric is policy, or gold p99 was zero.
Machine output carries a `budget_source` field. Budgets are `[lo, hi]`
bands, per language. One-sided take p99; two-sided take p05/p95. Policy
metrics encode taste and are never calibrated; the calibrate audit
reports any policy the corpus violates (1% ceiling for gates, 5% for
suspicions). What the gold data says: cognitive p99 lands at py 18, rs 16, ts 32;
Rust's doc culture pushes its comment ceiling to 78%. A test pins these
numbers to `calibration.toml`.

**`--helm`.** A Helm chart looks like an import graph. Treating it as
one produced 48 findings on a real chart and zero were true: keys reached
through `range`, through `index .root.Values`, inside comments, or
guarded by `| default`. Reports only overlay drift (which keys each
environment sets, by set comparison) and credentials in YAML scalars.

**`--render`.** `helm template` resolves every indirection by running
it. `--render` measures the manifests: which resources each environment
ships, and credentials a template injected from its values. On a
four-region chart with 35 keys of values drift, every environment
renders the same 19 resources.

**`--deps`.** Credentials, action at a distance, checker suppressions,
volume, and shared logic in the dependency tree. No unit-shape metrics.
One kiosk frontend: 47k own lines against 3.97M across 575 packages.

## Architecture

```
src/
  sem.rs      closed semantic ontology (Sem)
  lang/       language packs: dense kind_id -> Sem tables + hooks per grammar
  facts/      FileFacts/UnitFacts + single-pass extractor
  metrics/    registry with budget bands + pure functions over facts
  report/     distribution aggregation, clone classes, offender lists
  main.rs     CLI, gitignore-aware walk, rayon fold/reduce
```

Language packs lower tree-sitter CSTs into language-agnostic facts;
metrics never see a syntax tree. Adding a language: one kind table, four
hooks. Adding a metric: one function over facts. One bottom-up pass per
file.

Cross-language conformance suite: the same function in every supported
language must produce identical metrics.

## Languages

Python, Rust, TypeScript, TSX, Go, JavaScript, Zig, C, OCaml, shell,
C++, CUDA.

**Containers.** `.vue` and `.svelte` SFCs: `<script>` blocks are
TypeScript or JavaScript at true line numbers, everything else blanked.
Jupyter notebooks: code cells emitted at their true file line; markdown
cells, output blocks, and IPython magics blanked. GitHub Actions `run:`
blocks and Dockerfile `RUN` lines: shell at true line numbers.

Placement is verified. A cell whose JSON position disagrees with the
raw-text scan refuses the file rather than measuring it at invented
lines.

**Notebooks are not softened.** Against Python budgets, 38 notebooks
read cognitive 4%, cyclomatic 5%, depth 3%. What is elevated: echo
comments 57%, untyped params 16%, commented-out code 13%, repurposed
12%.

**C family.** Extensions do not settle the language. `.h` files
classified by four line-anchored spellings (`namespace`, `template<`,
access specifier, `class` + name): 0 of 1,027 C headers match, 98 of
104 C++ headers do. The six that miss are headers holding no C++. CUDA
classified by `__global__`, `__device__`, `__shared__`, `__constant__`,
`__host__` outside comments.

C is parsed without preprocessing. Macro bodies are invisible;
linkage-macro prefixes produce parse errors. Macro-heavy files fall
below the confidence bar and are excluded, visibly counted. C numbers
are floors. `#if`/`#elif`/`#else` count as real branches.

C++ gold: cognitive p99 = 30 and length p99 = 115, against C's 80 and 205.
Five header-only libraries (fmt, immer, flux, ctre, magic_enum) read 19
alone; adding kakoune and mold (99% parse rate) takes it to 30. RAII
kills `unmanaged` (same reason as Rust). Pure-virtual classes count for
`interface width`. Gtest `TEST(suite, case)` composed as `suite.case`;
Catch2's string-argument form does not parse.

CUDA rides the C++ pack. `__global__` and `__device__` are unnamed
tokens the tree never shows; `__shared__` is a `type_qualifier`;
`<<<grid, block>>>` is already a `call_expression`. One table entry for
a dialect. Gold reads params p99 = 15 against C++'s 5, magic numbers 30 against 10, and length 249 against 115.

OCaml is a control group: cognitive p99 = 6 and length p99 = 52, against
Python's 18/76, Rust's 16/93 and TypeScript's 32/106.

Shell declares no parameters (`$1` read from caller's frame), so the
interface family is structurally silent. `.sh`/`.bash` only.

**Calibration scoping.** A repository speaks only for its declared
language. Cutlass carries 1,115 `.cpp` and 596 `.py` beside its kernels.
Pooled by extension, adding five CUDA repos moved 46 budgets in
languages nobody was editing (Python length: 80 to 252).

## Performance

6.39M lines, 24.8k files, 325k units in 34s, 1.4 GB peak RSS (release
build, 2026-08). Facts are transient per file; what survives is per-unit
metrics, clone sites, fingerprints, vocabulary and bounded offender
lists.

Memory is the scaling limit, roughly linear in lines. Budget 2 GB for
10M lines.

- Clone-site paths shared (`Arc<str>`): 1.11 GB to 0.96 GB.
- Blast-radius ancestor matrix: 64 components at a time, `ncomp^2/8` to
  `ncomp*8` — 312 MB to 400 KB at 50k modules.
- Storing a clone class's first site inline to spare singletons a `Vec`
  allocation measured 7% worse: widening the struct grows hashbrown's
  table by more than the allocations it saves. Reverted.

## Roadmap

- Aggregate gold-relative summary (the ladder resists a single score)
