#!/bin/sh
# Load CarChomp's firewall (/etc/carchomp/firewall.nft, or FILE), or with -c
# only check it. carchomp-firewall.service runs it as
# /usr/local/lib/carchomp/firewall-load.sh; deploy/install.sh checks with it.
#
# The strict reverse-path rule needs the kernel's nftables fib support
# (nft_fib_inet). Where that is missing the whole file would fail and leave
# the car with no firewall at all, so the same rules go in without that one
# line instead, with a warning.
check=""
if [ "${1-}" = -c ]; then check=-c; shift; fi
f=${1:-/etc/carchomp/firewall.nft}
# Missing, the fallback below would feed nft nothing, which it accepts.
[ -r "$f" ] || { echo "$f: not readable" >&2; exit 1; }
err=$(nft $check -f "$f" 2>&1) && exit 0
if sed '/^ *fib saddr \. iif oif missing drop$/d' "$f" | nft $check -f /dev/stdin 2>/dev/null; then
    echo "warning: this kernel has no nftables fib support (nft_fib_inet): the firewall $([ -n "$check" ] && echo loads || echo is up) without its strict reverse-path check" >&2
    exit 0
fi
printf '%s\n' "$err" >&2
exit 1
