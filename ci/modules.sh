#!/usr/bin/env bash
# The modules of this repository as the packages geko downloads (docs/MODULES_PLAN.md): each folder
# of modules/ with a module.toml, compiled in release, with that module.toml completed (version,
# contract, platform) and signed.
#
#   ci/modules.sh build <folder> [module…]   compiles every module (or those named) into
#                                            <folder>/<module>/
#   ci/modules.sh publish <folder>           signs the packages of <folder> and pushes them to
#                                            $GEKO_MODULES_REGISTRY/geko-<module>
#   ci/modules.sh check [module…]            fails unless every module is published for the
#                                            contract of this geko and its platform
#
# The version of a module is that of Cargo.toml, followed by the commit (<version>-<commit>) unless
# GEKO_RELEASE=1; it is compiled into the module (GEKO_MODULE_VERSION), which says it to the host
# and to the bundles it builds. Each package goes to geko-<module>:<version>-<platform> and
# contract-<n>-<platform>, the newest of its contract, which every geko of that contract downloads;
# its manifest says the three in its annotations (geko.version, geko.contract, geko.platform), so
# that geko can refuse one that is not for it before downloading it.
#
# `build` must run on a system as old as the image of geko (debian:bookworm-slim): a module that
# needs a newer glibc does not start in it. The CI runs it in rust:<toolchain>-bookworm. Publishing
# needs GEKO_PACK_SIGNING_KEY (hex), the key of the packs, and a login to the registry.
set -euo pipefail
cd "$(dirname "$0")/.."

target=${CARGO_TARGET_DIR:-target}
registry=${GEKO_MODULES_REGISTRY:-ghcr.io/candymaps}

field() { tr -d '\r' < "$2" | sed -n "s/^$1 *= *\"\{0,1\}\([^\"]*\)\"\{0,1\} *\$/\1/p" | head -1; }

# The folders of the modules, or of those named.
modules() {
    local dir name
    for dir in modules/*/; do
        [ -f "$dir/module.toml" ] || continue
        name=$(field name "$dir/module.toml")
        [ $# -eq 0 ] || [[ " $* " == *" $name "* ]] || continue
        echo "${dir%/}"
    done
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
    local out=$1 tool contract platform version dir name kind entry
    shift
    tool=$(sign_tool)
    contract=$("$tool" contract)
    platform=$("$tool" platform)
    version=$(version)
    for dir in $(modules "$@"); do
        name=$(field name "$dir/module.toml")
        kind=$(field kind "$dir/module.toml")
        entry=$(field entry "$dir/module.toml")
        GEKO_MODULE_VERSION=$version cargo build --release --locked --bin "$entry"
        rm -rf "${out:?}/$name"
        mkdir -p "$out/$name"
        cp "$target/release/$entry" "$out/$name/$entry"
        printf 'kind = "%s"\nentry = "%s"\nname = "%s"\nversion = "%s"\ncontract = %s\nplatform = "%s"\n' \
            "$kind" "$entry" "$name" "$version" "$contract" "$platform" > "$out/$name/module.toml"
        echo "built $name $version (contract $contract, $platform) in $out/$name"
    done
}

# shellcheck source=ci/oras.sh
. ci/oras.sh

publish() {
    : "${GEKO_PACK_SIGNING_KEY:?GEKO_PACK_SIGNING_KEY is not set}"
    local out=$1 tool dir name version contract platform files ref
    tool=$(sign_tool)
    for dir in "$out"/*/; do
        dir=${dir%/}
        name=$(field name "$dir/module.toml")
        version=$(field version "$dir/module.toml")
        contract=$(field contract "$dir/module.toml")
        platform=$(field platform "$dir/module.toml")
        "$tool" sign "$dir"
        "$tool" verify "$dir"
        ref="$registry/geko-$name"
        files=$(cd "$dir" && find . -type f | sed 's|^\./||' | sort)
        # A fixed date, instead of that of the push, so that the same files give the same manifest.
        # shellcheck disable=SC2086
        (cd "$dir" && oras push --disable-path-validation "$ref:$version-$platform,contract-$contract-$platform" \
            --artifact-type application/vnd.geko.module.v1 \
            --annotation "org.opencontainers.image.created=1970-01-01T00:00:00Z" \
            --annotation "geko.version=$version" --annotation "geko.contract=$contract" --annotation "geko.platform=$platform" \
            $files)
        echo "published $ref ($version-$platform, contract-$contract-$platform)"
    done
}

check() {
    local tool contract platform dir name ref missing=0
    tool=$(sign_tool)
    contract=$("$tool" contract)
    platform=$("$tool" platform)
    for dir in $(modules "$@"); do
        name=$(field name "$dir/module.toml")
        ref="$registry/geko-$name:contract-$contract-$platform"
        if oras manifest fetch "$ref" > /dev/null 2>&1; then
            echo "published: $ref"
        else
            echo "not published: $ref" >&2
            missing=1
        fi
    done
    return $missing
}

case "${1:-}" in
    build) shift; build "${1:?usage: ci/modules.sh build <folder> [module…]}" "${@:2}" ;;
    publish) shift; publish "${1:?usage: ci/modules.sh publish <folder>}" ;;
    check) shift; check "$@" ;;
    *) echo "usage: ci/modules.sh build <folder> [module…] | publish <folder> | check [module…]" >&2; exit 2 ;;
esac
