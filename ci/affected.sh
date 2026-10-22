#!/usr/bin/env bash
# The checks of ci/test.sh that what changed since a commit can fail, for `.githooks/pre-push`
# and `make test-changed`:
#
#   ci/affected.sh <base> [<head>]
#
# prints the arguments for ci/test.sh (`--variant … --scope …`) for what changed from <base> to
# <head> (without it, to the working tree), or nothing when no check covers it (only
# documentation). By the paths, from the cheapest up:
#   code, contracts   any code, configuration or pack
#   results, data     what builds or searches the bundle: the providers, the geocoder, the build,
#                     the core, the packs and the languages; the golden of every country, or of
#                     the one whose pack alone changed
#   robustness        what serves it: the server, the host, the SDK and the search providers
#   instructions      the hot paths of a query
# A file of those whose diff only touches comments, logs or progress (`tracing::`, `progress::`),
# or a `progress.rs`, which only reports, counts for code and contracts alone: a log line does not
# move a hit. A changed line it cannot read as one of those counts in full, so the guess only ever
# errs on the side of more checks.
set -euo pipefail
cd "$(dirname "$0")/.."

base=${1:?usage: ci/affected.sh <base> [<head>]}
range=("$base" ${2:+"$2"})

# A changed line that cannot change what the code computes.
quiet='^[-+][[:space:]]*($|//|(tracing::)?(trace|debug|info|warn|error)!|.*progress::)'

# Whether the diff of `$1` only touches lines such as those.
only_quiet() {
    case $1 in */progress.rs) return 0 ;; esac
    case $1 in *.rs) ;; *) return 1 ;; esac
    ! git diff -U0 "${range[@]}" -- "$1" | grep -E '^[-+]' | grep -vE '^(\+\+\+|---) ' | grep -qvE "$quiet"
}

code=0 results=0 robustness=0 instructions=0 all_countries=0
countries=()
while IFS= read -r file; do
    case $file in
        *.md | docs/* | LICENSE | CLA.md | develop/* | Dockerfile | .github/* | .githooks/* | Makefile | ci/affected.sh) continue ;;
    esac
    code=1
    only_quiet "$file" && continue
    case $file in
        countries/*/*)
            country=${file#countries/} country=${country%%/*}
            [ -d "countries/$country/golden" ] && countries+=("$country")
            results=1 instructions=1 ;;
        languages/* | Cargo.lock | rust-toolchain.toml | ci/golden.sh | ci/test.sh | crates/geko-eval/* | crates/geko-bench/*)
            results=1 all_countries=1 robustness=1 instructions=1 ;;
        providers/geocode-fst/* | providers/reverse-rstar/* | providers/text-rules/* | modules/geko-geocoder/* | crates/geko-core/* | crates/geko-geom/*)
            results=1 all_countries=1 robustness=1 instructions=1 ;;
        providers/* | crates/geko-build/* | crates/geko-fetch/* | crates/geko-country-pack/*)
            results=1 all_countries=1 ;;
        crates/geko-server/* | crates/geko-app/* | crates/geko-module-sdk/*)
            results=1 all_countries=1 robustness=1 ;;
    esac
done < <(git diff --name-only "${range[@]}")

[ "$code" = 1 ] || exit 0
variants=code,contracts
[ "$results" = 1 ] && variants+=,results,data
[ "$robustness" = 1 ] && variants+=,robustness
[ "$instructions" = 1 ] && variants+=,instructions
if [ "$all_countries" = 1 ]; then
    scopes=$(for d in countries/*/golden; do d=${d#countries/}; echo "${d%%/*}"; done | paste -sd,)
else
    scopes=$(printf '%s\n' "${countries[@]}" | sort -u | paste -sd,)
fi
if [ "$results" = 1 ] && [ -n "$scopes" ]; then
    echo "--variant $variants --scope $scopes"
else
    echo "--variant code,contracts"
fi
