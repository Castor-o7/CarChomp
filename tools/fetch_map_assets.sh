#!/bin/sh
# Fetch the fonts and sprites the offline vector map needs for labels and
# icons. Run once, online: fetch_map_assets.sh /var/lib/carchomp/maps
set -eu
maps_dir=${1:?usage: $0 <maps_dir>}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git clone --quiet --depth 1 https://github.com/protomaps/basemaps-assets "$tmp"
mkdir -p "$maps_dir/assets"
rm -rf "$maps_dir/assets/fonts" "$maps_dir/assets/sprites"
mv "$tmp/fonts" "$tmp/sprites" "$maps_dir/assets/"
du -sh "$maps_dir/assets"
