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
| `elegance [paths...]` | scan; bodies ranked by how far past budget they sit |
| `elegance --full [--top N]` | every section and every per-metric offender list |
| `elegance --brief [paths...]` | the headline only — what kind of trouble, not which unit |
| `elegance --json [paths...]` | versioned machine output (schema 2) |
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
| `elegance tidy FILES... [-- FLAGS]` | clang-tidy on C++ that its Clang does not read yet |

| Reports | |
| :--- | :--- |
| `--hotspots` | rank complexity by how often it is edited |
| `--history record\|show` | trend ledger: are we getting better? |
| `--by` | roll findings up per directory, worst first |
| `--coupling` | undeclared co-change, sole authorship, debt age |
| `--deps` | look inside the dependencies you did not write |
| `--helm` | values-overlay drift and credentials in YAML |
| `--render` | render each environment, measure what ships |

### One entry, one body

The ranked sections list **bodies, not findings**. The function-shape
metrics move together — on a mixed reference tree, 93% of the units over
`cyclomatic` are also over `cognitive`, and 96% of those over `length`
are also over `live span` — so a row per metric named five different
functions for one fact and left the functions breaking six budgets each
unnamed. An entry now names the body once, says how far out it is and
how many budgets say so, and lists them:

```
   13x  4 gates      spice/tile/gen_die_behavioral.py:43  main
        live span 626>48  length 636>76  cognitive 57>18  cyclomatic 22>12
```

Ranked by distance, still: ranking by the count instead was measured and
put a body 2x past on five budgets above one 13x past on four. A finding
against a budget of zero has no distance and stays in the `policy`
section, so the count is of budgets that can be exceeded, not of every
rule the body trips.

A third line says what the body costs beyond itself, when something
does:

```
    9x  3 gates      config.py:42  _build_v5
        magic numbers 101>11  live span 192>48  length 195>76
        17 files import this one
```

Four facts can be true — the file is load-bearing, no test names the
body, the same finding fills the file, other bodies make the identical
claim — and only the rarest is printed, because a clause per fact puts
the entry back where the grouping found it. The first three are rung-7
inputs the report already computed and then summarized as a count.

The fourth exists because dropping the per-metric cap let one claim fill
a list from the other direction: six copies of a generated locale file
each read `demeter 35>2`, and five of them took the gold Lua corpus's
whole front page. Bodies whose numbers match to the digit are collapsed
into one entry that says how many it stands for. Different values are
different bodies — three functions over the same three budgets at
different numbers are three functions to open.

### What the file already knows

Every other fact on an entry is about the global rule: a metric, a
budget calibrated on admired code, a distance. The reader is standing in
one particular file, and the fact they are missing is what the **rest of
that file** did with the same budget — which decides whether the work is
an extraction or an afternoon:

```
    9x  3 gates      config.py:42  _build_v5
        magic numbers 101>11  live span 192>48  length 195>76
        magic numbers — alone here, the other 6 peak at 6
        17 files import this one

    3x  1 suspicion  drc/beol_193nm_drc.py:1005  check_via_enclosure
        loop depth 3>1
        loop depth — check_col_width does the same job at 1
```

One clause, chosen most useful first, because all three answer that one
question. A peer whose own name claims the same job and stayed inside
wins: it names a target one screen away rather than describing a
situation, and the two above are both `check_*(cell, report)` rules over
the same polygons — one written with a single loop, the other with
three. Failing that, nothing else here being over says the file knows
how to do this and one body drifted. Failing that, a count of how many
bodies here are over the same budget — which the ranked list can never
say for itself, because it shows at most one body per file.

It cannot read as an excuse, which was the risk in showing a local
distribution beside a global rule. On the reference tree two of the 332
findings with three or more peers sat at or below their file's median:
`main` reads live span 626 where the next body in the file reads 7, and
`_build_v5` reads 101 magic numbers where the next reads 6. The crowded
case is 83% of shape findings there, and a namesake covers 29 of 49
`cognitive` findings, 23 of 43 `cyclomatic` and 16 of 27 `depth` — it is
thinnest exactly where severity ranking looks, since `main`, `<module>`
and `generate_spice` have no namesake by construction, and that is where
the second clause has the most to say.

A named peer must be doing comparable work: at least half the budget,
because the largest clean value in a file is otherwise routinely a
one-argument shim, and *`emit_one` does the same at 1* is not advice
about a seven-argument entry point. Under a **floor** the largest clean
peer is the furthest from the finding, so it goes unnamed — *the other
30 peak at 12 asserts* answers nothing about a body that has none.

All of it is read off the measurements the file already produced, in a
second pass over them rather than over the source; `for_each` lends its
labels, so holding one file's measurements costs a `Vec` per file and
not a string per measurement. Whole-corpus cost, measured: 25.9s and
2.0 GiB, against 25-26s and 2.1 GB before.

### Machine output

`--json` emits **schema 2**; `--sarif` emits SARIF 2.1.0. A format flag
outranks a verbosity flag — `--json --full` is JSON, because `--json` is
already full: it caps no offender list and omits no section. `--json
--sarif` names two formats and is refused rather than silently resolved.

Schema 2 added the per-language denominators. Every metric here is
per-unit or per-file, so a rate compared across two corpora needs a
per-language denominator on **both** sides. Dividing one corpus's
per-language violation counts by its pooled unit total is how a
comparison once reported `params` firing 651x more often on admired code
than on real code — a number that was entirely an artifact of the
division.

```json
"languages": [
  { "lang": "rs", "files": 56, "units": 1265,
    "metrics": [ { "name": "cognitive", "measured": 1321, "violations": 3 } ] }
]
```

`measured` is the denominator, not `units`. A metric that also measures a
file's top level exceeds the unit count — above, `cognitive` measures
exactly one more per file, because a module scope is code too and `units`
excludes it. Metrics that skip test bodies, or languages without type
syntax, fall short of it instead. Summed
over the languages present, `measured` equals the pooled `n` on the same
metric and `units` equals the pooled `units` — an invariant the suite
checks across a merge, since the rows accumulate per worker.

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

### `elegance tidy`

clang-tidy parses with the Clang it was built with, and the Clang in each
release reads only part of C++26. A contract clause that Clang cannot
parse is an error, and no check then examines that declaration.

`elegance tidy` removes, for clang-tidy only, what its Clang does not read
and what does no work: contract clauses, `contract_assert`, annotations,
and class properties. A virtual file system overlay puts the changed text
at the original path, so each diagnostic shows the original file, line and
column. No file on disk changes.

```sh
elegance tidy --checks='bugprone-*' src/pool.cpp -- -std=c++26 -Iinclude
elegance tidy --clang-tidy clang-tidy-23 -p build src/pool.cpp
```

`-p BUILD` reads `compile_commands.json`. The command removes from a copy
of it the flags that this clang-tidy does not know, such as `-fcontracts`
from a GCC build. A clang-tidy in a container works with flags after `--`,
because every path in the overlay is relative to the working directory.

Reflection, a consteval block and, before Clang 23, an expansion statement
have no removal that keeps their meaning. The command tells how many files
use each one, and clang-tidy reports each use as an error. `--fix` is not
available, because clang-tidy calculates a fix on the changed text.

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
| `demeter` | attribute chains ≥ 3 data links; fluent calls exempt, `self` forgives one, a chain rooted at an import or a capitalised name is a namespace path and not one, tests exempt |
| `negations` | negated `!=` comparisons, negated negative-polarity names. A De Morgan candidate is NOT one: distributing it is usually longer and worse, and it was 80.5% of the metric on gold |

### Rung 2 — function shape

| | |
| :--- | :--- |
| `cognitive` | SonarSource: structural +1+nesting, `elif`/`else` flat 1, boolean sequences 1, recursion 1 |
| `cyclomatic` | McCabe — decision points + 1, a minimum test count |
| `depth` · `length` · `params` | |
| `live span` | McConnell ch. 13: a live variable is a mental register |
| `swallowed` | a handler that silences the error entirely |
| `built query` | SQL assembled by interpolation |
| `shelled out` | a value spliced into a command a shell will re-parse — by interpolation, by concatenation, or by handing the shell a variable after `-c`. The `-c` counts only where a shell LITERAL introduces it, and an argv list is the remedy |
| `blocking async` | a call that parks the thread inside an `async` unit — a qualified `sleep`, sync HTTP, `std::fs`, or a name from Node's documented sync API; a trailing `Sync` alone is not one, and a build script has no executor to stall |
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
| `generic name` | |
| `lying name` | `is_`/`has_` must return the language's boolean — `Boolean`, `Bool`, a TypeScript type predicate `v is T`, C89's `int`; `get_` must not mutate; a declared test's name is prose, not a contract |
| `broad catch` · `unwraps` | |
| `lost context` | a handler that binds the error, raises a new one and never names the original; an operandless re-raise — `throw;`, bare `raise` — carries the stack onward and is the remedy |
| `spooky` | eval, computed attribute access, metaclasses, transmute |
| `ceremony` · `unawaited coroutine` | both demoted from rung 2: neither has ever fired on the gold corpus, so neither has a precision anyone has tested. A gate has to be able to fail a build |
| `echo comments` · `comment ratio` | |
| `module doc` · `type doc` · `fn doc` · `field doc` · `inline doc` | prose words per comment run, pinned per language **and role** |
| `doc param` | a parameter the documentation names and the signature does not declare |
| `test asserts` | a declared test whose body asserts nothing |
| `untyped params` · `loose types` · `casts` · `suppressions` | type hygiene |

**Doc length** is the whole wordiness signal — compression, type-token
ratio and per-word semantic density were all measured against the same
corpus and either tracked something else or moved under 10%. It counts
*prose* words, so a thirty-line builder doc measures as its four words
of writing and its fenced example counts for nothing.

One budget per role, because one across roles is meaningless for all of
them: gold function summaries run to p99 = 258 prose words in Rust, 127
in TypeScript and 89 in Python, while a field's doc is a phrase and a
module header is a page. A role with fewer than 200 runs in gold
inherits its language's *pooled* doc p99 and `calibration.toml` records
the borrowing on the line above it — a budget nobody can see is
borrowed is worse than no budget. Trailing comments have none at all: a
trailing run is one line by construction, so its length measures line
width.

Bounded above only. A floor would fire on `/// The parsed AST.`, which
is correct as written; the counterweight against truncation is the
`public docs` coverage rate, bounded below.

**`doc param`** reads what a doc comment *claims* — `Args:` blocks,
`:param`, numpydoc, `@param` with or without a `{Type}`, `# Arguments`,
`<param name=>`, POD `=item $x` — and compares it against the signature.
Only *documented-but-absent* fires: a parameter left undocumented is
coverage, which `public docs` already measures, while a documented
parameter that does not exist is a false statement about the code. Over
14,068 gold units that name a parameter in documentation, 254 name one
that is absent (1.81%) — rich documents `control_codes` for a parameter
called `control`, curl's `my_sha256_update` documents `md` and `inlen`
against `ctx` and `len`. Two shapes are skipped whole, both because
syntax cannot decide them: a signature binding by pattern
(`{ limitLength, headerName }` documented as `options`) and one carrying
a splat, which accepts arguments it does not name.

`returns` stays a suspicion because a TS tuple annotation is *optional*:
its budget rides on how often gold annotates at all, and a declared
`[value, setter]` pair is legitimate style. Python takes the wider of
its annotation and the tuples its returns ship.

### Rung 4 — class and module

| | |
| :--- | :--- |
| `cohesion` | LCOM4: how many disconnected groups a class's methods fall into. One is cohesive; more means several objects sharing a name. Methods touching no member are excluded. Eleven of its sixteen languages declare too few classes in all of gold to hold a percentile, so they inherit the MEDIAN of the five that do |
| `and name` | a conjunction confesses two responsibilities |
| `feature envy` | a method living in another object's data belongs there |
| `pass-through` | Ousterhout's shallow wrapper / Fowler's Middle Man — a declared override, a lambda and a constructor re-declaring its superclass's are forwards the language demanded, not layers the author chose |

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
| `<=76 py · <=115 cpp · <=109 sh` | a mixed tree: one band per language, most files first, languages sharing a band named together |

A budget is per language, so a mixed scan has several. It used to print
`varies`, which named the problem and withheld the answer — and did so
on exactly the rows a reader needs the number for, since the metrics
that fire most are the ones calibrated per language. Past three distinct
bands the tail is counted (`+2`) rather than dropped, and the column is
printed last so a wide label costs no other column any width.

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

Every pack hook is asked through a method that records the question, so
a hook the core never consults is provably distinct from one that
correctly finds nothing — they used to produce the same zero, and nine
detectors died in that gap at once. The counters are live only under
debug assertions; `ELEGANCE_HOOKS=1` on such a build prints the ledger
after a scan. The test that reads it found Lua's and Ruby's `imports`
hooks had never been asked once: both are written against `require`,
which is a CALL in both languages, and neither had a module graph.

An assertion is spelled differently in every ecosystem, and where a
pack could not read the spelling its tests all reported that they check
nothing. Go's stdlib assertion is `t.Errorf(...)`, which is a method
call on the test handle rather than a name — and `fmt.Errorf`,
`err.Error()` and `log.Fatal` wear the same verbs, so the receiver is
resolved against the enclosing function's `testing.T` parameter rather
than guessed from the name. swift-testing's `#expect` and `#require`
are macro invocations, a node kind the Swift pack had not mapped at
all. Lua's busted writes `assert.same`, and reading only the trailing
segment left `same`. TypeScript takes `strictEqual` and `ok` bare from
`node:assert`, so the file's imports decide whether that name is an
assertion. Together those four read 86.9% of gold's Go tests, 91.3% of
its Lua and 31.6% of its Swift as assertionless; the figures are now
12.7%, 28.0% and 11.4%, and Lua's whole remainder is `describe`
containers counted as tests.

Whether a declaration is a method was read from the tree alone, and
that reading is exactly as good as the parse. `tree-sitter-c-sharp`
cannot parse a `#if`-guarded `else if` between an `if` and its `else`,
so the class in Newtonsoft.Json's `JsonTextReader.cs` ends at the first
one and the members below it become free functions — and the same
grammar accepts a `method_declaration` sitting directly in a namespace
without an error node, so nothing downstream could tell. A short table
names the kinds a grammar spells only for a type member (C#'s
`method_declaration` and its neighbours, Java's two) and settles the
question without the tree. That was `ceremony`'s last false positive on
the whole gold corpus.

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
| OCaml | `.ml` `.mli` | control group — tightest branching in the corpus; `unwraps` is dead — `failwith` IS the raise |
| C | `.c` `.h`\* | no preprocessing; numbers are floors; `demeter` is dead — no methods, so no delegate to hide |
| C++ | `.cpp` `.cc` `.cxx` `.hpp` `.hh` `.hxx` `.h`\* | templates measured as written; gtest names composed |
| CUDA | `.cu` `.cuh` | rides the C++ pack, own budgets |
| shell | `.sh` `.bash` | no declared parameters, so no interface family; `shelled out` is dead here — the language IS the shell |
| Perl | `.pl` `.pm` `.t` | parameters only where signatures are used; `unwraps` is dead — `die` IS the raise |
| PHP | `.php` | types grew in from the outside; `untyped params` reads the migration |
| Ruby | `.rb` `.rake` `.gemspec` | tightest function length in the corpus at 37 |
| Lua | `.lua` | no classes, so the class family is structurally silent; `unwraps` is dead — `error` IS the raise |
| Java | `.java` | everything declared, so the type family reads at full strength |
| C# | `.cs` | `async` is syntax, so `blocking async` is live here and dead in Java |
| Swift | `.swift` | `!` and `try!` are what `unwraps` counts |
| Scala | `.scala` `.sc` | `if` and `match` are expressions, so branches sit inside arguments |
| Elixir | `.ex` `.exs` | homoiconic: the ontology is built from call names, not syntax; `unwraps` is dead — `raise` IS the raise |
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

**C++ gold reads cognitive p99 = 32 and length p99 = 115, against C's 80
and 205** — for a language very nearly a superset of the other.
Conditional compilation is much of it, RAII most of the rest, since a
destructor removes the error path a C function writes by hand.

<details>
<summary>Two C++ decisions, and what the corpus says about itself</summary>

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

**elegance reads C++ as a compiler does, up to C++2d, the draft that
follows C++26.** The bundled grammar is older than the language, and a
parse error removes the rest of its scope from the tree. So elegance
normalizes each C++ file before the parser reads it. The normalization
removes the parts that do no work: contracts, attributes, specifiers,
reflection operators. Every call, branch and loop stays, and every byte
offset stays, so each finding shows the correct line.

<details>
<summary>What the normalization covers, and how a test holds it to that</summary>

`src/lang/cpp/dialect_probes.txt` holds 675 short sources. There is one
for each row of the Clang conformance table that has syntax, from C++98
to C++2d, and one for each GNU, Clang and MSVC extension that production
code uses. A test parses every probe after the normalization. A second
test makes sure that each rule applies to at least one probe. Samples of
ordinary code must stay the same after the normalization, and so must
shapes from 64 codebases where a rule once read code wrongly.

elegance also reads the macros that a project declares to clang-format:
`AttributeMacros`, `StatementAttributeLikeMacros`, `StatementMacros`,
`ForEachMacros`, `IfMacros`, `TypenameMacros` and `NamespaceMacros`.

`--errors FILE` shows the rules that applied to a file, with the paper for
each construct, above the parse errors that stay.
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

**A receiver keeps its sigil, and for a long time that hid it.** The
chain-base test compared raw text against `self`/`cls`/`this`, which
`$this` and `$self` can never equal — so every PHP and Perl method read
as envying a foreign object that was itself. It cost PHP 996 of its
1,138 `feature envy` findings and Perl 228 of 382, and because the same
branch is the only writer of a class's own members, both languages
measured **zero** classes for `cohesion` while being made of little
else. One sigil now comes off before the comparison; a name that is
*nothing but* a sigil keeps it, because `$` is a whole identifier in
both TypeScript and Solidity.

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

**`spooky` reached three more languages, and one of them argued back.**
Go's `unsafe.Pointer` family, and a `FieldByName`/`MethodByName` whose
member name is computed rather than spelled — 3 findings in 185 files,
all three in go-cmp, where reading an unexported field is the library's
whole job. OCaml's `Obj`, whose own manual opens by saying it is not
type-safe — 15 findings in 3,730 files. Zig's inline `asm`, the one
place a Zig file stops being Zig — 1 finding in 1,132. Zig's `@field`
looks like the computed attribute access this metric was written for
and was left out anyway: 444 of its 454 uses in the corpus take a
computed name, because that is how the language walks a struct at
comptime, so counting it would have measured the idiom.

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
