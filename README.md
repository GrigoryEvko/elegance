# elegance

`elegance` measures source code in 22 languages, compares each measurement
with a budget, and reports the functions that exceed it.

**The checks are deterministic.** Every file is parsed with tree-sitter and
every metric is computed from the syntax tree. There is no language model and
no network call, so the same source always produces the same report.

**The budgets come from data.** For most metrics the budget is the 99th
percentile (p99) of that metric over the gold corpus: 100 open-source
repositories, pinned by commit, chosen for the quality of their code. A finding
says that a function sits outside what 99% of that code does.

**The level of a check decides what it may do.** A check that reads one
function is exact enough to fail a build. A check that reads a class or a
dependency graph needs context to interpret, so it only reports.

```
$ elegance .
elegance — 65 files (rs 62, sh 3), 2065 units (28% test), 57427 lines, 1 with parse errors

gates — 41 over budget across 26 bodies. Old sludge is tolerated until touched, new sludge is blocked.
     16x  1 gate       src/lang/cppseed.rs:363  sha256
          magic numbers 31>2
          magic numbers — alone here, the other 10 peak at 1
          48 files import this one
      2x  3 gates      src/lang/cpp/dialect.rs:2542  bitfield_initializer
          cyclomatic 33>14  cognitive 22>16  length 111>93
```

## The ladder

Every metric sits on one rung, and the rung is the scope of code the metric
reads. Low rungs read little and are exact. High rungs read more and need a
person to interpret them. The rung therefore fixes what a finding is allowed to
do.

| Rung | Scope | Class | What a finding does |
| :--- | :--- | :--- | :--- |
| 0 | token | gate | can fail a build |
| 1 | expression | gate | can fail a build |
| 2 | function | gate | can fail a build |
| 3 | interface | suspicion | is reported for a person to judge |
| 4 | class and module | suspicion | is reported for a person to judge |
| 5 | dependency graph, rates | description | states a fact, with no budget |
| 6 | evolution | description | states a fact about how the code changes |
| 7 | tension | evidence | shows facts that are worse together |

`--baseline check` and `--diff` exit 1 on a gate. `--fail-on RUNG` moves the
line between gates and suspicions. The default is 2.

### Budgets

A budget is one of two kinds:

- **Calibrated** (`C` in the tables below): taken from the gold corpus, one
  value per language. An upper limit is the gold p99. A band is the gold p05
  and p95.
- **Policy** (`P`): a fixed rule that calibration never touches. Most policy
  budgets are zero, because the thing they count has no acceptable density.

A policy metric keeps its rung only while the gold corpus obeys it. `elegance
calibrate` lists every policy gate the corpus breaks in more than 1% of
measurements, and every policy suspicion it breaks in more than 5%. A rule that
admired code fails that often is measuring taste, and it moves up the ladder.

### Rung 0: tokens

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `secrets` | P | a credential in the source. A credential-shaped name is not enough: the value must also look like a real key |
| `magic numbers` | C | unnamed non-trivial numbers outside constants and tests |
| `abbreviated` | C | names with the vowels squeezed out: `usr`, `cfg`, `mgr` |

### Rung 1: expressions

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `expr depth` | C | the deepest expression tree on a single line |
| `demeter` | C | chains of three or more data links (`a.b.c.d`). Fluent calls, namespace paths and tests are exempt |
| `negations` | P | a negated `!=`, or a negated name that is already negative |

### Rung 2: functions

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `cognitive` | C | cognitive complexity (SonarSource): each branch costs 1 plus its nesting depth |
| `cyclomatic` | C | cyclomatic complexity (McCabe): decision points plus 1 |
| `depth` | C | maximum nesting depth |
| `length` | C | lines in the body |
| `params` | C | declared parameters |
| `live span` | C | the longest stretch of lines over which one local variable stays in use |
| `swallowed` | P | an error handler that does nothing with the error |
| `built query` | P | an SQL statement assembled by interpolating a value |
| `shelled out` | P | a value spliced into a command that a shell will parse again |
| `blocking async` | P | a call that parks the thread inside an `async` function |
| `conditional hook` | P | a React hook called inside a branch |

### Rung 3: interfaces

Signatures:

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `flag params` | P | a boolean parameter that switches the function's behavior |
| `bool traps` | C | a call passing two or more positional boolean literals |
| `kw opacity` | P | a public function that takes `**kwargs` |
| `confusable` | C | adjacent parameters of the same declared type |
| `stringly id` | C | a parameter that carries an identity but is typed as text |
| `returns` | C | the width of a returned tuple |
| `interface width` | C | methods per declared interface |
| `ceremony` | P | a declaration with no parameters, a single literal for a body, and three or more lines of documentation |
| `doc param` | P | a parameter the documentation names and the signature does not declare |

Names:

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `generic name` | P | a name built only from filler words |
| `lying name` | P | an `is_` or `has_` function that does not return a boolean, or a `get_` that mutates |
| `terse name` | C | a short name that stays live for many lines. The value is the span |
| `repurposed` | C | a variable given a second, unrelated meaning |

Errors:

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `broad catch` | P | a handler that catches every error type |
| `lost context` | P | a handler that raises a new error and drops the original |
| `unwraps` | P | more than two `unwrap` or `expect` calls outside a test |
| `spooky` | P | action at a distance: `eval`, computed attribute access, metaclasses, `transmute`, inline assembly |

Types:

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `untyped params` | C | parameters with no declared type, in languages that have type syntax |
| `loose types` | C | types that assert nothing: `Any`, `any`, `interface{}`, `anytype`, `void *` |
| `casts` | C | casts per function |
| `suppressions` | P | a comment telling a type checker to stop looking |
| `wildcard match` | C | a catch-all arm in a `match` |

Tests:

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `test asserts` | P | a declared test that asserts nothing |
| `vacuous asserts` | P | an assertion on a literal, which passes whatever the code does |
| `sleepy test` | C | a test that waits for a duration instead of a condition |
| `skipped tests` | C | a test switched off unconditionally |

Concurrency and loops:

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `unawaited coroutine` | P | a bare statement calling an `async` function of the same file without `await` |
| `dropped tasks` | C | a spawned task whose handle is discarded |
| `loop depth` | C | loops nested inside loops |
| `alloc in loop` | C | a copying call inside a loop: `to_string`, `to_owned`, `to_vec`, `deepcopy` |

Comments and documentation:

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `echo comments` | P | a comment that restates the code next to it |
| `commented code` | C | a comment block that parses as two or more statements |
| `comment ratio` | C | the share of comment lines in a file. This budget is a band |
| `module doc` `type doc` `fn doc` `field doc` `inline doc` | C | prose words in one comment, with a separate budget per role. Code examples inside a comment do not count |

### Rung 4: classes and modules

| Metric | Budget | What it finds |
| :--- | :--- | :--- |
| `cohesion` | C | LCOM4: the number of disconnected groups a class's methods fall into. One is cohesive |
| `feature envy` | P | a method that works on another object's data more than on its own |
| `pass-through` | P | a function that only forwards its arguments |
| `and name` | P | a name containing "and", which admits to two jobs |
| `magic strings` | C | the same string literal written out again and again |

### Rung 5: dependency graph and rates

The tool resolves every import to a file in the repository and describes the
graph that results. Nothing on this rung has a budget.

| Fact | Meaning |
| :--- | :--- |
| cycle mass | share of files, and of directories, inside an import cycle |
| depth | longest dependency chain: p50, p90 and maximum |
| load-bearing | files with the most direct and transitive dependents |
| deletable | share of files nothing depends on |
| orphans | files with no importer, no test importer and no entry-point name |
| tested only | files imported only by tests. Counted apart from orphans |
| dead exports | exported symbols no other file mentions |
| fat surface | files whose importers use few of their exports |
| shallow-wide | files with a wide public surface and little behind it |
| clones | the same logic in two or more places, whatever the names and literals |
| near-clones | copies that were edited afterwards |
| param clumps | groups of parameters that travel together across signatures |
| repeated dispatch | the same set of `case` labels switched on in three or more places |
| narrative | share of calls within a file that point down to a function defined later |
| idiom | how consistently each language's names are spelled |
| `asserts` `public docs` | coverage rates, shown beside the gold corpus's own rate |

### Rung 6: evolution

`--hotspots` and `--coupling` read the git history. `--history` keeps its own
ledger of past scans, and `--by` regroups the findings of the current one.

| Mode | Fact |
| :--- | :--- |
| `--hotspots` | complexity ranked by how often the code is edited |
| `--history record\|show` | the trend of findings across recorded scans |
| `--by` | findings rolled up per directory, worst first |
| `--coupling` | files that change together without a declared dependency, and files with a single author |

### Rung 7: tensions

A tension is a set of independent facts about one function that are worse
together than apart: it is over budget, no test names it, many files import its
file, and it holds duplicated logic.

Three independent facts must line up. Metrics that move together count as one,
since `cognitive`, `cyclomatic`, `length` and `live span` all rise because a
function is big. There is no composite score, because a score would hide which
of the facts is true.

## Install

```sh
cargo install elegance-rs
```

The crate is `elegance-rs`. The command is `elegance`.

Each release tag also builds binaries for eight targets, each with a `.sha256`
file, on the [releases page](https://github.com/GrigoryEvko/elegance/releases).
The musl builds are static.

| | Linux | macOS | Windows |
| :--- | :--- | :--- | :--- |
| x86_64 | `x86_64-linux-gnu` `x86_64-linux-musl` | `x86_64-macos` | `x86_64-windows.exe` |
| arm64 | `aarch64-linux-gnu` `aarch64-linux-musl` | `aarch64-macos` | `aarch64-windows.exe` |

### Claude Code plugin

```
/plugin marketplace add GrigoryEvko/elegance
/plugin install elegance-nudge@elegance
```

After each edit the plugin reports, in one line, the findings that edit
introduced. It is silent on clean files. See
[plugins/elegance-nudge](plugins/elegance-nudge/README.md).

## Commands

| Command | What it does |
| :--- | :--- |
| `elegance [paths...]` | scan. The default path is `.` |
| `elegance --brief` | the summary only |
| `elegance --full [--top N]` | every section, and the worst functions per metric |
| `elegance --explain file[:line]` | the score of one function, line by line |
| `elegance --context` | the repository's measured style, to read before writing code |
| `elegance --json` | machine output, schema 2 |
| `elegance --sarif` | SARIF 2.1.0, for GitHub code scanning |
| `elegance --baseline write` | record today's violations as the baseline |
| `elegance --baseline check` | exit 1 on a violation that is new or worse than the baseline |
| `elegance --diff REF` | judge only the code a change touched |
| `elegance --api REF` | breaking changes to the public surface |
| `elegance --fail-on RUNG` | the highest rung that may fail a build |
| `elegance install-hook` | a pre-commit hook running `--diff HEAD`. `uninstall-hook` removes it |
| `elegance calibrate <gold-dirs...>` | derive budgets from a gold corpus and write `calibration.toml` |
| `elegance tidy FILES... [-- FLAGS]` | clang-tidy on C++ that its Clang does not read yet |
| `elegance --cpp-seed FILE` | the names this C++ project declares. The default is `.elegance/cpp.seed` |
| `elegance --errors FILE` | the parse errors of one file, and the C++ rules that applied to it |
| `elegance --facts-dump FILE` | every unit, call and control event as rows, for language-pack developers |
| `elegance --version` | the version |

Beyond source code:

| Mode | What it does |
| :--- | :--- |
| `--deps` | looks inside the dependencies you did not write: credentials, `spooky` code, suppressions, volume per package |
| `--helm` | compares the keys each Helm values file sets, and finds credentials in YAML |
| `--render` | runs `helm template` per environment and measures the manifests that come back |

### `elegance tidy`

clang-tidy parses with the Clang it was built with, and the Clang in each
release reads only part of C++26. A contract clause that Clang cannot parse is
an error, and no check then examines that declaration.

`elegance tidy` removes, for clang-tidy only, what its Clang does not read and
what does no work: contract clauses, `contract_assert`, annotations and class
properties. A virtual file system overlay puts the changed text at the original
path, so each diagnostic shows the original file, line and column. No file on
disk changes.

```sh
elegance tidy --checks='bugprone-*' src/pool.cpp -- -std=c++26 -Iinclude
elegance tidy --clang-tidy clang-tidy-23 -p build src/pool.cpp
```

`-p BUILD` reads `compile_commands.json` and drops from a copy of it the flags
this clang-tidy does not know, such as `-fcontracts` from a GCC build.

Reflection, a consteval block and, before Clang 23, an expansion statement have
no removal that keeps their meaning. The command reports how many files use
each one, and clang-tidy reports each use as an error. `--fix` is not
available, because clang-tidy would compute the fix on the changed text.

## Reading the report

An entry is one function, not one finding, and entries are ranked by distance
from the budget.

```
     16x  1 gate       src/lang/cppseed.rs:363  sha256
          magic numbers 31>2
          magic numbers — alone here, the other 10 peak at 1
          48 files import this one
```

| Line | Content |
| :--- | :--- |
| 1 | how far past budget, how many budgets, where, and the name |
| 2 | each metric over budget, as value > budget |
| 3 | what the other functions in the same file do with that budget |
| 4 | what else the finding costs: importers of the file, a missing test, the same finding across the file, or identical bodies |

The sections:

| Section | Content |
| :--- | :--- |
| `gates` | functions over a budget on rungs 0 to 2 |
| `suspicions` | functions over a budget on rungs 3 and 4 |
| `policy` | findings against a budget of zero, which have no distance to rank by |
| `shape` | each metric's distribution in this scan, beside its budget |
| `coverage` | the two rate metrics, beside the gold corpus's rates |
| `architecture` | the facts of rung 5 |

A budget is printed in one of three forms:

| Printed | Rests on |
| :--- | :--- |
| `<=33` | a percentile of the gold corpus |
| `<=12.` | the compiled-in default. The trailing period marks a budget the corpus did not set |
| `<=76 py · <=115 cpp` | a mixed scan: one budget per language |

`--json` carries the same information in a `budget_source` field per metric. It
also gives a `measured` count per language and metric. Use `measured`, not
`units`, as the denominator of a rate.

## Continuous integration

```yaml
# .github/workflows/quality.yml
- run: elegance --baseline check .
- run: elegance --diff 'origin/main...HEAD' .
- run: elegance --sarif . > elegance.sarif
```

The baseline identifies a violation by metric, path and function name. Line
numbers are not part of the key, so an edit elsewhere in the file changes
nothing.

`--baseline check` tolerates what the baseline recorded and fails on a
violation that is new or has grown.

On a pull request, use three dots. `origin/main...HEAD` compares against the
merge base. Plain `origin/main` compares against the branch tip, so every merge
into `main` since you branched reads as your change.

## Configuration

`.elegance.toml` at the scan root. A `.elegance.toml` in a subdirectory
overrides the budgets for that subtree only.

```toml
exclude = ["migrations/**", "*_pb2.py"]
skip_dirs = ["fixtures"]

[budgets]
length = { hi = 100 }
"comment ratio" = { lo = 0.05, hi = 0.5 }
```

Always skipped:

- anything `.gitignore` excludes
- the directories `__pycache__`, `node_modules`, `target`, `venv`, `.venv`,
  `dist`, `vendor` and `third_party`
- generated files: a marker in the head of the file that opens its line or
  sits on a comment line. The markers are `@generated`, `do not edit`,
  `automatically generated`, `autogenerated`, `auto-generated` and
  `code generated by`
- minified bundles: a `.min.js` or `.min.mjs` file with a line longer than 200
  characters

### Layer contracts

A layer contract is the one architecture rule that can fail a build. You
declare which layer may import which:

```toml
[layers.gallery]
paths = ["src/gallery"]
may_import = ["shared"]

[layers.shared]
paths = ["src/shared"]        # no may_import: this layer imports nothing
```

A file belongs to a layer when its path contains one of the layer's `paths`,
and the longest match wins. `--baseline check` fails on every import the
contract forbids, and the baseline cannot record one: a rule the repository
wrote down is as wrong on the thousandth day as on the first. A file in no
layer is not judged, and neither is an import the tool cannot resolve.

## Languages

| Language | Key | Extensions | Notes |
| :--- | :--- | :--- | :--- |
| Python | `py` | `.py` | |
| Rust | `rs` | `.rs` | |
| TypeScript | `ts` | `.ts` `.mts` `.cts` | |
| TSX | `tsx` | `.tsx` | shares the TypeScript pack |
| JavaScript | `js` | `.js` `.mjs` `.cjs` `.jsx` | no type syntax, so no type metrics |
| Go | `go` | `.go` | |
| Zig | `zig` | `.zig` | |
| C | `c` | `.c` `.h` | not preprocessed. `demeter` does not apply |
| C++ | `cpp` | `.cpp` `.cc` `.cxx` `.hpp` `.hh` `.hxx` `.ipp` `.inl` `.h` | a gtest `TEST(a, b)` is named `a.b` |
| CUDA | `cu` | `.cu` `.cuh` `.h` | shares the C++ pack, with its own budgets |
| OCaml | `ml` | `.ml` `.mli` | `unwraps` does not apply: `failwith` is the raise |
| shell | `sh` | `.sh` `.bash` | no declared parameters, so no interface metrics |
| Perl | `pl` | `.pl` `.pm` `.t` | parameters only where signatures are used |
| PHP | `php` | `.php` | |
| Ruby | `rb` | `.rb` `.rake` `.gemspec` | a block passed to `each` or `map` counts as a loop |
| Lua | `lua` | `.lua` | no classes, so no class metrics |
| Java | `java` | `.java` | no `async` syntax, so no `blocking async` |
| C# | `cs` | `.cs` | |
| Swift | `swift` | `.swift` | `unwraps` counts `!` and `try!` |
| Scala | `scala` | `.scala` `.sc` | |
| Elixir | `ex` | `.ex` `.exs` | `def`, `if` and `case` are calls, classified by name |
| Solidity | `sol` | `.sol` | inline assembly is `spooky` |

A file with no extension takes its language from its shebang: `sh`, `bash`,
`dash`, `ksh`, `zsh`, `perl`, `python`, `ruby`, `lua` or `luajit`.

Code inside other files is read too. Everything that is not code is blanked
rather than removed, so line numbers stay true.

| File | What is read |
| :--- | :--- |
| `.vue` `.svelte` | `<script>` blocks, as TypeScript or JavaScript |
| `.ipynb` | code cells |
| `.github/workflows/*.yml` | `run:` blocks, as shell |
| `Dockerfile` | `RUN`, `ENV` and `ARG` bodies, as shell |

### Limits

- **`.h` does not name a language, so the text decides.** A line opening with
  `namespace`, `template<`, an access specifier, or `class` and a name makes
  the header C++. That rule matches 0 of 1,027 headers from four C repositories
  and 98 of 104 from three C++ ones. A CUDA keyword such as `__global__` makes
  it CUDA.
- **C is not preprocessed.** Macro bodies are invisible, so C numbers are
  floors. `#if`, `#elif` and `#else` count as branches.
- **C++ is normalized before it is parsed, up to C++2d.** The grammar is older
  than the language, and one parse error removes the rest of its scope from the
  tree. The normalization removes what does no work (contracts, attributes,
  specifiers, reflection operators) and keeps every call, branch, loop and byte
  offset, so findings point at the right line. `src/lang/cpp/dialect_probes.txt`
  holds 676 probe sources, and a test parses each one after the normalization.
  The macros a project declares to clang-format are read too.
- **A file with too many parse errors is excluded from the metrics.** The first
  line of the report counts the excluded files.
- **Haskell and PowerShell have no pack.** Their tree-sitter grammars could not
  parse the candidate repositories. `gold.toml` records the measurements.

## Calibration

`gold.toml` pins the 100 repositories of the gold corpus by commit, four or
more per language. `elegance calibrate` turns them into budgets under these
rules:

- A repository speaks only for its declared language. Shell is the one
  exception, since most shell lives inside other languages' repositories.
- A calibrated budget is the p99 of the metric for one language, or the p05 and
  p95 for a band.
- The corpus sets a budget only from 200 samples or more. Below that the
  compiled-in default stays.
- A p99 of zero sets no budget.
- Policy budgets do not move.

The gold p99 budgets for six metrics follow. A test compares every row with the
compiled calibration, so the table cannot drift.

| Language | `cognitive` | `cyclomatic` | `length` | `params` | `magic numbers` | `fn doc` |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| `py` | 18 | 12 | 76 | 10 | 11 | 89 |
| `rs` | 16 | 14 | 93 | 4 | 2 | 258 |
| `ts` | 32 | 20 | 106 | 7 | 2 | 127 |
| `tsx` | 34 | 22 | 168 | 4 | 7 | 137 |
| `go` | 31 | 24 | 202 | 3 | 2 | 163 |
| `js` | 19 | 12 | 79 | 4 | 3 | 125 |
| `zig` | 40 | 30 | 211 | 6 | 2 | 149 |
| `lua` | 65 | 34 | 155 | 5 | 4 | 151 |
| `rb` | 30 | 15 | 37 | 4 | 1 | 134 |
| `pl` | 27 | 16 | 79 | - | 3 | 45 |
| `php` | 32 | 19 | 104 | 6 | 3 | 88 |
| `java` | 12 | 10 | 58 | 5 | 3 | 150 |
| `cs` | 8 | 9 | 60 | 5 | 2 | 315 |
| `swift` | 11 | 10 | 83 | 6 | 6 | 162 |
| `scala` | 7 | 7 | 54 | 5 | 2 | 94 |
| `ex` | 5 | 8 | 91 | 6 | 1 | 105 |
| `sol` | 9 | 7 | 49 | 6 | 12 | 150 |
| `c` | 80 | 43 | 205 | 7 | 21 | 176 |
| `ml` | 6 | 8 | 52 | 5 | 1 | 118 |
| `sh` | 45 | 36 | 103 | - | 4 | 105 |
| `cpp` | 32 | 22 | 115 | 5 | 10 | 147 |
| `cu` | 55 | 30 | 249 | 15 | 30 | 93 |

A `-` means the corpus set no budget and the compiled-in default applies.

The same test holds two statements about this table true:

- Ruby has the smallest `length` budget of the 22 languages.
- The C++ budget for `cognitive` is less than half of the C budget.

The calibration is compiled into the binary. To redo it, fetch the corpus,
calibrate, and rebuild:

```sh
./gold-fetch.sh DIR
elegance calibrate DIR
cargo build --release
```

## Design

```
source file -> language pack -> facts -> metrics -> report
```

A language pack is a table mapping each node kind of one grammar to a semantic
kind, plus a few hooks. The set of semantic kinds (`Sem`) is closed and shared
by every language.

The extractor walks a file's syntax tree once and writes language-independent
facts. A metric is a pure function over those facts and never sees a syntax
tree. A conformance suite checks that the same function, written in each
language, produces the same metric values.

```
src/
  sem.rs         the closed set of semantic kinds
  lang/          one pack per language, and the C++ dialect normalization
  facts/         the facts and the extractor
  metrics/       the metric registry, the budgets and the metric functions
  graph/         import resolution and the facts of rung 5
  report/        aggregation, and the text, JSON and SARIF output
  calibrate.rs   budgets from the gold corpus
  ratchet.rs  diff.rs  api.rs               the CI modes
  layers.rs                                 layer contracts
  ci.rs  sfc.rs  notebook.rs                workflows, Dockerfiles, components, notebooks
  tidy.rs  clangfmt.rs  factsdump.rs        `tidy`, clang-format macros, `--facts-dump`
  near.rs  prose.rs  docparam.rs            near-clones, comment prose, documented parameters
  hotspots.rs  history.rs  coupling.rs  rollup.rs   the modes of rung 6
  deps.rs  helm.rs  render.rs               dependencies and Helm charts
  main.rs        the command line, the file walk and the parallel scan
```

## Performance

One scan of the gold corpus checkouts with `--brief`, on 2026-11-11:

| | |
| :--- | :--- |
| Input | 14,129,382 lines in 60,176 files, 655,002 units |
| Time | 22.4 s |
| Peak memory | 1.64 GB |
| Machine | Intel Core i7-14700HX, 28 threads |

## License

MIT
