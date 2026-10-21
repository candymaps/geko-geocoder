# What ci/test.sh and ci/golden-snapshot.sh share about the golden of a country pack: where its
# data live and where its snapshots are published (sourced by both).
#   GEKO_GOLDEN_COUNTRY    country pack, ISO 3166-1 in lowercase (default es)
#   GEKO_GOLDEN_REGIONS    regions to build, ISO 3166-2, comma-separated (default: ES-PM, Baleares, for
#                          Spain; the whole country for any other)
#   GEKO_GOLDEN_DIR        data directory, kept between runs as a cache (default
#                          /var/lib/geko/golden/<country>)
#   GEKO_GOLDEN_REGISTRY   where the snapshots are published (default ghcr.io/candymaps/geko/countries)

# shellcheck source=ci/oras.sh
. ci/oras.sh
# Where the golden snapshots are published: <registry>/<country>/golden:<snapshot>, private.
GOLDEN_REGISTRY="${GEKO_GOLDEN_REGISTRY:-ghcr.io/candymaps/geko/countries}"

golden_dir() { echo "${GEKO_GOLDEN_DIR:-/var/lib/geko/golden/$(golden_country)}"; }
golden_country() { echo "${GEKO_GOLDEN_COUNTRY:-es}"; }
golden_regions() { if [ "$(golden_country)" = es ]; then echo "${GEKO_GOLDEN_REGIONS-ES-PM}"; else echo "${GEKO_GOLDEN_REGIONS:-}"; fi; }

# Value of `key` in a snapshot file of `key=value` lines.
snapshot_field() { sed -n "s/^$2=//p" "$1"; }

# The artifact of the snapshot archive `$2` of country `$1`: golden-es-ES-PM-20261017.tar.gz is
# <registry>/es/golden:es-ES-PM-20261017.
snapshot_ref() { local tag=${2#golden-}; echo "$GOLDEN_REGISTRY/$1/golden:${tag%.tar.gz}"; }
