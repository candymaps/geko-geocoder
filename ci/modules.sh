#!/usr/bin/env bash
# The module of this repository as the package geko downloads: module/, with
# its library (the crate geko-module-<module>-dylib) compiled in release, and its module.toml
# completed (version, platform) and signed.
#
#   ci/modules.sh build <folder> [module…]   compiles every module (or those named) into
#                                            <folder>/<module>/
#   ci/modules.sh publish <folder>           signs the packages of <folder> and pushes them to
#                                            $GEKO_MODULES_REGISTRY/geko-<module>
#
# The version of a module is that of Cargo.toml, followed by the commit (<version>-<commit>) unless
# GEKO_RELEASE=1; it is compiled into the module (GEKO_MODULE_VERSION), which says it to the host
# and to the bundles it builds. Each package goes to geko-<module>:<version>-<platform>, which
# the modules.lock of a geko names by its digest; its manifest says both in its annotations
# (geko.version, geko.platform).
#
# `build` must run on a system as old as the image of geko (debian:bookworm-slim): a module that
# needs a newer glibc does not start in it. The CI runs it in rust:<toolchain>-bookworm. Publishing
# needs GEKO_PACK_SIGNING_KEY (hex), the key of the packs, and a login to the registry.
set -euo pipefail
cd "$(dirname "$0")/.."

target=${CARGO_TARGET_DIR:-target}
registry=${GEKO_MODULES_REGISTRY:-ghcr.io/candymaps}

field() { tr -d '\r' < "$2" | sed -n "s/^$1 *= *\"\{0,1\}\([^\"]*\)\"\{0,1\} *\$/\1/p" | head -1; }

# The folder of the module, unless other modules are named.
modules() {
    local name
    name=$(field name module/module.toml)
    [ $# -eq 0 ] || [[ " $* " == *" $name "* ]] || return 0
    echo module
}

sign_tool() {
    cargo build --release --locked -p geko-artifact --bin geko-module-sign >&2
    echo "$target/release/geko-module-sign"
}

version() {
    local version
    # Without the CR of a checkout made on Windows.
    version=$(tr -d '\r' < Cargo.toml | sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p')
    if [ "${GEKO_RELEASE:-0}" = 1 ]; then echo "$version"; else echo "$version-$(git rev-parse --short HEAD)"; fi
}

build() {
    local out=$1 tool platform version dir name kind entry
    shift
    tool=$(sign_tool)
    platform=$("$tool" platform)
    version=$(version)
    for dir in $(modules "$@"); do
        name=$(field name "$dir/module.toml")
        kind=$(field kind "$dir/module.toml")
        entry=$(field entry "$dir/module.toml")
        GEKO_MODULE_VERSION=$version cargo build --release --locked -p "geko-module-$name-dylib"
        rm -rf "${out:?}/$name"
        mkdir -p "$out/$name"
        cp "$target/release/$entry" "$out/$name/$entry"
        printf 'kind = "%s"\nentry = "%s"\nname = "%s"\nversion = "%s"\nplatform = "%s"\n' \
            "$kind" "$entry" "$name" "$version" "$platform" > "$out/$name/module.toml"
        echo "built $name $version ($platform) in $out/$name"
    done
}

# shellcheck source=ci/oras.sh
. ci/oras.sh

publish() {
    : "${GEKO_PACK_SIGNING_KEY:?GEKO_PACK_SIGNING_KEY is not set}"
    local out=$1 tool dir name version platform files ref
    tool=$(sign_tool)
    for dir in "$out"/*/; do
        dir=${dir%/}
        name=$(field name "$dir/module.toml")
        version=$(field version "$dir/module.toml")
        platform=$(field platform "$dir/module.toml")
        "$tool" sign "$dir"
        "$tool" verify "$dir"
        ref="$registry/geko-$name"
        files=$(cd "$dir" && find . -type f | sed 's|^\./||' | sort)
        # A fixed date, instead of that of the push, so that the same files give the same manifest.
        # shellcheck disable=SC2086
        (cd "$dir" && oras push --disable-path-validation "$ref:$version-$platform" \
            --artifact-type application/vnd.geko.module.v1 \
            --annotation "org.opencontainers.image.created=1970-01-01T00:00:00Z" \
            --annotation "geko.version=$version" --annotation "geko.platform=$platform" \
            $files)
        echo "published $ref ($version-$platform)"
    done
}

case "${1:-}" in
    build) shift; build "${1:?usage: ci/modules.sh build <folder> [module…]}" "${@:2}" ;;
    publish) shift; publish "${1:?usage: ci/modules.sh publish <folder>}" ;;
    *) echo "usage: ci/modules.sh build <folder> [module…] | publish <folder>" >&2; exit 2 ;;
esac
