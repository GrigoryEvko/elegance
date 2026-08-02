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

```
/plugin marketplace add GrigoryEvko/elegance
/plugin install elegance-nudge@elegance
```

That is the whole install. The plugin cannot ship the binary — most of it is
C compiled per architecture, and there are eight — so at session start it
fetches the one this machine needs from the latest release, for Linux
(x86_64/arm64, static musl), macOS (Intel/Apple Silicon) and Windows
(x86_64/arm64).

Three things about that, because a hook is going to execute what it downloads:

- **Verified.** Every release publishes a `.sha256` beside its binary, and a
  download that does not match it is deleted rather than run.
- **Visible.** It prints what it fetched and from where. A plugin that puts an
  executable on your disk silently has earned no trust.
- **Idempotent.** The latest tag is resolved with a single redirect — no API
  call, no rate limit — and nothing happens at all when that tag is already
  installed.

An `elegance` already on `PATH`, in `~/.cargo/bin` or `~/.local/bin` always
wins; somebody who built from source meant it:

```bash
cargo install --git https://github.com/GrigoryEvko/elegance
```

Hooks are snapshotted at startup, so the plugin takes effect in the next
session.

## Languages

Python, Rust, TypeScript, TSX, Go, JavaScript, Zig, C, OCaml, shell, C++ and
CUDA, plus Vue and Svelte components, GitHub Actions workflows, Dockerfiles and
Jupyter notebooks, all read at true file lines.
