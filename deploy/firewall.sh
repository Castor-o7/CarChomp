#!/bin/sh
# Print CarChomp's nftables ruleset: drop everything inbound except what the
# car needs. Arguments: admin networks (IPv4 CIDR) that may also use the web
# UI and see the .local name. deploy/install.sh writes the output to
# /etc/carchomp/firewall.nft, which carchomp-firewall.service loads.
#
# Rules match addresses, never interfaces: with one radio, wlan0 is sometimes
# a client (eduroam, a phone) and sometimes the hotspot (10.42.0.0/24).
# Only this table is replaced: NetworkManager's hotspot NAT lives elsewhere.
set -eu
cat <<'NFT'
#!/usr/sbin/nft -f
# Written by deploy/install.sh (deploy/firewall.sh); run it again to change.
table inet carchomp
delete table inet carchomp
table inet carchomp {
    chain input {
        type filter hook input priority filter; policy drop;
        iif lo accept
        ct state invalid drop
        ct state established,related accept
        icmp type echo-request limit rate 5/second accept
        icmp type { destination-unreachable, time-exceeded, parameter-problem } accept
        icmpv6 type echo-request limit rate 5/second accept
        icmpv6 type { destination-unreachable, packet-too-big, time-exceeded, parameter-problem, nd-router-advert, nd-neighbor-solicit, nd-neighbor-advert, mld-listener-query } accept
        # Replies to this device's own DHCP requests (as a Wi-Fi or wired client).
        udp sport 67 udp dport 68 accept
        udp sport 547 udp dport 546 accept
        # The hotspot: web UI, SSH, and NetworkManager's dnsmasq (DNS, DHCP;
        # a new client asks for an address from 0.0.0.0), and mDNS.
        ip saddr 0.0.0.0 udp sport 68 udp dport 67 accept
        ip saddr 10.42.0.0/24 tcp dport { 22, 53, 80 } accept
        ip saddr 10.42.0.0/24 udp dport { 53, 67, 5353 } accept
        # SSH from anywhere: key-only once a user has a key (install.sh).
        tcp dport 22 accept
NFT
if [ $# -gt 0 ]; then
    nets=$(printf '%s, ' "$@")
    printf '        # --admin-net\n'
    printf '        ip saddr { %s } tcp dport 80 accept\n' "${nets%, }"
    printf '        ip saddr { %s } udp dport 5353 accept\n' "${nets%, }"
fi
printf '    }\n}\n'
