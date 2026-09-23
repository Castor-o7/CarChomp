#!/bin/sh
# NetworkManager dispatcher script, installed by deploy/install.sh as
# /etc/NetworkManager/dispatcher.d/50-carchomp-avahi: announce this device's
# .local name only while it is on its own hotspot (10.42.0.0/24) or on an
# admin network (ADMIN_NETS in /etc/carchomp/install.env), never on eduroam or
# a cafe's Wi-Fi. By address, not interface: wlan0 is either one.
ADMIN_NETS=""
# shellcheck source=/dev/null
[ ! -f /etc/carchomp/install.env ] || . /etc/carchomp/install.env

# a.b.c.d as a number.
ip2int() {
    old=$IFS IFS=.
    # shellcheck disable=SC2086 # split on the dots
    set -- $1
    IFS=$old
    echo $((($1 << 24) + ($2 << 16) + ($3 << 8) + $4))
}

# Whether IPv4 address $1 is in network $2 (a.b.c.d/n, or one address).
in_net() {
    p=32
    case $2 in */*) p=${2#*/} ;; esac
    m=$((p == 0 ? 0 : (0xffffffff << (32 - p)) & 0xffffffff))
    [ $(($(ip2int "$1") & m)) -eq $(($(ip2int "${2%/*}") & m)) ]
}

for dev in wlan0 eth0; do
    for addr in $(ip -4 -o addr show dev "$dev" 2>/dev/null | sed -n 's|.* inet \([0-9.]*\)/.*|\1|p'); do
        for net in 10.42.0.0/24 $ADMIN_NETS; do
            if in_net "$addr" "$net"; then
                systemctl --no-block start avahi-daemon.socket avahi-daemon.service
                exit 0
            fi
        done
    done
done
# The socket too: a name lookup would start the daemon again through it.
systemctl --no-block stop avahi-daemon.socket avahi-daemon.service
