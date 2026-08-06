# elegance

**Per-function code metrics for twenty-two languages.** Budgets pinned to
gold-corpus p99 — the tool reports what deviates from what admired code
does. Tree-sitter parse, Rust, 6.39M lines in 34s.

```
metric             p50     p90     p99     max   budget     violate
cognitive            0       7      34    1088   <=15          3.7%

worst — cognitive (budget <=15):
    1088  site-packages/.../fastjsonschema_validations.py:157  validate_...
```

---

## Install

```sh
cargo install --git https://github.com/GrigoryEvko/elegance
```

Or a released binary — the musl build is static, so it needs no matching
glibc:

```sh
curl -fsSLO https://github.com/GrigoryEvko/elegance/releases/latest/download/elegance-x86_64-linux-musl
chmod +x elegance-x86_64-linux-musl && sudo mv elegance-x86_64-linux-musl /usr/local/bin/elegance
```

Eight binaries per release, each built natively, each with a `.sha256`
sidecar and smoke-tested on this repository before publishing:

| | Linux (gnu / musl) | macOS | Windows |
| :--- | :--- | :--- | :--- |
| **x86_64** | `x86_64-linux-gnu` · `x86_64-linux-musl` | `x86_64-macos` | `x86_64-windows.exe` |
| **arm64** | `aarch64-linux-gnu` · `aarch64-linux-musl` | `aarch64-macos` | `aarch64-windows.exe` |

### Claude Code plugin

```
/plugin marketplace add GrigoryEvko/elegance
/plugin install elegance-nudge@elegance
```

Reports what an edit introduced, one line, as it happens. Advisory,
deduplicated per session, silent on clean files.
See [plugins/elegance-nudge](plugins/elegance-nudge/README.md).

---

## Usage

| Command | |
| :--- | :--- |
| `elegance [paths...]` | scan; findings ranked by how far past budget they sit |
| `elegance --full [--top N]` | every section and every per-metric offender list |
| `elegance --brief [paths...]` | the headline only — what kind of trouble, not which unit |
| `elegance --json [paths...]` | versioned machine output (schema 1) |
| `elegance --explain file[:line]` | per-construct score breakdown |
| `elegance --baseline write` | record today's violations as the ledger |
| `elegance --baseline check` | exit 1 on new or worsened violations |
| `elegance --diff HEAD` | judge only what this change touched |
| `elegance --diff 'origin/main...'` | judge only what a PR added |
| `elegance --fail-on RUNG` | which rungs may block (default 2) |
| `elegance --sarif [paths...]` | SARIF 2.1.0 for code scanning |
| `elegance install-hook` | pre-commit hook running `--diff HEAD` |
| `elegance --context [paths...]` | the repo's measured style, for a writer |
| `elegance calibrate <gold-dirs>` | re-derive budgets from a gold corpus |

| Reports | |
| :--- | :--- |
| `--hotspots` | rank complexity by how often it is edited |
| `--history record\|show` | trend ledger: are we getting better? |
| `--by` | roll findings up per directory, worst first |
| `--coupling` | undeclared co-change, sole authorship, debt age |
| `--deps` | look inside the dependencies you did not write |
| `--helm` | values-overlay drift and credentials in YAML |
| `--render` | render each environment, measure what ships |

### The ratchet

Identity is **(metric, path, qualified unit)** — line numbers are
deliberately not part of the key. `--baseline check` fails on new or
worsened violations; old sludge is tolerated until touched. Only rungs
0–2 gate by default: `--fail-on 0` tightens, `--fail-on 4` loosens.

```yaml
# .github/workflows/quality.yml
- run: elegance --baseline check .
- run: elegance --diff 'origin/main...HEAD' .
- run: elegance --sarif . > elegance.sarif
```

Three dots matter on a pull request. `--diff origin/main` compares
against the branch tip, so every merge into main since you branched reads
as your change. `origin/main...HEAD` compares against the merge base.
`--diff HEAD` is what a pre-commit hook wants.

### `--explain`

```
gnarly  sample.py:17  cognitive 31  cyclomatic 14  depth 6
  L19    loop       cognitive +1  cyclomatic +1
  L24    if         cognitive +5  cyclomatic +1  (1 + nesting 4)
```

### Configuration

`.elegance.toml` at the scan root:

```toml
exclude = ["migrations/**", "*_pb2.py"]
skip_dirs = ["fixtures"]

[budgets]
length = { hi = 100 }
"comment ratio" = { lo = 0.05, hi = 0.5 }
```

Files with generated-code markers (`@generated`, `DO NOT EDIT`) are
skipped, as are `vendor/`, `third_party/`, `node_modules/` and friends.

---

## Metrics

The rung decides what a finding can *do*:

| Rung | Scope | Verdict |
| :--- | :--- | :--- |
| **0–2** | token · expression · function | **gates CI** |
| **3–4** | interface · class/module | suspicion |
| **5–6** | dependency graph · evolution | report only |
| **7** | tensions | paired evidence |

### Rung 0 — token hygiene

| | |
| :--- | :--- |
| `magic numbers` | unnamed non-trivial literals outside constant contexts; test bodies exempt |

### Rung 1 — expression shape

| | |
| :--- | :--- |
| `expr depth` | tallest single-line expression tree — multi-line formatting is rewarded |
| `demeter` | attribute chains ≥ 3 data links; fluent calls exempt, `self` forgives one |
| `negations` | double negatives, negated negative-polarity names, De Morgan candidates |

### Rung 2 — function shape

| | |
| :--- | :--- |
| `cognitive` | SonarSource: structural +1+nesting, `elif`/`else` flat 1, boolean sequences 1, recursion 1 |
| `cyclomatic` | McCabe — decision points + 1, a minimum test count |
| `depth` · `length` · `params` | |
| `live span` | McConnell ch. 13: a live variable is a mental register |
| `swallowed` | a handler that silences the error entirely |
| `built query` | SQL assembled by interpolation |
| `conditional hook` | a React hook reached through a branch |

**`built query`** is judged by where the hole lands — a comparison slot,
where a value belongs, or an identifier slot, where a table name does.
`IN (${placeholders})` and `VALUES ${rows}` stay silent: that is
structure whose values travel separately, and vscode writes the safe
form five times for every unsafe one. Two findings in 6.39M lines of
gold, both real.

**`conditional hook`** — React identifies a hook by the order it is
called in, so one reached through a branch renumbers every hook after it
the moment the condition flips, and the component reads another hook's
state. Fires only inside a component or custom hook, in a file importing
React. Solid is exempt: it tracks dependencies at run time.

### Rung 3 — interface shape

| | |
| :--- | :--- |
| `returns` | tuple width; unwraps one generic level, so `Result<(A,B,C), E>` counts 3 and `Vec<(A,B)>` counts 1 |
| `interface width` | methods per declared interface — Go gold's median is 1 |
| `repurposed` | Fowler's Split Variable; compound operators, conditional overrides, loop refills, swaps, member writes and let-shadowing exempt |
| `flag params` · `kw opacity` | |
| `pass-through` | Ousterhout's shallow wrapper / Fowler's Middle Man |
| `generic name` | |
| `lying name` | `is_`/`has_` must return bool; `get_` must not mutate |
| `broad catch` · `unwraps` | |
| `spooky` | eval, computed attribute access, metaclasses, transmute |
| `echo comments` · `comment ratio` | |
| `test asserts` · `lazy test name` | |
| `untyped params` · `loose types` · `casts` · `suppressions` | type hygiene |

`returns` stays a suspicion because a TS tuple annotation is *optional*:
its budget rides on how often gold annotates at all, and a declared
`[value, setter]` pair is legitimate style. Python takes the wider of
its annotation and the tuples its returns ship.

### Rung 4 — class and module

| | |
| :--- | :--- |
| `cohesion` | LCOM4: how many disconnected groups a class's methods fall into. One is cohesive; more means several objects sharing a name. Methods touching no member are excluded |
| `and name` | a conjunction confesses two responsibilities |
| `feature envy` | a method living in another object's data belongs there |

### Rung 5 — dependency graph and rates

Cycle mass, dependency depth, deletability, blast radius, orphans,
interface depth, fat surfaces, encapsulation leaks, `dead exports`,
step-down narrative ordering.

| | |
| :--- | :--- |
| `clones` | Type-2 structural duplication via normalized Merkle hashing — only logic, since duplicated data tables are content |
| `near-clones` | winnowed fingerprints, catching *edited* copies |
| `param clumps` | Fowler's Data Clumps |
| `repeated dispatch` · `untested complexity` | |
| `public docs` · `asserts` | coverage **rates**, rendered beside gold's own share |

Rates never render as per-unit findings — `63% of 103 public units
documented; admired rs: 84%`. Admired code fails the per-unit claims 81%
and 90% of the time, and a suspicion the gold corpus fails nine times in
ten is a distributional fact wearing the wrong rung.

### Rung 6 — evolution

`--hotspots` (churn × complexity, Tornhill), `--history` (are we getting
better), `--by` (which directory is in trouble).

### Rung 7 — tensions

Facts that are worse together than apart: a unit over budget, that no
test mentions, in a file half the codebase imports, holding duplicated
logic. Each is survivable alone and already reported at its own rung.

Three independent facts must align, and correlated metrics count as one
— cognitive, cyclomatic, length and live span all trip because a
function is big. Deliberately **no score**: a composite number would
hide which of the facts is true.

### Layer contracts — the one architecture claim that gates

Every other architecture measurement is a description, and a number
about a graph is not a verdict about a design. A declared contract is
different:

```toml
[layers.gallery]
paths = ["src/gallery"]
may_import = ["shared"]

[layers.playground]
paths = ["src/playground"]
may_import = ["shared"]

[layers.shared]
paths = ["src/shared"]        # may_import absent: reaches nothing
```

`--baseline check` fails on any breach. Deliberately **not ratcheted**:
every other gate tolerates recorded sludge because the threshold is
calibrated, but a rule the repository wrote down is wrong on the first
day and on the thousandth. Unresolved imports are never judged, and a
file in no declared layer is unjudged rather than guessed at.

### Budget notation

| Printed | Rests on |
| :--- | :--- |
| `<=33` | a percentile of the gold corpus |
| `<=12.` | the compiled-in default — fewer than 200 samples, a policy metric, or a gold p99 of zero |

Machine output says the same in a `budget_source` field. Budgets are
`[lo, hi]` bands, per language: one-sided take gold p99, two-sided take
p05/p95. Policy metrics encode taste and are never calibrated, but the
calibrate audit reports any policy the corpus itself violates (1%
ceiling for gates, 5% for suspicions).

What the gold data says: cognitive p99 lands at py 18, rs 16, ts 32;
Rust's doc culture pushes its comment ceiling to 78%. A test pins these
numbers to `calibration.toml` — the README cannot drift from the corpus.

---

## Outside the ladder

<details>
<summary><b><code>--helm</code></b> — a chart is not an import graph</summary>

A Helm chart *looks* like an import graph: `values.yaml` declares keys,
templates reference them. Treating it as one produced 48 findings on a
real chart and **zero were true** — keys reached through
`range $name, $deploy`, through `index .root.Values .svc`, mentioned
only inside a comment, or guarded by `| default`.

So this tier reports only what needs no template evaluation: **overlay
drift** (which keys each environment sets, by set comparison) and
**credentials in YAML scalars**, using the same tested rules the
extractor applies to source.
</details>

<details>
<summary><b><code>--render</code></b> — measure what actually ships</summary>

The way past a template you cannot evaluate is to stop analysing the
template. `helm template` resolves every indirection by running it, and
`--render` measures the manifests that come back: which resources each
environment actually ships (a values diff cannot tell you this — a
resource may appear or vanish through a conditional), and credentials a
template injected from its values, invisible in the template and the
values file alike.

On a real four-region chart with 35 keys of values drift, every
environment renders the same 19 resources.
</details>

<details>
<summary><b><code>--deps</code></b> — the code you did not write</summary>

The dependency tree is where a supply-chain problem hides, and the
ordinary scan prunes it by design. This mode looks anyway and reports
only what matters about code you cannot change: credentials compiled
into it, action at a distance, checker suppressions, volume per package,
and logic it shares with your own tree.

Every unit-shape metric is deliberately absent — a dependency's
cognitive complexity is trivia. One kiosk frontend measures 47k lines of
its own against **3.97M lines across 575 packages**.
</details>

---

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
metrics never see a syntax tree. Adding a language is one kind table and
four hooks; adding a metric is one function over facts. One bottom-up
pass per file computes control events, clone fingerprints and expression
heights together.

A cross-language conformance suite pins the ontology: the same function
written in every supported language must produce identical metrics.

---

## Languages

| Language | Extensions | Notes |
| :--- | :--- | :--- |
| Python | `.py` | |
| Rust | `.rs` | doc culture pushes the comment ceiling to 78% |
| TypeScript | `.ts` | |
| TSX | `.tsx` | rides the TypeScript pack |
| JavaScript | `.js` `.mjs` `.cjs` `.jsx` | |
| Go | `.go` | median declared interface holds **1** method |
| Zig | `.zig` | |
| OCaml | `.ml` `.mli` | control group — tightest branching in the corpus |
| C | `.c` `.h`\* | no preprocessing; numbers are floors |
| C++ | `.cpp` `.cc` `.cxx` `.hpp` `.hh` `.hxx` `.h`\* | RAII kills `unmanaged`; gtest names composed |
| CUDA | `.cu` `.cuh` | rides the C++ pack, own budgets |
| shell | `.sh` `.bash` | no declared parameters, so no interface family |
| Perl | `.pl` `.pm` `.t` | parameters only where signatures are used |
| PHP | `.php` | types grew in from the outside; `untyped params` reads the migration |
| Ruby | `.rb` `.rake` `.gemspec` | tightest function length in the corpus at 37 |
| Lua | `.lua` | no classes, so the class family is structurally silent |
| Java | `.java` | everything declared, so the type family reads at full strength |
| C# | `.cs` | `async` is syntax, so `blocking async` is live here and dead in Java |
| Swift | `.swift` | `!` and `try!` are what `unwraps` counts |
| Scala | `.scala` `.sc` | `if` and `match` are expressions, so branches sit inside arguments |
| Elixir | `.ex` `.exs` | homoiconic: the ontology is built from call names, not syntax |
| Solidity | `.sol` | inline assembly is `spooky`; visibility is compulsory, so `public docs` reads a decision |

\* `.h` is the one extension that underdetermines its language.
[The text decides it](#the-c-family).

### Containers

Blanking everything but the code preserves **true line numbers**, so
`--explain`, `--diff` and the baseline need no offset bookkeeping and no
new grammar.

| Container | Read as |
| :--- | :--- |
| `.vue` / `.svelte` | `<script>` blocks as TypeScript or JavaScript |
| `.ipynb` | code cells as Python; markdown, outputs and IPython magics blanked |
| `.github/workflows/` | `run:` blocks as shell |
| `Dockerfile` | `RUN` (shell form), `ENV` and `ARG` bodies as shell |

Placement is verified rather than assumed: cells come from a JSON parse,
positions from a scan of the raw text, and a disagreement on any line
refuses the file rather than measuring it at invented lines.

**Notebooks are not softened, and the measurement is why.** Against
ordinary Python budgets, 38 real notebooks read cognitive 4%, cyclomatic
5% and depth 3% — the complexity budgets already fit. What is elevated
is exactly the exploratory-hygiene family: echo comments 57%, untyped
params 16%, commented-out code 13%, repurposed variables 12%. Those are
the things worth knowing when a notebook is promoted into a pipeline.

### The C family

<details>
<summary>Extensions do not settle the language, so the text does</summary>

Reading every `.h` as C dropped a third of every C++ repository as
unparseable — leveldb lost 47 of 56 headers, re2 20 of 23, fmt 23 of 25
— because headers are where C++ keeps its classes. Reading every `.h` as
C++ parses at least as well on C, and would then file musl's 655 headers
under `cpp`, calibrating one language on another's code.

So the label needs deciding as well as the grammar, by four
line-anchored spellings that are not C: `namespace`, `template<`, an
access specifier, `class` + a name. Validated before it was written —
**0 of 1,027** headers from lua, musl, redis and curl match, and **98 of
104** from fmt, leveldb and re2 do. The six that miss are `c.h`
(leveldb's C API), `export.h`, `port.h` and `thread_annotations.h`:
headers holding no C++ at all.

CUDA is classified the same way, by `__global__`, `__device__`,
`__shared__`, `__constant__` and `__host__` appearing outside comments.
</details>

**C is parsed without preprocessing, so its numbers are floors.**
Function-like macros read as calls, but macro *bodies* are invisible and
linkage-macro prefixes (`LUA_API void f(...)`) produce local parse
errors — macro-heavy files fall below the confidence bar and are
excluded, visibly counted in the report header. `#if`/`#elif`/`#else`
count as real branches: conditional compilation is control flow the
reader must follow.

**C++ gold reads cognitive p99 = 30 and length p99 = 115, against C's 80
and 205** — for a language very nearly a superset of the other.
Conditional compilation is much of it, RAII most of the rest, since a
destructor removes the error path a C function writes by hand.

<details>
<summary>Three C++ decisions, and what the corpus says about itself</summary>

- **RAII kills `unmanaged`** for the reason it is dead in Rust: a
  destructor runs on scope exit, so there is no missing guard to find.
- **A class whose methods are all pure virtual** is what `interface
  width` counts — that is an interface in everything but keyword.
- **gtest's `TEST(args_test, basic)`** is read as the declaration it is.
  The grammar can only see a function called TEST, so the pack composes
  the name gtest itself prints: `args_test.basic`. Judging the case
  alone read 61% of the C++ gold corpus as lazily named — worse than a
  corpus of notorious code, which is how the bug announced itself.
  Catch2 is the stated limit: `TEST_CASE("a pool takes a slot")` puts a
  string where a parameter belongs and does not parse.

The corpus is honest about itself too. Five hand-made modern libraries
(fmt, immer, flux, ctre, magic_enum) read 19 on their own; adding two
applications by one hand each — kakoune and mold, which parse at 99%,
the best figures anywhere in this corpus — takes it to 30. An
application branches harder than a header-only library, and a C++ budget
derived only from libraries would have been one nobody could meet.
</details>

**CUDA adds exactly two named kinds to C++** — a whole dialect for one
table entry, which is what the hourglass was built to buy. `__global__`
and `__device__` are unnamed tokens the tree never shows, `__shared__`
arrives as an ordinary `type_qualifier`, and `add<<<grid, block>>>(x)`
is already a `call_expression` with one extra child.

Statistically it is not C++ at all. Gold reads params p99 = 15 against C++'s 5, magic numbers 30 against 10, and length 249 against 115.
A kernel really does take fifteen arguments and really is full of tile
sizes, and borrowing C++'s budgets would have flagged nearly every one.

**OCaml is a control group: cognitive p99 = 6 and length p99 = 52,
against Python's 18/76, Rust's 16/93 and TypeScript's 32/106** — three
to five times tighter on complexity. Idiomatic OCaml iterates with
`List.iter` and a lambda, which the ontology reads as a call rather than
a loop, so loop-based metrics read low for it by construction.

Perl declares parameters only through signatures (`sub f ($x, $y = 1)`).
A sub that unpacks `@_` by hand reads as taking none, so the interface
family measures the age of the code.

The Perl grammar is `ts-parser-perl` 1.2.1. `tree-sitter-perl` stops at
1.1.2 and rejects `use Module -flag`, which costs 1,214 parse errors in
2,946 corpus files against 21 for the current grammar.

**Ruby's function length p99 is 37**, against OCaml's 52 and Python's 76.
Blocks are its control flow, so `each` and `map` count as iteration;
otherwise Ruby contains no loops.

**Lua reads cognitive p99 = 65**, three and a half times Python's. It has no classes,
so the class family is structurally silent: a method is a function in a
table and `self` is a calling convention.

**PHP holds two ages at once.** An untyped array-shaped function and a
`final readonly class` with union types are both ordinary, so `untyped
params` measures the migration.

**C# and Java branch least of any language with classes** — cognitive
p99 = 8 and 12, against Python's 18 and TypeScript's 32, with only
OCaml's 6 below them. Mandatory class structure spreads the branching
across many small methods instead of concentrating it in one.

C# splits a method signature across `#if` in the same way C does, and
those files fall below the confidence bar and are counted in the header.
The `serilog` corpus candidate parsed at 86% for this reason.

**Elixir's cognitive p99 is 5**, below OCaml's 6 and the lowest here.
The language is homoiconic, so `def`, `if` and `case` are calls rather
than syntax and this pack classifies them by the name being called.
`Enum.each` is a function, so iteration reads as a call the way it does
in OCaml and Ruby, and loop depth measures nothing.

**Solidity is the one language here where a finding is financial.**
Inline assembly drops beneath both the type system and the overflow
checks, so it is `spooky` on the same grounds as `unsafe`; `delegatecall`
runs another contract's code against this contract's storage.

Two candidates were cut for grammar defects rather than for what their
code looks like. `tree-sitter-haskell` 0.23.1 corrupts the heap on two
adjacent `LANGUAGE` pragmas — it aborts on 89 of 128 files in aeson and
321 of 366 in pandoc, and there is no later release. The PowerShell
grammar cannot read a scriptblock used as a hashtable value or
`.where({...})`, which puts 95% of ImportExcel out of reach; calibrating
on the files that survive would pin budgets to the simple ones. Kotlin
cannot be built at all: `tree-sitter-kotlin` 0.3.8 pins tree-sitter
below 0.23, and two crates cannot link the same native library.

**Shell provisions production and nothing was measuring it.** A single
Kubernetes repository here holds 22.6k lines of it, including a
3,287-line registry provisioner. A shell function declares no parameters
(`$1` is read from the caller's frame), so the whole interface family is
structurally silent — a fact about the language, recorded in the parity
matrix rather than left to look like a gap. `.sh`/`.bash` only:
`Lang::from_path` is a pure path predicate the walk calls on every file,
and sniffing would change what a walk costs.

### Calibration scoping

**A repository speaks only for the language it was declared for.** A
modern CUDA repository is a Python and C++ monorepo with kernels inside
— cutlass alone carries 1,115 `.cpp` files and 596 `.py` — and pooled by
extension, adding five of them moved **46 budgets in languages nobody
was editing**, Python's length from 80 to 252 and C++'s params from 5 to
115. Shell opts back in, because 207 of its 342 corpus files are
genuinely build scripts living inside other checkouts.

---

## Performance

**6.39M lines / 24.8k files / 325k units in 34s, 1.4 GB peak RSS**
(single machine, release build, 2026-08). Facts are transient per file;
what survives is per-unit metric values, clone sites, fingerprints, the
identifier vocabulary and bounded offender lists.

Memory, not speed, is the scaling limit, and it is roughly linear in
lines — budget about 2 GB for a 10M-line tree. Two things were done
about it and one was undone:

- clone-site paths are shared (`Arc<str>`), not cloned per site:
  1.11 GB → 0.96 GB
- the blast-radius ancestor matrix is computed 64 components at a time,
  turning `ncomp²/8` bytes into `ncomp·8` — 312 MB → 400 KB at 50k
  modules
- storing a clone class's first site inline to spare singletons a `Vec`
  allocation measured 7% **worse** and was reverted: widening the struct
  grows hashbrown's table across millions of entries by more than the
  allocations it saves

---

## Roadmap

- An aggregate gold-relative summary — maybe; the ladder resists a
  single score
