#!/usr/bin/env bash
# The checks of every change, the same locally and on the CI (GitHub Actions): the CI
# files only prepare Rust and the cache, then run this script. Only checks: the data of the golden
# are prepared by ci/golden-snapshot.sh.
#
#   ci/test.sh [--variant <variants>] [--scope <scopes>] [--include-performance]
#
# --variant, what is checked, comma-separated (default: all of them):
#   code          clippy (warnings are errors), dependency rules (what geko-core and each
#                 technology may use), licenses and security advisories (cargo-deny), and clippy
#                 on each combination of modules, sources and packs
#   contracts     unit tests and the conformance suites of every port
#   results       the reference queries and every keystroke of them, hit by hit, with the
#                 calibration of `confidence`, duplicated results and the Pelias and Photon modes
#   data          the canonical dataset (entities, duplicates, integrity), the size of the bundle
#                 and the determinism of the build
#   robustness    fuzz, and a reload of the bundle under load
#   instructions  the instructions of the hot paths, counted by valgrind
# --scope, on which data, comma-separated (default es,lu); `code` and `contracts` need none:
#   es, lu, …     the golden of a country pack: Baleares for es, the whole country otherwise, on
#                 the frozen data named in countries/<country>/golden/snapshot
#   spain         the whole of Spain against the last pushed commit, on the data of
#                 /var/lib/geko (local, on demand: make test-regression; see spain())
#   pelias        the golden of es against the instance of Pelias of GEKO_PELIAS_URL (results only)
# --include-performance  also what depends on the machine: the latencies of the golden against
#                 those of this machine, and the load, the hit rate under load and the server of
#                 spain against those of its base
#
# The golden builds and serves a bundle of a region of a country, and fails if any category of
# any query set of the country pack (countries/<country>/golden/) is worse than its baseline
# (golden/baseline/). Its data are the frozen downloads named in golden/snapshot, so that only a
# change in the code can change the results: the baseline is measured on them. Without a
# snapshot, or with GEKO_GOLDEN_FRESH=1, it downloads the sources (PROJECT.md §7, accepting
# their licenses) instead; CartoCiudad is a public service, so do that on a schedule or by hand.
# A snapshot not in $GEKO_GOLDEN_DIR/snapshots is pulled from
# ghcr.io/edgaza/geko/countries/<country>/golden:<snapshot>, a private package that the CI reads
# with its own token and a machine after `oras login ghcr.io`. Environment, besides that of
# ci/golden.sh:
#   GEKO_GOLDEN_SETS       query sets of the pack (default: those listed in golden/sets, and every
#                          keystroke of the generated queries, `keystrokes`)
#   GEKO_GOLDEN_PORT       port of the server (default 18080)
#   GEKO_GOLDEN_DISABLE    sources left out, space separated (default "overture": the frozen data
#                          has none yet; set it empty to build with every source)
#   GEKO_GOLDEN_TOLERANCE  allowed drop per category, in percentage points (default 0). Fresh
#                          data shift results a little; scheduled runs may allow 1.
#   GEKO_GOLDEN_UPDATE=1   write the new reports as the baseline instead of checking
#   GEKO_LATENCY_UPDATE=1  record the latencies of this machine (kept in GEKO_GOLDEN_DIR, not in
#                          the repository), which later runs with --include-performance compare with
#   GEKO_GOLDEN_FRESH=1    download the sources instead of using the snapshot
#   GEKO_SNAPSHOT_MAX_AGE_DAYS  age past which the golden warns that its snapshot should be
#                          renewed (default: golden/snapshot-max-age-days of the pack, or 60); it
#                          never fails for that
#   GEKO_FUZZ_REQUESTS, GEKO_FUZZ_SEED  random and mutated requests sent after the queries (default
#                          5000, seed 1): any 5xx or dropped connection fails
#   GEKO_BENCH=0           skip the instruction counts, which need valgrind (an error without it)
#   GEKO_BASE              the commit that the instructions and spain compare with (default: the
#                          last pushed one, where HEAD leaves its upstream)
#   GEKO_AB_DIR            where the worktree of that commit and its builds are kept (default
#                          /var/lib/geko/ab)
#   GEKO_GOLDEN_PROFILE    Cargo profile (default release). `ci` compiles much faster; use it
#                          where latencies are not compared
#   GEKO_GOLDEN_SNAPSHOT_URL  a base URL to download the snapshot archives from instead
set -euo pipefail
cd "$(dirname "$0")/.."
# The country packs as they are in this repository: the checks test them before they are published.
export GEKO_PACKS="${GEKO_PACKS:-$PWD/countries}"
# shellcheck source=ci/golden.sh
. ci/golden.sh

lint() {
    rustup component list --installed 2>/dev/null | grep -q '^clippy' || rustup component add clippy
    cargo clippy --workspace --all-targets --locked -- -D warnings
}

test() {
    cargo test --workspace --locked
}

# Modules, sources and country packs are chosen by Cargo feature: each one left out must still
# compile and pass clippy. Without the pack of Spain, or with only another one, the engine must
# not depend on it.
features() {
    local set failed=0
    for set in "" geocoder,source-osm,geocode-fst,reverse-rstar \
        tiles,source-osm geocoder,tiles,source-osm,geocode-fst,reverse-rstar; do
        echo "geko-app features: [${set}]"
        cargo clippy -p geko-app --no-default-features --features "$set" --all-targets --locked -- -D warnings || failed=1
    done
    return $failed
}

direct_dependencies() { cargo tree -p "$1" -e normal --depth 1 --prefix none --locked | tail -n +2 | cut -d' ' -f1 | sort -u; }

# Crates of this workspace that depend on `$1` directly.
direct_dependents() {
    cargo tree -i "$1" -e normal --depth 1 --prefix none --workspace --locked 2>/dev/null | tail -n +2 \
        | grep -F " ($PWD/" | cut -d' ' -f1 | sort -u
}

# Fails, naming them, if `found` (one per line) has any name not in `allowed` (space-separated).
only() {
    local what=$1 allowed=$2 found=$3 name bad=""
    for name in $found; do
        [[ " $allowed " == *" $name "* ]] || bad="$bad $name"
    done
    [ -z "$bad" ] || { echo "$what:$bad" >&2; return 1; }
}

# The rules of AGENTS.md that the dependency graph can show: geko-core holds only the domain and
# the traits, and each concrete technology stays inside its provider or its crate. A new user of
# one of them is either a rule broken or a decision to write down here.
architecture() {
    local failed=0
    only "geko-core depends on" "serde serde_json thiserror" "$(direct_dependencies geko-core)" || failed=1
    only "fst is used by" "geocode-fst" "$(direct_dependents fst)" || failed=1
    only "levenshtein_automata is used by" "geocode-fst" "$(direct_dependents levenshtein_automata)" || failed=1
    only "osmpbf is used by" "source-osm" "$(direct_dependents osmpbf)" || failed=1
    only "ureq is used by" "geko-fetch geko-eval" "$(direct_dependents ureq)" || failed=1
    only "tokio is used by" "geko-server geko-app" "$(direct_dependents tokio)" || failed=1
    local http="geko-server geko-module-geocoder geko-module-tiles"
    only "axum is used by" "$http" "$(direct_dependents axum)" || failed=1
    only "tower-http is used by" "$http" "$(direct_dependents tower-http)" || failed=1
    return $failed
}

CARGO_DENY_VERSION=0.20.2

# Licenses of the dependencies (deny.toml) and security advisories of the versions in Cargo.lock.
# The licenses of the data sources are checked by the conformance suite of every source.
deny() {
    local bin
    bin=$(command -v cargo-deny || true)
    if [ -z "$bin" ] || ! "$bin" --version | grep -q " $CARGO_DENY_VERSION\$"; then
        local dir="${CARGO_HOME:-$HOME/.cargo}/bin" name="cargo-deny-$CARGO_DENY_VERSION-x86_64-unknown-linux-musl"
        mkdir -p "$dir"
        curl -fsSL "https://github.com/EmbarkStudios/cargo-deny/releases/download/$CARGO_DENY_VERSION/$name.tar.gz" \
            | tar -xzf - -C "$dir" --strip-components=1 "$name/cargo-deny"
        bin="$dir/cargo-deny"
    fi
    "$bin" --locked check
}

# Warns when the snapshot named in `snapshot` is older than the days of its pack
# (golden/snapshot-max-age-days, by how fast its sources change; GEKO_SNAPSHOT_MAX_AGE_DAYS
# overrides it; 60 without either): the sources have moved on since, and the golden no longer
# measures what users would see. It does not fail: the snapshot is still what the baseline was
# measured on.
snapshot_age() {
    local snapshot=$1 country=$2 file day age max
    max=${GEKO_SNAPSHOT_MAX_AGE_DAYS:-$(grep -v '^#' "$(dirname "$snapshot")/snapshot-max-age-days" 2> /dev/null | tr -d '[:space:]' || true)}
    max=${max:-60}
    file=$(snapshot_field "$snapshot" file)
    day=$(echo "$file" | grep -o '[0-9]\{8\}' | tail -1 || true)
    [ -n "$day" ] || return 0
    age=$((($(date -u +%s) - $(date -u -d "$day" +%s)) / 86400))
    [ "$age" -gt "$max" ] || return 0
    echo "warning: the snapshot of $country is $age days old ($file): renew it (docs/PROJECT.md §20.1)" >&2
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        echo "::warning::the snapshot of $country is $age days old: renew it (docs/PROJECT.md §20.1)"
    fi
}

# Puts the frozen downloads named in `snapshot` into `dir/raw`, unless they are already there.
restore_snapshot() {
    local snapshot=$1 dir=$2 country=$3
    local file sha archive
    file=$(snapshot_field "$snapshot" file) sha=$(snapshot_field "$snapshot" sha256)
    [ "$(cat "$dir/raw/.snapshot" 2>/dev/null)" = "$sha" ] && return 0
    archive="$dir/snapshots/$file"
    if [ ! -f "$archive" ]; then
        mkdir -p "$dir/snapshots"
        if [ -n "${GEKO_GOLDEN_SNAPSHOT_URL:-}" ]; then
            curl -fsSL -o "$archive.part" "${GEKO_GOLDEN_SNAPSHOT_URL%/}/$file"
            mv "$archive.part" "$archive"
        elif ! (cd "$dir/snapshots" && oras pull --no-tty "$(snapshot_ref "$country" "$file")" > /dev/null); then
            echo "golden data $file is neither in $dir/snapshots nor in $(snapshot_ref "$country" "$file") (oras login ghcr.io?); GEKO_GOLDEN_FRESH=1 downloads the sources" >&2
            return 1
        fi
    fi
    echo "$sha  $archive" | sha256sum -c --quiet - || { echo "$archive does not match its sha256" >&2; return 1; }
    rm -rf "$dir/raw" && mkdir -p "$dir/raw"
    tar -xzf "$archive" -C "$dir/raw"
    echo "$sha" > "$dir/raw/.snapshot"
}

now_ms() { echo $(($(date +%s%N) / 1000000)); }

# Where the A/B checks keep the worktree of the base commit and what they build with it.
AB_DIR="${GEKO_AB_DIR:-/var/lib/geko/ab}"

# The commit that the A/B checks compare the working tree with: GEKO_BASE, or else the last
# pushed one, where HEAD leaves its upstream.
base_commit() {
    if [ -n "${GEKO_BASE:-}" ] && git cat-file -e "$GEKO_BASE^{commit}" 2> /dev/null; then
        git rev-parse "$GEKO_BASE"
    else
        git merge-base HEAD '@{upstream}' 2> /dev/null || git merge-base HEAD origin/main
    fi
}

# The binaries of `commit`, built in a worktree of it ($AB_DIR/base) with their own target
# directory, both kept between runs so that only what changed is built again. Prints that target
# directory.
base_binaries() {
    local commit=$1 tree="$AB_DIR/base" target
    target="$(realpath -m "${CARGO_TARGET_DIR:-target}")-base"
    if [ -e "$tree/.git" ]; then
        git -C "$tree" checkout --quiet --detach --force "$commit" >&2
    else
        mkdir -p "$AB_DIR"
        git worktree add --detach "$tree" "$commit" >&2
    fi
    (cd "$tree" && CARGO_TARGET_DIR="$target" cargo build --release --locked -p geko-app >&2 \
        && CARGO_TARGET_DIR="$target" cargo build --profile ci --locked -p geko-bench >&2)
    echo "$target"
}

# geko with the configuration file `config`, whose data live in the directory that holds it.
run_geko() { local bin=$1 config=$2; shift 2; GEKO_CONFIG="$config" GEKO_DATA="$(dirname "$config")" "$bin/geko" "$@"; }

# Starts `geko serve` with `config` in the background, as $server, and waits until it has loaded
# a bundle and answers; $opened_ms is how long that took. Any further arguments are a command
# that runs it, as `taskset -c 0-3`, which must exec it so that $server is geko.
serve() {
    local bin=$1 config=$2 log=$3 port=$4 started
    shift 4
    started=$(now_ms)
    # Not through run_geko: a function in the background runs in a subshell, and killing it would
    # leave geko serving on the port.
    GEKO_CONFIG="$config" GEKO_DATA="$(dirname "$config")" "$@" "$bin/geko" serve > "$log" 2>&1 &
    server=$!
    for _ in $(seq 600); do
        kill -0 "$server" || { cat "$log"; return 1; }
        if curl -sf -m 2 "http://127.0.0.1:$port/health" | grep -q '"bundle":"'; then
            opened_ms=$(($(now_ms) - started))
            return 0
        fi
        sleep 0.5
    done
    echo "the server did not load a bundle in 5 minutes" >&2
    return 1
}

# Checks `report` against the hit rates of `baseline` and, when a report of this machine is
# given, against its latencies.
check_report() {
    local bin=$1 report=$2 baseline=$3 latency=$4 tolerance=$5
    if [ -n "$latency" ] && [ -f "$latency" ]; then
        "$bin/geko-eval" check --baseline "$baseline" --report "$report" --tolerance "$tolerance" --latency-baseline "$latency"
    else
        "$bin/geko-eval" check --baseline "$baseline" --report "$report" --tolerance "$tolerance" --ignore-latency
    fi
}

# Checks the data report of the canonical dataset of `dir` against `baseline`, or makes it the
# baseline.
check_data() {
    local bin=$1 dir=$2 country=$3 baseline=$4 update=$5 reports=$6
    "$bin/geko-eval" data --canonical "$dir/canonical/${country^^}" --out "$reports/data.json"
    if [ "$update" = 1 ]; then
        mkdir -p "$(dirname "$baseline")" && cp "$reports/data.json" "$baseline"
    else
        "$bin/geko-eval" check-data --baseline "$baseline" --report "$reports/data.json"
    fi
}

# Switches the server to a new version of the bundle (the same files, linked under another name)
# while 16 autocomplete requests at a time keep it busy: no request may fail, and the server
# must end up serving the new version.
reload_under_load() {
    local bin=$1 dir=$2 port=$3 cases=$4
    local bundles="$dir/bundles" current copy failed=0
    current=$(readlink "$bundles/current")
    copy="$current-reload"
    rm -rf "${bundles:?}/$copy" && cp -al "$bundles/$current" "$bundles/$copy"
    "$bin/geko-eval" load --url "http://127.0.0.1:$port" --cases "$cases" --route autocomplete --concurrency 16 \
        --seconds 8 --no-errors --out "$dir/reports/reload.json" &
    local load=$!
    sleep 5
    ln -sfn "$copy" "$bundles/current"
    wait "$load" || failed=1
    curl -sf "http://127.0.0.1:$port/health" | grep -q "\"bundle\":\"$copy\"" || { echo "the server did not load the new bundle" >&2; failed=1; }
    ln -sfn "$current" "$bundles/current"
    rm -rf "${bundles:?}/$copy"
    return $failed
}

# Digest of what a build writes, without its stamps and the timings of its reports.
build_digest() {
    (cd "$1" && find canonical segments -type f ! -name '*.stamp.json' ! -name 'fusion_report.json' -print0 \
        | sort -z | xargs -0 sha256sum)
}

# A second build of the same inputs must reuse every stage, and a forced one must write the same
# bytes (PROJECT.md §8.3): the incremental cut depends on both.
determinism() {
    local bin=$1 dir=$2 failed=0 out
    build_digest "$dir" > "$dir/reports/digest-before.txt"
    out=$(run_geko "$bin" "$dir/geko.toml" build)
    local rerun
    rerun=$(echo "$out" | grep -E '^(staged|relocated|indexed|reverse) ' | grep -v 'up to date' || true)
    echo "$out" | grep -E '^fused ' | grep -q 'merge up to date' || rerun="$rerun"$'\n'"$(echo "$out" | grep -E '^fused ')"
    echo "$out" | grep -qE '^bundle .* is up to date' || rerun="$rerun"$'\n'"a new bundle was published"
    if [ -n "${rerun//$'\n'/}" ]; then
        echo "a second build with nothing changed did not reuse:$rerun" >&2
        failed=1
    fi
    run_geko "$bin" "$dir/geko.toml" build --force > /dev/null
    build_digest "$dir" > "$dir/reports/digest-after.txt"
    if ! diff -q "$dir/reports/digest-before.txt" "$dir/reports/digest-after.txt" > /dev/null; then
        echo "a forced build of the same inputs wrote different files:" >&2
        diff "$dir/reports/digest-before.txt" "$dir/reports/digest-after.txt" | grep '^[<>]' | head -20 >&2
        failed=1
    fi
    return $failed
}

# Builds and serves the golden regions (see the top of this file): sets $bin, $dir, $port, $golden
# and $baseline for the caller, and $server, which the caller must stop.
golden_serve() {
    local country regions scope_regions="" offline=false
    dir=$(golden_dir) country=$(golden_country) regions=$(golden_regions) port="${GEKO_GOLDEN_PORT:-18080}"
    golden="countries/$country/golden" baseline="countries/$country/golden/baseline"
    [ -z "$regions" ] || scope_regions="regions = [$(echo "$regions" | sed 's/[^,][^,]*/"&"/g; s/,/, /g')]"
    mkdir -p "$dir/reports"
    if [ "${GEKO_GOLDEN_FRESH:-0}" = 1 ] || [ ! -f "$golden/snapshot" ]; then
        rm -f "$dir/raw/.snapshot"
    else
        snapshot_age "$golden/snapshot" "$country"
        restore_snapshot "$golden/snapshot" "$dir" "$country"
        offline=true
    fi
    local profile="${GEKO_GOLDEN_PROFILE:-release}"
    cargo build --profile "$profile" --locked -p geko-app -p geko-eval
    bin="${CARGO_TARGET_DIR:-target}/$profile"
    cat > "$dir/geko.toml" <<EOF
[modules]
geocoder = true
tiles    = false

[[scope]]
country = "${country^^}"
$scope_regions

[data]
accept_licenses = true
offline         = $offline
keep_downloads  = true

[serve]
listen      = "127.0.0.1:$port"
reload_secs = 1
# Without the time budget, which cuts wherever the machine is slowest at the moment: the hits,
# every keystroke too, must be the same on any machine. Spain checks the budget under load.
search_budget_ms       = 0
autocomplete_budget_ms = 0
EOF
    for source in ${GEKO_GOLDEN_DISABLE-overture}; do
        printf '\n[sources.%s]\nenabled = false\n' "$source" >> "$dir/geko.toml"
    done
    run_geko "$bin" "$dir/geko.toml" build
    serve "$bin" "$dir/geko.toml" "$dir/serve.log" "$port"
}

# The Pelias and Photon modes must find what the native API finds: they only translate names
# and shapes. Photon searches as the text is typed, so it is compared with native autocomplete.
compatible_modes() {
    local bin="$1" dir="$2" port="$3" golden="$4" failed=0 set
    local out="$dir/reports/compatible" url="http://127.0.0.1:$port"
    mkdir -p "$out"
    for set in generated reverse; do
        [ -f "$golden/$set.jsonl" ] || continue
        sed 's/"autocomplete":false/"autocomplete":true/' "$golden/$set.jsonl" > "$out/$set-typed.jsonl"
        "$bin/geko-eval" run --name native --url "$url" --cases "$golden/$set.jsonl" --out "$out/$set-native.json" > /dev/null
        "$bin/geko-eval" run --name native-typed --url "$url" --cases "$out/$set-typed.jsonl" --out "$out/$set-native-typed.json" > /dev/null
        "$bin/geko-eval" run --name pelias --url "$url/pelias" --cases "$golden/$set.jsonl" --out "$out/$set-pelias.json" > /dev/null
        "$bin/geko-eval" run --name photon --api photon --url "$url/photon" --cases "$golden/$set.jsonl" --out "$out/$set-photon.json" > /dev/null
        echo "compatible modes, $set:"
        "$bin/geko-eval" check --baseline "$out/$set-native.json" --report "$out/$set-pelias.json" --ignore-latency --hits-only | grep -E "LOST|lost and|^Error" || true
        "$bin/geko-eval" check --baseline "$out/$set-native.json" --report "$out/$set-pelias.json" --ignore-latency --hits-only > /dev/null || failed=1
        "$bin/geko-eval" check --baseline "$out/$set-native-typed.json" --report "$out/$set-photon.json" --ignore-latency --hits-only | grep -E "LOST|lost and|^Error" || true
        "$bin/geko-eval" check --baseline "$out/$set-native-typed.json" --report "$out/$set-photon.json" --ignore-latency --hits-only > /dev/null || failed=1
    done
    return $failed
}

golden() {
    local bin dir port golden baseline server opened_ms cases
    local tolerance="${GEKO_GOLDEN_TOLERANCE:-0}" update="${GEKO_GOLDEN_UPDATE:-0}" failed=0
    golden_serve
    trap 'kill $server 2>/dev/null || true' RETURN
    # Latencies are only comparable on the machine that measured them: those of this machine
    # are kept beside its data, not in the repository.
    local latencies="$dir/latency-baseline" set latency=""
    # Fresh data are not those of the baseline: their counts differ by design. Unless they are
    # becoming it (renewing the snapshot): then their counts and sizes are the new baseline.
    if wants data && { [ "${GEKO_GOLDEN_FRESH:-0}" != 1 ] || [ "$update" = 1 ]; }; then
        check_data "$bin" "$dir" "$(golden_country)" "$baseline/data.json" "$update" "$dir/reports" || failed=1
        # The sizes of the bundle: the same on any machine, unlike the time and memory of the build.
        cp "$dir/build_report.json" "$dir/reports/build.json"
        if [ "$update" = 1 ]; then
            cp "$dir/reports/build.json" "$baseline/build.json"
        else
            "$bin/geko-eval" check-build --baseline "$baseline/build.json" --report "$dir/reports/build.json" \
                --ignore-time --ignore-memory || failed=1
        fi
    fi
    # The hit rates, and with --include-performance also the latencies of the same requests.
    if wants results; then
        # Every keystroke of the generated queries, as an app sends them while they are typed.
        "$bin/geko-eval" keystrokes --cases "$golden/generated.jsonl" --out "$dir/reports/keystrokes.jsonl" > /dev/null
        for set in ${GEKO_GOLDEN_SETS:-$(grep -v "^#" "$golden/sets") keystrokes}; do
            cases="$golden/$set.jsonl"
            [ "$set" = keystrokes ] && cases="$dir/reports/keystrokes.jsonl"
            "$bin/geko-eval" run --name "$set" --url "http://127.0.0.1:$port" --cases "$cases" --out "$dir/reports/$set.json" > /dev/null
            if [ "$update" = 1 ]; then
                mkdir -p "$baseline" && cp "$dir/reports/$set.json" "$baseline/$set.json"
            else
                # Latencies are only comparable on the machine that measured them: those of this
                # machine are kept beside its data, not in the repository.
                latency=""
                [ "$PERFORMANCE" = 1 ] && latency="$latencies/$set.json"
                check_report "$bin" "$dir/reports/$set.json" "$baseline/$set.json" "$latency" "$tolerance" || failed=1
            fi
            if [ "$update" = 1 ] || [ "${GEKO_LATENCY_UPDATE:-0}" = 1 ]; then
                mkdir -p "$latencies" && cp "$dir/reports/$set.json" "$latencies/$set.json"
            elif [ -n "$latency" ] && [ ! -f "$latency" ]; then
                echo "no latencies of this machine to compare with: GEKO_LATENCY_UPDATE=1 records them" >&2
            fi
        done
        compatible_modes "$bin" "$dir" "$port" "$golden" || failed=1
    fi
    if wants robustness; then
        "$bin/geko-eval" fuzz --url "http://127.0.0.1:$port" --cases "$golden/generated.jsonl" \
            --requests "${GEKO_FUZZ_REQUESTS:-5000}" --seed "${GEKO_FUZZ_SEED:-1}" --out "$dir/reports/fuzz.json" || failed=1
        reload_under_load "$bin" "$dir" "$port" "$golden/generated.jsonl" || failed=1
    fi
    kill "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
    if wants data && [ "${GEKO_GOLDEN_DETERMINISM:-1}" = 1 ]; then
        determinism "$bin" "$dir" || failed=1
    fi
    if wants instructions && [ "${GEKO_GOLDEN_FRESH:-0}" != 1 ]; then
        bench "$dir" "$golden" || failed=1
    fi
    return $failed
}

# geko against an instance of Pelias (PROJECT.md §13.2) on the golden regions, with
# the queries of entities that Pelias has too: geko must not be worse in any category, and must
# lead by 10 points in all (PROJECT.md §1.1). Pelias is a shared service: its answers are asked
# with a pause between requests and kept for GEKO_PELIAS_MAX_AGE_DAYS (default 30), since its
# data change little. Environment: GEKO_PELIAS_URL, the address of the instance of Pelias
# (required), with the OSM data of the golden regions.
pelias() {
    local bin dir port golden baseline server opened_ms failed=0 set
    local url="${GEKO_PELIAS_URL:?--scope pelias needs GEKO_PELIAS_URL, the address of an instance of Pelias with the same OSM data}"
    golden_serve
    trap 'kill $server 2>/dev/null || true' RETURN
    mkdir -p "$dir/pelias"
    for set in generated-osm holdout-osm; do
        local cached="$dir/pelias/$set.json"
        if [ -z "$(find "$cached" -mtime -"${GEKO_PELIAS_MAX_AGE_DAYS:-30}" 2>/dev/null)" ]; then
            "$bin/geko-eval" run --name pelias --url "$url" --cases "$golden/$set.jsonl" --delay-ms 200 --out "$cached.part" > /dev/null
            mv "$cached.part" "$cached"
        fi
        "$bin/geko-eval" run --name geko --url "http://127.0.0.1:$port" --cases "$golden/$set.jsonl" --out "$dir/reports/$set.json" > /dev/null
        "$bin/geko-eval" check --baseline "$cached" --report "$dir/reports/$set.json" --ignore-latency --hits-only --total-lead 10 || failed=1
    done
    return $failed
}

# Identity of the downloads under `dir/raw`: names, sizes and times of the data files, not of
# the bookkeeping that a check for updates rewrites.
data_id() {
    find "$1/raw" -type f ! -name '*.checked' ! -name '*.raw.json' ! -name '*.md5' ! -name '.snapshot' -printf '%P %s %T@\n' \
        | sort | sha256sum | cut -d' ' -f1
}

# The value `name` of `now` (lines `name value`) clearly above that of `baseline`: by `factor`
# and by `margin`.
compare_values() {
    local what=$1 baseline=$2 now=$3 name=$4 factor=$5 margin=$6 b n
    b=$(awk -v k="$name" '$1 == k { print $2 }' "$baseline") n=$(awk -v k="$name" '$1 == k { print $2 }' "$now")
    [ -n "$b" ] && [ -n "$n" ] || return 0
    if awk -v b="$b" -v n="$n" -v f="$factor" -v m="$margin" 'BEGIN { exit !(n > b * f && n > b + m) }'; then
        echo "WORSE $what $name: $b -> $n" >&2
        return 1
    fi
}

# Instructions of the hot paths of the engine (geko-bench) over the reference queries of the
# golden bundle, counted by callgrind: unlike time, they barely change between runs and machines,
# so they are compared on CI too. A count is worse past BENCH_FACTOR: HashMap seeds move it by a
# few hundredths of a percent, and other CPUs pick other SIMD paths in a few libraries.
BENCH_FACTOR=1.03
BENCH_PATHS="parse search autocomplete reverse"

# Sets $valgrind: VALGRIND, the one on the PATH, or the one extracted without root where it is
# not installed (AGENTS.md, «Environment»). Without it the instruction counts are an error, unless
# GEKO_BENCH=0 skips them: skipping them in silence hid that they did not run.
find_valgrind() {
    local extracted="$HOME/.local/valgrind-deb/root/usr"
    valgrind="${VALGRIND:-}"
    if [ -z "$valgrind" ] && command -v valgrind > /dev/null; then
        valgrind=valgrind
    elif [ -z "$valgrind" ] && [ -x "$extracted/bin/valgrind" ]; then
        valgrind="$extracted/bin/valgrind"
        export VALGRIND_LIB="${VALGRIND_LIB:-$extracted/libexec/valgrind}"
    fi
    [ -n "$valgrind" ] && return 0
    echo "valgrind is needed for the instruction counts (GEKO_BENCH=0 skips them)" >&2
    return 1
}

# The instructions of each hot path of `bench_bin` (geko-bench) on `bundle`, over `cases` and the
# reverse queries `reverse_cases`, one line `path count` each in `out`.
instructions() {
    local bench_bin=$1 bundle=$2 cases=$3 reverse_cases=$4 out=$5 b n
    : > "$out"
    for b in $BENCH_PATHS; do
        n=$("$valgrind" --tool=callgrind --callgrind-out-file=/dev/null --toggle-collect="*bench_$b*" \
            "$bench_bin" "$bundle" "$([ "$b" = reverse ] && echo "$reverse_cases" || echo "$cases")" "$b" 2>&1 \
            | sed -n 's/.*Collected : //p')
        [ -n "$n" ] || { echo "callgrind counted nothing for $b" >&2; return 1; }
        echo "$b $n" >> "$out"
    done
}

# The paths of `now` with clearly more instructions than in `baseline`.
compare_instructions() {
    local baseline=$1 now=$2 b failed=0
    paste "$baseline" "$now" | awk '{ printf "%-14s %15s %15s\n", $1, $2, $4 }'
    for b in $BENCH_PATHS; do
        compare_values instructions "$baseline" "$now" "$b" "$BENCH_FACTOR" 0 || failed=1
    done
    return $failed
}

# The instructions of the golden bundle, with the binaries of the working tree and of the base
# commit, in the same run: a count changes from one CPU to another (the SIMD paths of a few
# libraries), so it is only compared with one of the same machine, and a baseline kept in the
# repository would be that of another machine. A base that cannot read the bundle (its format
# changed) counts on its own build of the same frozen data.
bench() {
    local dir=$1 golden=$2 valgrind
    [ "${GEKO_BENCH:-1}" = 0 ] && { echo "instruction counts skipped (GEKO_BENCH=0)" >&2; return 0; }
    find_valgrind || return 1
    cargo build --profile ci --locked -p geko-bench
    local cases="$golden/generated.jsonl" reverse="$golden/reverse.jsonl" base target bundle
    instructions "${CARGO_TARGET_DIR:-target}/ci/geko-bench" "$dir/bundles/current" "$cases" "$reverse" "$dir/reports/bench.txt" || return 1
    base=$(base_commit)
    target=$(base_binaries "$base")
    bundle="$dir/bundles/current"
    if ! instructions "$target/ci/geko-bench" "$bundle" "$cases" "$reverse" "$dir/reports/bench-base.txt" 2> /dev/null; then
        echo "the base cannot read this bundle: it builds its own from the same data"
        bundle=$(golden_base_bundle "$target/release" "$dir") || return 1
        instructions "$target/ci/geko-bench" "$bundle" "$cases" "$reverse" "$dir/reports/bench-base.txt" || return 1
    fi
    echo "instructions, base $(git rev-parse --short "$base") and new:"
    compare_instructions "$dir/reports/bench-base.txt" "$dir/reports/bench.txt"
}

# The bundle that the base binaries in `base_bin` build from the downloads of the golden in `dir`
# (linked, not copied), in $AB_DIR/golden-<country>, kept between runs. Prints its path.
golden_base_bundle() {
    local base_bin=$1 dir=$2 data
    data="$AB_DIR/golden-$(golden_country)"
    mkdir -p "$data"
    rm -rf "$data/raw" && cp -al "$dir/raw" "$data/raw"
    cp "$dir/geko.toml" "$data/geko.toml"
    run_geko "$base_bin" "$data/geko.toml" build >&2 || return 1
    echo "$data/bundles/current"
}

# Whole of Spain (`make test-regression`), where some failures only show at the scale of a
# country: the reference queries, every keystroke of them and the reverse queries; the
# instructions of the hot paths; throughput and p99 under load; the hit rate while the server is
# loaded; the memory of the build, the data it produced, the size of the bundle, and the start
# and the memory of the server. Local and on demand: it needs the data of the whole country (60
# min of downloads, 30 of build, 5.6 GB of peak memory), and CI checks every push with the
# golden of each country.
#
# It compares the working tree with a base commit, by default the last pushed one, whose
# binaries it builds in a worktree ($AB_DIR/base, with their own target directory). What does not
# depend on how busy the machine is (hits, data, sizes, memory, instructions) is compared with
# the baseline of the base, measured once for each base commit and data in $dir/check-baseline.
# What does (latency, throughput, hits under load), only with --include-performance, is measured on
# both servers at once, in turns, in the same run (AGENTS.md: measure A/B in the same run). The
# variants choose the rest: results (the query sets and keystrokes), data (the canonical dataset,
# the size of the bundle and the memory of the build) and instructions. Environment:
#   GEKO_SPAIN_DIR         data directory (default /var/lib/geko)
#   GEKO_SPAIN_PORT        port of the server (default 18081); the base serves on the next one
#   GEKO_SPAIN_DISABLE     sources left out of the build, space separated (e.g. "overture"): to
#                          measure a source against the same data without it
#   GEKO_SPAIN_FRESH=1     download what changed before building; the baseline of the base is
#                          then measured again on the new data
#   GEKO_SPAIN_FORCE=1     every build stage runs, base and new
#   GEKO_SPAIN_BUILD_AB=1  also compare the time of the build: base, new, base, new
#   GEKO_SPAIN_LOAD_SECONDS  seconds each server is measured at each route and concurrency
#                          (default 10), in five slices taken in turns with the other server
#   GEKO_BENCH=0           skip the instruction counts (they need valgrind)
#   GEKO_SPAIN_LOADED_TOLERANCE  allowed drop of the hit rate under load, in percentage points
#                          (default 1): under load, the time budget cuts wherever the machine is
#                          slowest, which varies between runs
#   GEKO_REFERENCE_MACHINE=1  also check the latency and throughput objectives of PROJECT.md
#                          §1.1, only meaningful on the reference hardware
spain() {
    local dir="${GEKO_SPAIN_DIR:-/var/lib/geko}" port="${GEKO_SPAIN_PORT:-18081}" base_port
    local golden=countries/es/golden baseline reports offline=true
    base_port=$((port + 1)) baseline="$dir/check-baseline" reports="$dir/check-reports"
    [ "${GEKO_SPAIN_FRESH:-0}" = 1 ] && offline=false
    mkdir -p "$reports"
    local base valgrind=""
    base=$(base_commit)
    if wants instructions && [ "${GEKO_BENCH:-1}" != 0 ]; then
        find_valgrind || return 1
    fi
    cargo build --release --locked -p geko-app -p geko-eval
    cargo build --profile ci --locked -p geko-bench
    local bin base_target base_bin
    bin=$(realpath "${CARGO_TARGET_DIR:-target}/release")
    base_target=$(base_binaries "$base")
    base_bin="$base_target/release"
    spain_config "$dir/check-spain.toml" "$dir" "$port" "$offline"
    # On 8 cores or more, the servers run on the first 4, as the reference hardware (PROJECT.md
    # §1.1), and the client on the others.
    local pin_server="" pin_client=""
    if [ "$(nproc)" -ge 8 ]; then
        pin_server="taskset -c 0-3" pin_client="taskset -c 4-$(($(nproc) - 1))"
    fi
    [ "${GEKO_SPAIN_FRESH:-0}" = 1 ] && run_geko "$bin" "$dir/check-spain.toml" fetch
    local data failed=0 server opened_ms
    data=$(data_id "$dir")
    trap 'kill $server ${base_server:-} 2>/dev/null || true' RETURN
    if [ "$(cat "$baseline/commit" 2>/dev/null)" != "$base" ] || [ "$(cat "$baseline/data-id" 2>/dev/null)" != "$data" ] \
        || [ ! -e "$dir/ab/base-data/bundles/current" ]; then
        echo "measuring the baseline of $base"
        spain_baseline "$bin" "$base_bin" "$dir" "$base_port" "$offline" "$baseline" "$reports" || return 1
        echo "$base" > "$baseline/commit" && echo "$data" > "$baseline/data-id"
    fi
    # Instructions are the same on every run: those of the base are counted once, on its bundle.
    if [ -n "$valgrind" ] && [ ! -s "$baseline/bench.txt" ]; then
        instructions "$base_target/ci/geko-bench" "$dir/ab/base-data/bundles/current" "$golden/spain-generated.jsonl" \
            "$golden/spain-reverse.jsonl" "$baseline/bench.txt" || return 1
    fi

    run_geko "$bin" "$dir/check-spain.toml" build ${GEKO_SPAIN_FORCE:+--force}
    cp "$dir/build_report.json" "$reports/build.json"
    if wants data; then
        "$bin/geko-eval" check-build --baseline "$baseline/build.json" --report "$reports/build.json" --max-hot-gb 4 --ignore-time || failed=1
        check_data "$bin" "$dir" es "$baseline/data.json" 0 "$reports" || failed=1
    fi
    if wants results; then
        spain_hits "$bin" "$dir/check-spain.toml" "$dir" "$port" "$reports" "$reports" || failed=1
        local set
        for set in spain-generated spain-holdout spain-focus spain-reverse keystrokes; do
            check_report "$bin" "$reports/$set.json" "$baseline/$set.json" "" 0 || failed=1
        done
    fi
    if [ -n "$valgrind" ]; then
        echo "instructions, base and new:"
        { instructions "$(realpath "${CARGO_TARGET_DIR:-target}/ci/geko-bench")" "$dir/bundles/current" "$golden/spain-generated.jsonl" \
            "$golden/spain-reverse.jsonl" "$reports/bench.txt" && compare_instructions "$baseline/bench.txt" "$reports/bench.txt"; } || failed=1
    fi

    [ "$PERFORMANCE" = 1 ] || return $failed

    # Both servers at once, with the configuration of production.
    "$bin/geko-eval" keystrokes --cases "$golden/spain-generated.jsonl" --cases "$golden/spain-holdout.jsonl" \
        --out "$reports/keystrokes.jsonl" > /dev/null
    local url="http://127.0.0.1:$port" base_url="http://127.0.0.1:$base_port" base_server base_opened
    # shellcheck disable=SC2086
    serve "$base_bin" "$dir/ab/base-data/geko.toml" "$dir/ab/base-data/serve.log" "$base_port" $pin_server
    base_server=$server base_opened=$opened_ms
    # shellcheck disable=SC2086
    serve "$bin" "$dir/check-spain.toml" "$dir/check-serve.log" "$port" $pin_server
    local seconds="${GEKO_SPAIN_LOAD_SECONDS:-10}" route cases objectives
    for route in search autocomplete reverse; do
        case $route in
            search) cases="$golden/spain-generated.jsonl" objectives="--max-p99-ms 15" ;;
            autocomplete) cases="$reports/keystrokes.jsonl" objectives="--max-p99-ms 5 --min-qps 2000" ;;
            reverse) cases="$golden/spain-reverse.jsonl" objectives="--max-p99-ms 5" ;;
        esac
        [ "${GEKO_REFERENCE_MACHINE:-0}" = 1 ] || objectives=""
        # shellcheck disable=SC2086
        $pin_client "$bin/geko-eval" load-ab --base-url "$base_url" --url "$url" --cases "$cases" --route "$route" \
            --concurrency 1,4,16 --seconds "$seconds" --out-base "$reports/load-$route-base.json" --out "$reports/load-$route.json" > /dev/null
        # shellcheck disable=SC2086
        "$bin/geko-eval" check-load --baseline "$reports/load-$route-base.json" --report "$reports/load-$route.json" $objectives || failed=1
    done
    rss() { awk '/^VmRSS:/ { print int($2 / 1024) }' "/proc/$1/status"; }
    printf 'open_ms %s\nrss_mb %s\n' "$base_opened" "$(rss "$base_server")" > "$reports/server-base.txt"
    printf 'open_ms %s\nrss_mb %s\n' "$opened_ms" "$(rss "$server")" > "$reports/server.txt"
    # The reference queries again on each server, while 16 autocomplete requests at a time keep it
    # busy.
    local side side_url
    for side in base new; do
        side_url=$url
        [ "$side" = base ] && side_url=$base_url
        # shellcheck disable=SC2086
        $pin_client "$bin/geko-eval" load --url "$side_url" --cases "$reports/keystrokes.jsonl" --route autocomplete \
            --concurrency 16 --seconds 100000 > /dev/null &
        local load=$!
        # shellcheck disable=SC2086
        $pin_client "$bin/geko-eval" run --name "loaded-$side" --url "$side_url" --cases "$golden/spain-generated.jsonl" \
            --out "$reports/loaded-$side.json" > /dev/null
        kill "$load" 2> /dev/null || true
        wait "$load" 2> /dev/null || true
    done
    kill "$server" "$base_server" 2> /dev/null || true
    wait "$server" "$base_server" 2> /dev/null || true
    "$bin/geko-eval" check --baseline "$reports/loaded-base.json" --report "$reports/loaded-new.json" \
        --tolerance "${GEKO_SPAIN_LOADED_TOLERANCE:-1}" --ignore-latency || failed=1
    paste "$reports/server-base.txt" "$reports/server.txt" | awk '{ printf "%-8s %10s %10s\n", $1, $2, $4 }'
    compare_values server "$reports/server-base.txt" "$reports/server.txt" open_ms 1.5 2000 || failed=1
    compare_values server "$reports/server-base.txt" "$reports/server.txt" rss_mb 1.2 200 || failed=1

    if [ "${GEKO_SPAIN_BUILD_AB:-0}" = 1 ]; then
        spain_build_ab "$bin" "$base_bin" "$dir" "$reports" || failed=1
    fi
    return $failed
}


# The configuration of the check, serving on `port`, and beside it the same without the time
# budget (`-exact`): the hit rates are measured without it, because it cuts wherever the machine
# is slowest at the moment, and a few keystrokes would change from one run to the next. The
# budget is checked where it matters, under load, with the configuration of production.
spain_config() {
    local file=$1 dir=$2 port=$3 offline=$4 source
    mkdir -p "$dir"
    cat > "$file" <<EOF
[modules]
geocoder = true
tiles    = false

[[scope]]
country = "ES"

[data]
accept_licenses = true
offline         = $offline
keep_downloads  = true

[serve]
listen = "127.0.0.1:$port"
EOF
    for source in ${GEKO_SPAIN_DISABLE:-}; do
        printf '\n[sources.%s]\nenabled = false\n' "$source" >> "$file"
    done
    sed 's/^listen = .*/&\nsearch_budget_ms       = 0\nautocomplete_budget_ms = 0/' "$file" > "${file%.toml}-exact.toml"
}

# The hit rates of the server of `config` (served without the time budget) on the reference
# queries of Spain and every keystroke of them, written to `out`; the keystrokes are written to
# `reports` the first time.
spain_hits() {
    local bin=$1 config=$2 dir=$3 port=$4 reports=$5 out=$6 golden=countries/es/golden set cases
    local server opened_ms url="http://127.0.0.1:$4"
    serve "$bin" "${config%.toml}-exact.toml" "$dir/check-serve.log" "$port"
    local cold
    cold=$(curl -s -o /dev/null -w '%{time_total}' -G "$url/v1/search" --data-urlencode "text=Calle Mayor 23, Madrid")
    echo "first query: $(awk -v s="$cold" 'BEGIN { printf "%.1f", s * 1000 }') ms (not compared: it reads the disk)"
    # The cold part of the bundle is mapped: after a restart it is read from disk, and the
    # latencies would measure the disk instead of the engine. Read it all once.
    find -L "$(dirname "$config")/bundles/current/" -type f -exec cat {} + > /dev/null
    "$bin/geko-eval" keystrokes --cases "$golden/spain-generated.jsonl" --cases "$golden/spain-holdout.jsonl" \
        --out "$reports/keystrokes.jsonl" > /dev/null
    for set in spain-generated spain-holdout spain-focus spain-reverse keystrokes; do
        cases="$golden/$set.jsonl"
        [ "$set" = keystrokes ] && cases="$reports/keystrokes.jsonl"
        "$bin/geko-eval" run --name "$set" --url "$url" --cases "$cases" --out "$out/$set.json" > /dev/null
    done
    kill "$server" 2> /dev/null || true
    wait "$server" 2> /dev/null || true
}

# The baseline of the base commit: its build of the data of `dir`, its data report and hit
# rates, and a copy of its bundle ($dir/ab/base-data) for the load measured beside the new one.
spain_baseline() {
    local bin=$1 base_bin=$2 dir=$3 port=$4 offline=$5 baseline=$6 reports=$7 data="$3/ab/base-data" current
    run_geko "$base_bin" "$dir/check-spain.toml" build ${GEKO_SPAIN_FORCE:+--force}
    rm -rf "$baseline" && mkdir -p "$baseline"
    cp "$dir/build_report.json" "$baseline/build.json"
    "$bin/geko-eval" data --canonical "$dir/canonical/ES" --out "$baseline/data.json"
    # The new build replaces the bundle: the base keeps its own, linked, not copied.
    current=$(readlink "$dir/bundles/current")
    rm -rf "$data" && mkdir -p "$data/bundles"
    cp -al "$dir/bundles/$current" "$data/bundles/$current"
    ln -s "$current" "$data/bundles/current"
    spain_config "$data/geko.toml" "$data" "$port" true
    spain_hits "$base_bin" "$data/geko.toml" "$data" "$port" "$reports" "$baseline"
}

# The time of the build, base and new in turns twice, each run rebuilding what the other
# changed: the faster run of each side is compared. Last the new one, whose data stay.
spain_build_ab() {
    local bin=$1 base_bin=$2 dir=$3 reports=$4 side i=0
    for side in base new base new; do
        i=$((i + 1))
        if [ "$side" = base ]; then run_geko "$base_bin" "$dir/check-spain.toml" build; else run_geko "$bin" "$dir/check-spain.toml" build; fi
        cp "$dir/build_report.json" "$reports/build-ab-$i-$side.json"
    done
    python3 - "$reports" <<'EOF'
import glob, json, sys
reports = sys.argv[1]
for side in ("base", "new"):
    runs = [json.load(open(f)) for f in sorted(glob.glob(f"{reports}/build-ab-*-{side}.json"))]
    best = runs[0]
    for stage in best["stages"]:
        times = [s["elapsed_ms"] for r in runs for s in r["stages"] if s["stage"] == stage["stage"] and not s["reused"]]
        if times:
            stage["elapsed_ms"], stage["reused"] = min(times), False
    json.dump(best, open(f"{reports}/build-ab-{side}.json", "w"), indent=2)
EOF
    echo "time of the build, the faster run of each side:"
    "$bin/geko-eval" check-build --baseline "$reports/build-ab-base.json" --report "$reports/build-ab-new.json"
}

ALL_VARIANTS=code,contracts,results,data,robustness,instructions
VARIANTS=$ALL_VARIANTS SCOPES=es,lu PERFORMANCE=0

usage() { echo "usage: $0 [--variant $ALL_VARIANTS] [--scope es,lu,…,spain,pelias] [--include-performance]" >&2; }

# Whether the variant `$1` was asked for.
wants() { [[ ",$VARIANTS," == *",$1,"* ]]; }

while [ $# -gt 0 ]; do
    case $1 in
        --variant) VARIANTS=${2:?--variant takes a list}; shift 2 ;;
        --variant=*) VARIANTS=${1#*=}; shift ;;
        --scope) SCOPES=${2:?--scope takes a list}; shift 2 ;;
        --scope=*) SCOPES=${1#*=}; shift ;;
        --include-performance) PERFORMANCE=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) usage; exit 2 ;;
    esac
done
for variant in ${VARIANTS//,/ }; do
    [[ ",$ALL_VARIANTS," == *",$variant,"* ]] || { echo "unknown variant $variant" >&2; usage; exit 2; }
done
for scope in ${SCOPES//,/ }; do
    case $scope in
        spain | pelias) ;;
        *) [ -d "countries/$scope/golden" ] || { echo "unknown scope $scope: no countries/$scope/golden" >&2; usage; exit 2; } ;;
    esac
done

# Runs a check in a subshell, where errexit stops it at its first failure as when it ran alone,
# and goes on with the next one: the summary names the checks that failed. Whatever the check
# left in the background (a server, a load) ends with it.
FAILED=()
run() {
    local name=$1 status
    shift
    echo "==> $name"
    set +e
    (
        set -e
        trap 'kill $(jobs -p) 2> /dev/null || true' EXIT
        "$@"
    )
    status=$?
    set -e
    [ "$status" = 0 ] || FAILED+=("$name")
}

if wants code; then
    run lint lint
    run architecture architecture
    run deny deny
    run features features
fi
wants contracts && run test test
for scope in ${SCOPES//,/ }; do
    case $scope in
        spain) wants results || wants data || wants instructions || [ "$PERFORMANCE" = 1 ] && run spain spain ;;
        pelias) wants results && GEKO_GOLDEN_COUNTRY=es run pelias pelias ;;
        *) wants results || wants data || wants robustness || wants instructions \
            && GEKO_GOLDEN_COUNTRY=$scope run "golden $scope" golden ;;
    esac
done
if [ ${#FAILED[@]} -gt 0 ]; then
    echo "failed: ${FAILED[*]}" >&2
    exit 1
fi
echo "passed"
