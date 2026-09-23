#!/bin/sh
# Install a release bundle uploaded from the web UI. deploy/install.sh puts
# this at /usr/local/lib/carchomp/update.sh; carchomp-update.service runs it
# as root once carchompd has saved the upload. Progress, for
# GET /api/system/update, goes to status.json.
#
#   update.sh             install update/bundle.tar.gz with install.sh --reuse
#   update.sh --stopped   mark an update that died (or a power cut) as failed,
#                         so a new one can be started
set -u
dir=/var/lib/carchomp/update
bundle=$dir/bundle.tar.gz src=$dir/src log=$dir/update.log
version=""

# $1 as the inside of a JSON string: valid UTF-8, no control characters,
# lines joined with \n.
json() {
    printf '%s' "$1" | iconv -c -f UTF-8 -t UTF-8 | tr '\t' ' ' | tr -d '\000-\010\013-\037\177' |
        sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | awk 'NR > 1 { printf "\\n" } { printf "%s", $0 }'
}

# Replace status.json in one step, so a reader never sees half of it.
status() {
    tmp=$(mktemp "$dir/.status.XXXXXX") || return 0
    printf '{"state":"%s","version":"%s","message":"%s","log":"%s"}\n' "$1" "$(json "$version")" \
        "$(json "$2")" "$(json "$(tail -n 40 "$log" 2>/dev/null)")" >"$tmp"
    chmod 644 "$tmp"
    mv -f "$tmp" "$dir/status.json"
}

fail() {
    status failed "$1"
    rm -f "$bundle"
    exit 1
}

if [ "${1-}" = --stopped ]; then
    # Still starting (this is carchompd restarting during an update): leave it.
    [ "$(systemctl is-active carchomp-update.service 2>/dev/null)" != activating ] || exit 0
    if grep -qs '"state":"running"' "$dir/status.json"; then
        version=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$dir/status.json")
        status failed "the update was interrupted; upload it again"
    fi
    exit 0
fi

rm -f "$log"
: >"$log"
status running "checking the upload"
[ -f "$bundle" ] || fail "nothing was uploaded"
list=$(tar -tzf "$bundle" 2>>"$log") || fail "not a gzip tarball"
if printf '%s\n' "$list" | grep -qvE '^carchomp(/|$)' || printf '%s\n' "$list" | grep -qE '(^|/)\.\.(/|$)'; then
    fail "not a carchomp bundle: everything must be inside carchomp/"
fi
for f in carchomp/VERSION carchomp/deploy/install.sh; do
    printf '%s\n' "$list" | grep -qxF "$f" || fail "not a carchomp bundle: no $f"
done
version=$(tar -xzOf "$bundle" carchomp/VERSION 2>>"$log" | head -n 1)

status running "unpacking"
rm -rf "$src"
mkdir "$src" || fail "could not create $src"
tar -xzf "$bundle" --no-same-owner -C "$src" 2>>"$log" || fail "could not unpack the bundle"

sh "$src/carchomp/deploy/install.sh" --reuse >>"$log" 2>&1 &
pid=$!
while kill -0 "$pid" 2>/dev/null; do
    status running "installing"
    sleep 2
done
wait "$pid"
rc=$?
[ "$rc" -eq 0 ] || fail "the installer failed (exit status $rc)"
rm -f "$bundle"
status "done" "installed"
