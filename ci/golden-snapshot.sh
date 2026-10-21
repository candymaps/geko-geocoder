#!/usr/bin/env bash
# The data of the golden of each country pack: the downloads of its regions, frozen in an archive
# that countries/<country>/golden/snapshot names with its sha256, so that only a change in the code
# can change the results of ci/test.sh. Not a check: it prepares what the checks run on.
#
#   ci/golden-snapshot.sh renew   [--scope es,lu] [--only-local]   # all of it (make golden-snapshot)
#   ci/golden-snapshot.sh create  [--scope es,lu]   # freeze the downloads of the golden as its data
#   ci/golden-snapshot.sh publish [--scope es,lu]   # push them to the registry (private)
#
# `--scope` takes the country packs, comma-separated (default: every one with a golden). `renew`
# downloads the sources afresh and measures the new baseline on them
# (GEKO_GOLDEN_FRESH=1 GEKO_GOLDEN_UPDATE=1 ci/test.sh), freezes those same downloads (`create`)
# and publishes them (`publish`), unless `--only-local`: then the snapshot stays on this machine,
# and the golden of the CI cannot find it until it is published. Then review the data table of
# the report and commit golden/snapshot with golden/baseline (docs/PROJECT.md §20.1).
# Publishing needs `oras login ghcr.io` with a token with write:packages. Environment: that of
# ci/golden.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=ci/golden.sh
. ci/golden.sh

create() {
    local dir country regions name sha
    dir=$(golden_dir) country=$(golden_country) regions=$(golden_regions)
    [ -d "$dir/raw" ] || { echo "no downloads in $dir/raw: run GEKO_GOLDEN_FRESH=1 ci/test.sh --scope $country first" >&2; return 1; }
    name="golden-$country${regions:+-${regions//,/-}}-$(date -u +%Y%m%d).tar.gz"
    mkdir -p "$dir/snapshots"
    tar --sort=name --owner=0 --group=0 --numeric-owner --exclude=./.snapshot -C "$dir/raw" -cf - . \
        | gzip -n > "$dir/snapshots/$name"
    sha=$(sha256sum "$dir/snapshots/$name" | cut -d' ' -f1)
    cat > "countries/$country/golden/snapshot" <<EOF
# Frozen downloads of ${regions:-${country^^}} that the golden baseline is measured on (ci/test.sh, ci/golden-snapshot.sh).
file=$name
sha256=$sha
EOF
    echo "$sha" > "$dir/raw/.snapshot"
    echo "$dir/snapshots/$name ($(du -h "$dir/snapshots/$name" | cut -f1)): publish it with ci/golden-snapshot.sh publish --scope $country"
}

# Pushes the snapshot that golden/snapshot names, signed by nothing but its sha256 in the
# repository: the golden checks it before using it. Its package is linked to the repository, so
# that the CI reads it with its own token; it is created private, and stays so.
publish() {
    local dir country snapshot file
    dir=$(golden_dir) country=$(golden_country) snapshot="countries/$(golden_country)/golden/snapshot"
    file=$(snapshot_field "$snapshot" file)
    [ -f "$dir/snapshots/$file" ] || { echo "$dir/snapshots/$file does not exist: run ci/golden-snapshot.sh create --scope $country first" >&2; return 1; }
    (cd "$dir/snapshots" && oras push --no-tty "$(snapshot_ref "$country" "$file")" "$file" \
        --artifact-type application/vnd.geko.golden-snapshot.v1 \
        --annotation "org.opencontainers.image.source=https://github.com/candymaps/geko" \
        --annotation "org.opencontainers.image.description=Frozen downloads of the golden of $country (OpenStreetMap ODbL 1.0, © OpenStreetMap contributors; national sources under their licenses, see countries/$country/README.md)" \
        --annotation "org.opencontainers.image.created=1970-01-01T00:00:00Z")
    echo "published $(snapshot_ref "$country" "$file")"
}

# The data of the golden of the country afresh: its downloads, the baseline measured on them, the
# snapshot of those downloads and, unless `only_local`, its publication.
renew() {
    local country only_local=$1
    country=$(golden_country)
    GEKO_GOLDEN_FRESH=1 GEKO_GOLDEN_UPDATE=1 ci/test.sh --variant results,data,robustness,instructions --scope "$country"
    create
    if [ "$only_local" = 1 ]; then
        echo "not published (--only-local): the golden of the CI cannot find it until ci/golden-snapshot.sh publish --scope $country"
    else
        publish
    fi
    echo "review the data table of the report, then commit countries/$country/golden/snapshot and golden/baseline"
}

usage() { echo "usage: $0 renew|create|publish [--scope country,country…] [--only-local]" >&2; }

command=${1:-}
shift || true
scope=$(for d in countries/*/golden; do basename "$(dirname "$d")"; done | paste -sd, -)
only_local=0
while [ $# -gt 0 ]; do
    case $1 in
        --scope) scope=${2:?--scope takes the country packs}; shift 2 ;;
        --scope=*) scope=${1#*=}; shift ;;
        --only-local) only_local=1; shift ;;
        *) usage; exit 2 ;;
    esac
done
[ "$only_local" = 0 ] || [ "$command" = renew ] || { echo "--only-local is for renew" >&2; usage; exit 2; }
case $command in
    renew | create | publish)
        for country in ${scope//,/ }; do
            [ -d "countries/$country/golden" ] || { echo "countries/$country has no golden" >&2; exit 2; }
            if [ "$command" = renew ]; then
                GEKO_GOLDEN_COUNTRY=$country renew "$only_local"
            else
                GEKO_GOLDEN_COUNTRY=$country "$command"
            fi
        done
        ;;
    *) usage; exit 2 ;;
esac
