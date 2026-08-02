# elegance

Fast source-code analysis that measures clarity, simplicity and elegance —
to keep code pristine, duplication-free and easy to follow. The bar is
Wozniak / Jane Street-grade code: minimal machinery, obvious structure,
nothing you have to squint at.

Parses code with [tree-sitter](https://tree-sitter.github.io/) and reports
per-function metrics. Written in Rust for speed on large codebases.

## Usage

```sh
elegance [paths...] [--top N]      # files or directories, defaults to .
elegance --json [paths...]         # versioned machine output (schema 1),
                                   #   complete violation list for CI/baselines
elegance --explain file[:line]     # per-construct score breakdown
elegance --baseline write          # record today's violations as the ledger
elegance --baseline check          # exit 1 on NEW or WORSENED violations
elegance --diff HEAD               # judge only what this change touched
elegance --fail-on RUNG            # which rungs may block (default 2)
elegance install-hook              # pre-commit hook running --diff HEAD
elegance --sarif [paths...]        # SARIF 2.1.0 for code scanning / PR annotations
elegance --history record|show     # trend ledger: are we getting better?
elegance --hotspots                # rank complexity by how often it is edited
elegance --by                      # roll findings up per directory, worst first
elegance --coupling                # undeclared co-change and sole authorship
elegance --deps                    # look inside the dependencies you did not write
elegance --helm                    # values-overlay drift and credentials in YAML
elegance --render                  # render each environment, measure what ships
elegance --context [paths...]      # the repo's measured style, for a writer
                                   #   to read BEFORE producing code
elegance calibrate <gold-dirs>     # re-derive budgets from a gold corpus
```

The ratchet is the deployment model: old sludge is tolerated until
touched, new sludge is blocked. Identity is (metric, path, qualified
unit) — line numbers never break the baseline — and the baseline stores
each violation's magnitude, so a unit already in the ledger cannot
quietly get worse. Only rungs 0-2 gate by default.

Teams start permissive and tighten: `--fail-on 0` blocks token hygiene
only, `--fail-on 2` is the default, and findings above the threshold are
always reported, never blocking.

```yaml
# .github/workflows/quality.yml
- run: elegance --baseline check .          # blocks new or worsened sludge
- run: elegance --diff origin/main .        # annotates what the PR touched
- run: elegance --sarif . > elegance.sarif   # then upload-sarif for PR annotations
```

Reports tail distributions per metric (a codebase is as bad as the code
you read most often — means hide monsters) and lists only budget
violations, so clean code reports quietly:

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

`--explain` makes every score an explanation, not a verdict:

```
gnarly  sample.py:17  cognitive 31  cyclomatic 14  depth 6
  L19    loop       cognitive +1  cyclomatic +1
  L24    if         cognitive +5  cyclomatic +1  (1 + nesting 4)
  ...
```

## Configuration

`.elegance.toml` at the scan root:

```toml
exclude = ["migrations/**", "*_pb2.py"]  # glob excludes
skip_dirs = ["fixtures"]                 # extra junk-dir names
[budgets]                                # per-repo taste overrides
length = { hi = 100 }
"comment ratio" = { lo = 0.05, hi = 0.5 }
```

Files with generated-code markers (`@generated`, `DO NOT EDIT`, ...) are
skipped entirely — machine-written code is content, not craft. `vendor/`,
`third_party/`, `node_modules/` and friends are always skipped.

## Metrics

Organized by ladder rung, because the rung is what decides what a finding
can *do*: **0-2 are violations** and may gate CI, **3-4 are suspicions**
worth a human look, **5-6 are distributional reports** that never gate,
**7 is paired evidence** for a human to weigh.

**Rung 0 — token hygiene.** `magic numbers`: unnamed non-trivial literals
outside constant contexts (K&P; McConnell ch. 12). Test bodies are exempt
— expected values *are* the test.

**Rung 1 — expression shape.** `expr depth` (tallest single-line
expression tree: the clever-one-liner detector, so multi-line formatting
is rewarded), `demeter` (attribute chains ≥3 data links; fluent call
chains exempt, `self` forgives one link), `negations` (double negatives,
negated negative-polarity names, De Morgan candidates).

**Rung 2 — function shape.** `conditional hook` (React identifies a hook
by the *order* it is called in, so one reached through a branch
renumbers every hook after it the moment the condition flips, and the
component reads another hook's state — a corruption, not a style
question). It fires only inside a component or a custom hook, in a file
that imports React: a lowercase factory returning hooks is not one, and
a `useX()` helper in an unrelated codebase is a function with a name.
Solid's `createSignal`/`createEffect` are deliberately exempt — Solid
tracks dependencies at run time, so a conditional one is legal there.
Plus `cognitive` (SonarSource semantics:
structural constructs cost 1 + nesting, `elif`/`else`/filters a flat 1,
boolean operators 1 per new sequence, direct recursion 1), `cyclomatic`
(decision points + 1 — kept as McCabe meant it, a minimum test count),
`depth`, `length`, `params`, `live span` (McConnell ch. 13: a live
variable is a mental register), `swallowed` (a handler that silences the
error entirely).

**Rung 3 — interface shape.** `returns` (four values travelling
together are a struct in hiding — the `params` argument pointed at the
other end of the signature. Go reads its result list; Rust and TS a
declared tuple return, unwrapping exactly one generic level, because
`Result<(A,B,C), E>` and `Promise<[A,B]>` ship their tuples and the
wrapper is not one of the values — without that the whole async half
of TypeScript read as a single value. `Vec<(A,B)>` and `Array<[A,B]>`
stay at one: a list *of* tuples is one value however many it holds.
Python takes the wider of its `-> tuple[...]` annotation and the
tuples its `return`s ship, and treats a variadic `tuple[int, ...]` as
the sequence it is. A JS array or an OCaml tuple is already one value
and stays silent. A
suspicion rather than a gate because a TS tuple annotation is
*optional*: its budget rides on how often gold annotates at all, and a
declared `[value, setter]` pair is legitimate style), `interface width`
(methods per declared interface — Go's "the bigger the interface, the
weaker the abstraction" as one number. Methods only: a TS props shape
is a record, and an embedded interface or extends clause is
composition, the cure for width, never billed as the disease. The
widest in gold is JQuery at 310, and Go gold's median width of 1 says
the small-interface culture is real), `repurposed` (a straight-line
`x = ...` whose new value never mentions the old gives the same name a
second meaning — Fowler's Split Variable, the one def-use insight
cheap at syntax cost. Collecting updates, compound operators,
conditional overrides, loop refills, try-sheltered fills, swaps,
member writes, and Rust let-shadowing are all exempt by construction;
tests are exempt because a test refills one variable per scenario —
gold said so at 82 refills inside redis's own test driver), `flag
params`, `kw opacity`, `pass-through`
(Ousterhout's shallow wrapper / Fowler's Middle Man), `generic name`,
`lying name` (`is_`/`has_` must return bool; `get_` must not mutate),
`broad catch`, `unwraps`, `spooky` (eval, computed attribute access,
metaclasses, transmute), `echo comments`, `comment ratio` (two-sided in
principle; gold p05 is 0% everywhere, so the "too few" flank is vacuous
by the corpus's own verdict), `test asserts`, `lazy test name`, and the
type-hygiene family: `untyped params`, `loose types`, `casts`,
`suppressions`.

**Rung 4 — class and module cohesion.** `and name` (a conjunction
confesses two responsibilities), `feature envy` (a method living in
another object's data belongs there).

**Rung 5 — dependency graph and rates.** Reported, never gated: cycle
mass at file and directory granularity, dependency depth, deletability,
blast radius, orphans, interface depth (Parnas: hide a lot behind a
little), fat surfaces, encapsulation leaks, `dead exports`, and
step-down narrative ordering. Plus the codebase-wide recurrences —
`clones` (Type-2 structural duplication via normalized Merkle hashing;
only *logic* qualifies, since duplicated data tables are content),
`near-clones` (winnowed fingerprints catching *edited* copies, with the
cores suppressed above the idiom cap counted rather than silenced),
`param clumps` (Fowler's Data Clumps), `repeated dispatch`, and
`untested complexity`. And the coverage RATES — `public docs` and
`asserts` render as shares beside gold's own share ("63% of 103 public
units documented; admired rs: 84%"), never as per-unit findings:
admired code fails the per-unit claims 81% and 90% of the time, and a
suspicion the gold corpus fails nine times in ten is a distributional
fact wearing the wrong rung.

**Outside the ladder — `--helm`.** A Helm chart *looks* like an import
graph — `values.yaml` declares keys, templates reference them — and
treating it as one is a category error. Measured on a real chart, that
naive analysis produced 48 findings and **zero were true**: keys reached
through `range $name, $deploy`, through `index .root.Values .svc`,
mentioned only inside a comment, or guarded by `| default`. Helm is a
template language with aliasing, computed access and helper
indirection — the same "text stops predicting the run" property
`Sem::Spooky` names in code.

So this tier reports only what needs no template evaluation: **overlay
drift** (which keys each environment sets, by set comparison — one
production region setting 22 keys where another sets 55 is a fact, and
whether the fallback was intended is judgment, which is why it never
gates) and **credentials in YAML scalars**, using the same tested rules
the extractor applies to source rather than a second set that drifts
from them.

**Outside the ladder — `--render`.** The way past a template you cannot
evaluate is not a cleverer analysis; it is to stop analysing the
template. `helm template` resolves every indirection by running it, and
`--render` measures the manifests that come back: which resources each
environment actually ships (a values diff cannot tell you this — a
resource may appear or vanish through a conditional), and credentials a
template injected from its values, which are invisible in the template
and the values file alike. On a real four-region chart with 35 keys of
values drift, every environment renders the same 19 resources — which
is worth knowing and nothing else could say. Report-only; a manifest is
not code and never moves a language budget.

**Outside the ladder — `--deps`.** The dependency tree is where a
supply-chain problem hides, and the ordinary scan prunes it by design.
This mode looks anyway and reports only what matters about code you
cannot change: credentials compiled into it, action at a distance,
checker suppressions, volume per package, and logic it shares with your
own tree (a vendored copy, or your code copied out of a dependency).
Every unit-shape metric is deliberately absent — a dependency's
cognitive complexity is trivia, not a finding. One kiosk frontend
measures 47k lines of its own against **3.97M lines across 575
packages**.

**Rung 6 — evolution.** `--hotspots` (churn × complexity: Tornhill),
`--history` (are we getting better), `--by` (which directory is in
trouble).

Budgets are `[lo, hi]` bands, **per language**, pinned to gold-corpus
percentiles (`calibration.toml`, regenerated by `./gold-fetch.sh &&
elegance calibrate /tmp/gold` and baked in at build time). One-sided
budgets take gold p99; bands take p05/p95; policy metrics encode taste
and are never calibrated — but the calibrate audit reports any policy
the corpus itself violates (1% ceiling for gates, 5% for suspicions).
What the gold data says: cognitive p99 lands at py 22, rs 17, ts 32;
Rust's doc culture pushes its comment ceiling to 75%. (A test pins this
paragraph to `calibration.toml` — the README cannot drift from the
corpus.) Every metric carries a quality-ladder rung
(0 token hygiene, 1 expression, 2 function, 3 interface, 4 class/module,
5 dependency graph, 6 evolution, 7 tensions); the verdict type derives
from the rung — rungs 0-2 are CI-gateable violations, 3-4 suspicions,
5-6 distributional reports, 7 paired evidence for humans. Violations
feed the worst-offender lists with exact `file:line` locations.

## Architecture

```
src/
  sem.rs      closed semantic ontology (Sem) + what each category means to metrics
  lang/       language packs: dense kind_id -> Sem tables + tiny hooks per grammar
  facts/      FileFacts/UnitFacts + single-pass extractor (the waist of the system)
  metrics/    registry with budget bands + pure functions over facts
  report/     distribution aggregation, clone classes, offender lists, rendering
  main.rs     CLI, gitignore-aware walk, rayon fold/reduce
```

Language packs lower tree-sitter CSTs into language-agnostic facts;
metrics never see a syntax tree. Adding a language = one kind table plus
four small hooks; adding a metric = one function over facts. One
bottom-up pass per file computes control events, clone fingerprints and
expression heights together.

A cross-language conformance suite pins the ontology: the same function
written in every supported language must produce identical metrics.

## Languages

Python, Rust, TypeScript, TSX, Go, JavaScript, Zig, C, OCaml, shell.
Next: C++.

`.vue` single-file components are read as what they contain. A
component is a container, not a language: its `<script>` blocks are
ordinary TypeScript or JavaScript, and everything outside them is
replaced with blank lines, so the pack reads real code at line numbers
that are already true — `--explain`, `--diff` and the baseline need no
offset bookkeeping and no new grammar. Template expressions
(`:prop="expr"`) are out of scope in this tier.

**GitHub Actions and Dockerfiles are containers too.** A `run:` block
and a `RUN` line are shell scripts that deploy, build and hold
credentials, reviewed less than any source file because the file they
live in is "configuration". They are blanked the same way and read by
the shell pack at true line numbers. Only `.github/workflows/` — a
Helm chart is also YAML and belongs to `--helm`. Dockerfile `RUN` in
shell form only, since the exec form is a JSON array that never
reaches a shell; `ENV` and `ARG` keep their `NAME=value` bodies, which
are already shell assignments, so a Dockerfile's baked-in secrets are
read by the same detector that reads source.

Shell is the language that provisions production and the one nothing
was measuring: a single Kubernetes deployment repository here holds
22.6k lines of it, including a 3,287-line registry provisioner. A shell
function declares no parameters (`$1` is read from the caller's frame),
so the whole interface family is structurally silent — a fact about the
language, recorded in the parity matrix rather than left to look like a
gap. `.sh`/`.bash` only: extensionless scripts with shebangs are real,
but `Lang::from_path` is a pure path predicate the walk calls on every
file, and sniffing would change what a walk costs.

OCaml is in the corpus as a control group. Its gold reads
cognitive p99 = 6 and length p99 = 52, against admired Python's 22/80,
Rust's 17/98 and TypeScript's 32/108 — three to five times as tight on
complexity and up to twice on function length. Idiomatic OCaml iterates
with `List.iter` and a lambda, which the ontology reads as a call rather
than a loop, so loop-based metrics read low for it by construction.

C caveat: sources are parsed without preprocessing. Function-like macros
read as calls (fine), but macro *bodies* are invisible and linkage-macro
prefixes (`LUA_API void f(...)`) or operator-argument macros (`intop(+,
a, b)`) produce local parse errors — macro-heavy files fall below the
confidence bar and are excluded from metrics, visibly counted in the
report header. C numbers are floors, not truths, for `.h` tricks.
`#if`/`#elif`/`#else` count as real branches: conditional compilation is
control flow the reader must follow.

## Performance

Measured on the gold corpus: **6.39M lines / 24.8k files / 325k units in
34s, 1.4 GB peak RSS** (single machine, release build, 2026-08). The
near-clone tier — winnowing fingerprints for every production unit —
joined after the earlier 4.8s/1.0GB figure this paragraph used to carry,
and the corpus itself grew by a third. Facts are transient per file;
what survives is per-unit metric values, clone sites, fingerprints, the
identifier vocabulary and bounded offender lists.

Memory, not speed, is the scaling limit, and it is roughly linear in
lines — so budget about 2 GB for a 10M-line tree rather than assuming it
is free. Two things were done about it and one was undone:

- clone-site paths are shared (`Arc<str>`), not cloned per site: 1.11 GB → 0.96 GB
- the blast-radius ancestor matrix is computed 64 components at a time,
  turning `ncomp²/8` bytes into `ncomp·8` — 312 MB → 400 KB at 50k modules
- storing a clone class's first site inline to spare singletons a `Vec`
  allocation measured 7% **worse** and was reverted: widening the struct
  grows hashbrown's table across millions of entries by more than the
  allocations it saves

## Roadmap

- shell, Vue SFC (script-block delegation) and C++ packs
- config tier: Helm values-overlay drift, rendered-manifest analysis
- `--deps`: secrets/spooky/suppressions in the dependencies you didn't write
- an aggregate gold-relative summary (maybe — the ladder resists a single score)
