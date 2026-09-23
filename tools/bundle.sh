#!/bin/sh
# Build a release bundle, the tarball deploy/install.sh and the web UI's
# system update install from:
#
#   tools/bundle.sh [TARGET]      -> target/bundle/carchomp-<version>-<arch>.tar.gz
#
# TARGET is a Rust target triple, aarch64-unknown-linux-gnu (64-bit Raspberry
# Pi OS) by default, or "host". Building for another machine than this one
# needs `cross` (Docker) or `cargo zigbuild` (zig) on the PATH; without them,
# run this on an arm64 Linux machine, as the release workflow does. The UI is
# built with npm (Node >= 20.19).
#
# The bundle has one top directory, carchomp/: deploy/, tools/, README.md,
# carchompd.example.toml, VERSION, target/release/carchompd and ui/dist/. It
# has no Cargo.toml and no ui/package.json, so the installer uses the prebuilt
# binary and UI instead of building them.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
host=$(rustc -vV | sed -n 's/^host: //p')
target=${1:-aarch64-unknown-linux-gnu}
[ "$target" != host ] || target=$host
case $target in
    aarch64-*-linux-*) arch=arm64 ;;
    x86_64-*-linux-*) arch=amd64 ;;
    arm*-linux-*) arch=armhf ;;
    *) arch=$target ;;
esac
# The daemon reports this in /api/health; CI may set it, else git names it.
CARCHOMP_VERSION=${CARCHOMP_VERSION:-$(git -C "$root" describe --tags --always --dirty)}
export CARCHOMP_VERSION

step() { printf '\n==> %s\n' "$*"; }
step "carchompd $CARCHOMP_VERSION for $target"
build="--release --locked --manifest-path $root/Cargo.toml -p carchompd"
if [ "$target" = "$host" ]; then
    cargo build $build
    bin=$root/target/release/carchompd
else
    if command -v cross >/dev/null; then
        # cross builds in a container; let the version in.
        CROSS_BUILD_ENV_PASSTHROUGH=CARCHOMP_VERSION cross build $build --target "$target"
    elif command -v cargo-zigbuild >/dev/null; then
        cargo zigbuild $build --target "$target"
    else
        echo "cannot build for $target on $host: install cross or cargo-zigbuild, or run this on $target" >&2
        exit 1
    fi
    bin=$root/target/$target/release/carchompd
fi

step "UI"
[ -d "$root/ui/node_modules" ] || (cd "$root/ui" && npm ci)
(cd "$root/ui" && npm run build)

step "Bundle"
out=$root/target/bundle
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
dir=$tmp/carchomp
mkdir -p "$dir/target/release" "$dir/ui" "$out"
cp -R "$root/deploy" "$root/tools" "$root/README.md" "$root/carchompd.example.toml" "$dir/"
cp "$bin" "$dir/target/release/carchompd"
cp -R "$root/ui/dist" "$dir/ui/dist"
echo "$CARCHOMP_VERSION" >"$dir/VERSION"
tarball=$out/carchomp-$CARCHOMP_VERSION-$arch.tar.gz
# Owned by root, whoever built it; no macOS resource forks.
if tar --version | grep -q GNU; then owner="--owner=0 --group=0 --numeric-owner"; else owner="--uid 0 --gid 0"; fi
COPYFILE_DISABLE=1 tar -czf "$tarball" $owner -C "$tmp" carchomp
echo "$tarball"
