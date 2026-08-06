#!/usr/bin/env bash
# Fetch the pinned gold corpus from gold.toml into $1, defaulting to a
# durable cache rather than /tmp: the corpus is 100 repositories and an
# hour of network, and /tmp is cleared on reboot. Losing it mid-session
# is not the worst of it — `elegance calibrate` pointed at the absence
# then had nothing to measure, and the run before this guard existed
# overwrote every budget in calibration.toml with an empty table.
# Reproducible: each repo is checked out at its pinned SHA, so calibration
# derived from the corpus is auditable and re-derivable on any machine.
set -euo pipefail

target="${1:-${XDG_CACHE_HOME:-$HOME/.cache}/elegance-gold}"
manifest="$(dirname "$0")/gold.toml"

# Parse [[repo]] tables in file order. `prune` is optional and holds one
# or more space-separated directories to delete after checkout: subtrees
# that are in the repo but are not the repo's own measurable style —
# vendored third-party code, a generated amalgamation of the library
# beside the library, or a test suite whose bodies live inside quoted
# strings the parser cannot enter. Each one is justified where it is
# declared, in gold.toml. It stays LAST in the row so that `read` hands
# it the whole remainder of the line however many entries it holds.
mapfile -t rows < <(awk -F'"' '
    /^\[\[repo\]\]/ { if (sha != "") print lang, name, url, sha, prune;
                      lang=""; name=""; url=""; sha=""; prune="" }
    /^lang/  { lang=$2 } /^name/ { name=$2 } /^url/ { url=$2 }
    /^sha/   { sha=$2 }  /^prune/ { prune=$2 }
    END      { if (sha != "") print lang, name, url, sha, prune }' "$manifest")

for row in "${rows[@]}"; do
    read -r lang name url sha prune <<< "$row"
    dir="$target/$lang/$name"
    if [ -d "$dir/.git" ] && [ "$(git -C "$dir" rev-parse HEAD)" = "$sha" ]; then
        echo "ok       $lang/$name @ ${sha:0:12}"
        continue
    fi
    echo "fetching $lang/$name @ ${sha:0:12}${prune:+ (pruning $prune/)}"
    rm -rf "$dir"
    mkdir -p "$dir"
    git -C "$dir" init -q
    git -C "$dir" remote add origin "$url"
    git -C "$dir" fetch -q --depth 1 origin "$sha"
    git -C "$dir" checkout -q FETCH_HEAD
    for sub in $prune; do rm -rf "${dir:?}/${sub:?}"; done
done

echo "gold corpus ready in $target"
