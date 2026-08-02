# elegance-nudge

Measures every file Claude edits and reports, in one line, **only what that
edit introduced**.

```
elegance store.go: fetch:44 cognitive 31>22, magic numbers 14>9 · file:1 secrets
```

Read it as `unit:line metric value>budget`. Findings are grouped by the unit
they live in, because "one function has three problems" is a different fact
from "three functions have one each", and the budget travels with the value
because `cognitive 31` alone says nothing about how far over the line it is.
Budgets are per language, pinned to gold-corpus percentiles.

## What it deliberately does not do

- **It never blocks.** Advisory only. A hook that fails the turn turns every
  judgement call into an argument.
- **It never repeats.** Findings are diffed against the previous run on that
  file, so a problem is mentioned once per session. Ignoring it is allowed and
  costs no further context; reintroducing it says so again.
- **It is silent on a clean file**, which is the common case and must cost
  nothing. A nudge that fires every turn stops being read.

## Install

Needs the `elegance` binary on `PATH`, in `~/.local/bin`, or in `~/.cargo/bin`:

```bash
cargo install --git https://github.com/GrigoryEvko/elegance
```

Then, in Claude Code:

```
/plugin marketplace add GrigoryEvko/elegance
/plugin install elegance-nudge@elegance
```

The hook is registered automatically and takes effect in the next session —
hook configuration is snapshotted at startup.

## Languages

Python, Rust, TypeScript, TSX, Go, JavaScript, Zig, C, OCaml, shell, C++ and
CUDA, plus Vue and Svelte components, GitHub Actions workflows, Dockerfiles and
Jupyter notebooks, all read at true file lines.
