#!/usr/bin/env bash
# Fetch the pinned gold corpus from gold.toml into $1 (default /tmp/gold).
# Reproducible: each repo is checked out at its pinned SHA, so calibration
# derived from the corpus is auditable and re-derivable on any machine.
set -euo pipefail

target="${1:-/tmp/gold}"
manifest="$(dirname "$0")/gold.toml"

# Parse [[repo]] tables in file order. `prune` is optional and holds one
# directory to delete after checkout: a subtree that is in the repo but
# is not the repo's own measurable style — vendored third-party code, or
# a test suite whose bodies live inside quoted strings the parser cannot
# enter. Each one is justified where it is declared, in gold.toml.
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
    [ -n "$prune" ] && rm -rf "${dir:?}/${prune:?}"
done

echo "gold corpus ready in $target"
