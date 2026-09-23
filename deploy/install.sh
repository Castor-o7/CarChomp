#!/bin/sh
# Install carchomp on Raspberry Pi OS or Debian (bookworm or newer).
# Run as root from a checkout or release bundle:
#
#   sudo deploy/install.sh [--kiosk USER] [--hotspot SSID PASSPHRASE] [--demo]
#
#   --kiosk USER     open the UI full-screen when USER's desktop session starts
#   --hotspot ...    become a Wi-Fi access point whenever no known network is
#                    in range, so the web UI is always reachable
#   --demo           no GPS or radio yet: run a simulator in their place, so
#                    everything else can be used and tested on the device.
#                    Run again without --demo once the hardware is there; that
#                    deletes what the simulator recorded.
#
# Safe to run again: it upgrades in place and keeps data and configuration.
set -eu

kiosk_user="" ssid="" psk="" demo=""
while [ $# -gt 0 ]; do
    case $1 in
        --kiosk) kiosk_user=$2; shift 2 ;;
        --hotspot) ssid=$2 psk=$3; shift 3 ;;
        --demo) demo=yes; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
# Check the options before changing anything, so a typo cannot leave a half
# upgraded system behind.
if [ -n "$kiosk_user" ]; then
    home=$(getent passwd "$kiosk_user" | cut -d: -f6)
    [ -n "$home" ] || { echo "--kiosk: no such user: $kiosk_user" >&2; exit 2; }
fi
if [ -n "$ssid" ]; then
    case ${#psk} in
        8 | 9 | [1-5][0-9] | 6[0-3]) ;;
        64) case $psk in *[!0-9a-fA-F]*) echo "--hotspot: a 64-character passphrase must be hex" >&2; exit 2 ;; esac ;;
        *) echo "--hotspot: the passphrase must be 8 to 63 characters" >&2; exit 2 ;;
    esac
    # Raspberry Pi OS keeps Wi-Fi blocked until a country is set.
    if command -v raspi-config >/dev/null && [ -z "$(raspi-config nonint get_wifi_country 2>/dev/null)" ]; then
        echo "--hotspot: set the Wi-Fi country first, e.g. raspi-config nonint do_wifi_country US" >&2; exit 2
    fi
fi
root=$(cd "$(dirname "$0")/.." && pwd)
step() { printf '\n==> %s\n' "$*"; }
have_systemd() { [ -d /run/systemd/system ]; }

step "Packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -qy --no-install-recommends postgresql postgresql-postgis gpsd ca-certificates curl git unzip avahi-daemon \
    $([ -n "$ssid" ] && echo network-manager iw rfkill)

step "carchompd"
if [ -f "$root/Cargo.toml" ]; then
    # A checkout: always build, so an upgrade never installs a stale binary.
    # Slow on a Pi the first time (~20 min); later runs rebuild what changed.
    apt-get install -qy --no-install-recommends build-essential
    PATH="$HOME/.cargo/bin:$PATH"
    # Distro cargo is too old for edition 2024 on bookworm.
    v=$(cargo --version 2>/dev/null | cut -d' ' -f2) || v=""
    if [ -z "$v" ] || [ "$(printf '%s\n1.85.0\n' "$v" | sort -V | head -n1)" != 1.85.0 ]; then
        if command -v rustup >/dev/null; then rustup toolchain install stable --profile minimal && rustup default stable
        else curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal; fi
    fi
    cargo build --release --locked --manifest-path "$root/Cargo.toml" -p carchompd
fi
# Refuse a binary built for another machine (e.g. a synced target/ from a Mac):
# its ELF magic and e_machine must match the userland's (not uname -m, which
# names the kernel: 32-bit Pi OS runs a 64-bit kernel).
elf() { printf '%s%s' "$(od -An -tx1 -N4 "$1")" "$(od -An -tx1 -j18 -N2 "$1")"; }
if [ "$(elf "$root/target/release/carchompd")" != "$(elf /bin/sh)" ]; then
    echo "$root/target/release/carchompd was not built for this system" >&2; exit 1
fi
install -m 755 "$root/target/release/carchompd" /usr/local/bin/carchompd

step "UI"
if [ -f "$root/ui/package.json" ]; then
    # A checkout: always build, like carchompd. Vite needs Node >= 20.19,
    # newer than bookworm's, so take it from NodeSource when the distro's is old.
    node_ok() { node -e 'const [a, b] = process.versions.node.split(".").map(Number); process.exit(a > 22 || (a === 22 && b >= 12) || (a === 20 && b >= 19) ? 0 : 1)' 2>/dev/null; }
    node_ok || apt-get install -qy --no-install-recommends npm
    if ! node_ok; then
        mkdir -p /etc/apt/keyrings
        curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key -o /etc/apt/keyrings/nodesource.asc
        echo "deb [signed-by=/etc/apt/keyrings/nodesource.asc] https://deb.nodesource.com/node_22.x nodistro main" >/etc/apt/sources.list.d/nodesource.list
        apt-get update -q
        apt-get install -qy nodejs
    fi
    (cd "$root/ui" && npm ci && npm run build)
fi
[ -f "$root/ui/dist/index.html" ] || { echo "no UI in $root/ui/dist" >&2; exit 1; }
rm -rf /usr/share/carchomp/ui
mkdir -p /usr/share/carchomp
cp -r "$root/ui/dist" /usr/share/carchomp/ui

step "pmtiles (offline map downloads)"
# Pinned and checksummed: carchompd runs this binary. To upgrade, change the
# version and both sums (from the release's asset digests) together.
version=1.31.2
case $(uname -m) in
    aarch64 | arm64) arch=arm64 sum=f8bd47e7ea866863489cad588fbaf2f31f42e5821f7a03f009b3769f05801cb1 ;;
    x86_64) arch=x86_64 sum=3ed7dbf4ec2e6dfe5e25b6f70d1ffc932729f93c86db353bf514dd71010a312f ;;
    *) arch="" ;;
esac
if [ -z "$arch" ]; then
    echo "warning: no pmtiles build for $(uname -m); downloading offline maps will not work" >&2
elif ! pmtiles version 2>&1 | grep -qF "$version"; then
    tmp=$(mktemp -d)
    if curl -sfL -o "$tmp/pmtiles.tar.gz" "https://github.com/protomaps/go-pmtiles/releases/download/v$version/go-pmtiles_${version}_Linux_$arch.tar.gz" &&
        echo "$sum  $tmp/pmtiles.tar.gz" | sha256sum -c --quiet &&
        tar -xzf "$tmp/pmtiles.tar.gz" -C "$tmp" pmtiles; then
        install -m 755 "$tmp/pmtiles" /usr/local/bin/pmtiles
    else
        echo "warning: could not install pmtiles; downloading offline maps will not work until it is on the PATH" >&2
    fi
    rm -rf "$tmp"
fi

step "User, directories, configuration"
id carchomp >/dev/null 2>&1 || useradd --system --home-dir /var/lib/carchomp --shell /usr/sbin/nologin carchomp
mkdir -p /var/lib/carchomp/maps /etc/carchomp
[ -f /etc/carchomp/carchompd.toml ] || install -m 644 "$root/deploy/carchompd.toml" /etc/carchomp/carchompd.toml
[ -d /var/lib/carchomp/maps/assets ] || "$root/tools/fetch_map_assets.sh" /var/lib/carchomp/maps
chown -R carchomp: /var/lib/carchomp

step "Database"
if have_systemd; then systemctl enable --now postgresql; else service postgresql start; fi
as_postgres() { su postgres -c "psql -qtAX $1"; }
[ "$(as_postgres "-c \"SELECT 1 FROM pg_roles WHERE rolname = 'carchomp'\"")" = 1 ] || su postgres -c "createuser carchomp"
[ "$(as_postgres "-c \"SELECT 1 FROM pg_database WHERE datname = 'carchomp'\"")" = 1 ] || su postgres -c "createdb -O carchomp carchomp"
# Extensions need a superuser; everything else is carchompd's own migrations.
as_postgres "-d carchomp -c 'CREATE EXTENSION IF NOT EXISTS postgis'"

step "Wi-Fi permissions"
# Let the daemon (and nobody else new) manage networks from the web UI.
mkdir -p /etc/polkit-1/rules.d
cat >/etc/polkit-1/rules.d/50-carchomp.rules <<'RULES'
polkit.addRule(function (action, subject) {
    if (subject.user == "carchomp" && action.id.indexOf("org.freedesktop.NetworkManager.") == 0) {
        return polkit.Result.YES;
    }
});
RULES

step "Sensors: $([ -n "$demo" ] && echo simulated || echo real)"
# The simulator listens away from the real gpsd (2947) and Direwolf (8001).
# carchompd is pointed at it by a demo copy of the configuration, so the
# real one is never edited.
config=/etc/carchomp/carchompd.toml dropin=/etc/systemd/system/carchompd.service.d/demo.conf
if [ -n "$demo" ]; then
    apt-get install -qy --no-install-recommends python3
    install -m 755 "$root/tools/sim.py" /usr/local/bin/carchomp-sim
    install -m 644 "$root/deploy/carchomp-sim.service" /etc/systemd/system/carchomp-sim.service
    { printf 'gpsd = "127.0.0.1:12947"\naprs_kiss = "127.0.0.1:18001"\n'; sed -e '/^#* *gpsd = /d' -e '/^#* *aprs_kiss = /d' "$config"; } >/etc/carchomp/demo.toml
    mkdir -p "${dropin%/*}"
    printf '[Service]\nExecStart=\nExecStart=/usr/local/bin/carchompd /etc/carchomp/demo.toml\n' >"$dropin"
    if have_systemd; then systemctl daemon-reload && systemctl enable carchomp-sim && systemctl restart carchomp-sim; fi
elif [ -f /etc/systemd/system/carchomp-sim.service ]; then
    if have_systemd; then systemctl disable --now carchomp-sim; systemctl stop carchompd; fi
    rm -f /etc/systemd/system/carchomp-sim.service /usr/local/bin/carchomp-sim /etc/carchomp/demo.toml "$dropin"
    # Older installers switched by editing the configuration itself.
    sed -i -e 's|^gpsd = "127.0.0.1:12947"|gpsd = "127.0.0.1:2947"|' -e 's|^aprs_kiss = "127.0.0.1:18001"|# aprs_kiss = "127.0.0.1:8001"|' "$config"
    # Simulated drives would make real roads look known, and the simulated
    # APRS scene would stay on the map: remove all the simulator recorded.
    su postgres -c "psql -qtAX -1 -v ON_ERROR_STOP=1 -d carchomp" <<'SQL' || echo "warning: could not remove the simulator's data" >&2
CREATE TEMP TABLE sim ON COMMIT DROP AS
    SELECT id FROM source WHERE (kind, name) IN (('gps', '127.0.0.1:12947'), ('aprs', '127.0.0.1:18001'));
WITH d AS (DELETE FROM track WHERE id IN (SELECT track_id FROM fix WHERE source_id IN (SELECT id FROM sim)) RETURNING 1)
    SELECT 'removed ' || count(*) || ' simulated tracks' FROM d;
WITH d AS (DELETE FROM aprs_packet WHERE source_id IN (SELECT id FROM sim) RETURNING 1)
    SELECT 'removed ' || count(*) || ' simulated APRS packets' FROM d;
DELETE FROM source WHERE id IN (SELECT id FROM sim);
SQL
fi

step "Service"
install -m 644 "$root/deploy/carchompd.service" /etc/systemd/system/carchompd.service
if have_systemd; then
    systemctl daemon-reload
    systemctl enable carchompd
    systemctl restart carchompd
fi

if [ -n "$ssid" ]; then
    step "Wi-Fi hotspot fallback"
    rfkill unblock wlan
    nmcli radio wifi on
    # Replace the profile only once the new one exists, so a failure keeps the
    # old fallback. It never autoconnects: NetworkManager would bring an access
    # point up before its first scan has found a known network, and would not
    # leave it when one comes into range. carchomp-wifi decides instead.
    nmcli connection delete carchomp-hotspot-new >/dev/null 2>&1 || true
    nmcli connection add type wifi ifname wlan0 con-name carchomp-hotspot-new \
        autoconnect no ssid "$ssid" mode ap ipv4.method shared ipv4.addresses 10.42.0.1/24 \
        wifi-sec.key-mgmt wpa-psk wifi-sec.psk "$psk" >/dev/null
    nmcli connection delete carchomp-hotspot >/dev/null 2>&1 || true
    nmcli connection modify carchomp-hotspot-new connection.id carchomp-hotspot
    cat >/usr/local/sbin/carchomp-wifi <<'SH'
#!/bin/sh
# Join a known Wi-Fi network in range, else host carchomp-hotspot.
# Run every two minutes by carchomp-wifi.timer; installed by deploy/install.sh.
dev=wlan0 ap=carchomp-hotspot
case $(nmcli -g GENERAL.STATE device show $dev) in
    100*) [ "$(nmcli -g GENERAL.CONNECTION device show $dev)" = $ap ] || exit 0
        # Someone is on the hotspot: do not pull it from under them.
        [ -z "$(iw dev $dev station dump)" ] || exit 0
        nmcli connection down $ap >/dev/null ;;
    30*) ;;
    *) exit 0 ;; # connecting, or unavailable
esac
seen=$(nmcli -g SSID device wifi list ifname $dev --rescan yes | sed 's/\\:/:/g')
nmcli -g NAME,TYPE connection show | sed -n 's/:802-11-wireless$//p' | sed 's/\\:/:/g' | while IFS= read -r name; do
    [ "$name" != $ap ] || continue
    ssid=$(nmcli -g 802-11-wireless.ssid connection show "$name" | sed 's/\\:/:/g')
    printf '%s\n' "$seen" | grep -qxF "$ssid" && nmcli connection up "$name" >/dev/null 2>&1 && exit 10
done
[ $? -eq 10 ] || nmcli connection up $ap >/dev/null
SH
    chmod 755 /usr/local/sbin/carchomp-wifi
    cat >/etc/systemd/system/carchomp-wifi.service <<'UNIT'
[Unit]
Description=CarChomp: join a known Wi-Fi network, else host the hotspot
After=NetworkManager.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/carchomp-wifi
UNIT
    cat >/etc/systemd/system/carchomp-wifi.timer <<'UNIT'
[Unit]
Description=CarChomp Wi-Fi fallback check

[Timer]
OnBootSec=45s
OnUnitActiveSec=2min

[Install]
WantedBy=timers.target
UNIT
    if have_systemd; then systemctl daemon-reload && systemctl enable --now carchomp-wifi.timer; fi
    # Unblocking takes a moment to show.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        case $(nmcli -g GENERAL.STATE device show wlan0 2>/dev/null) in "" | 10* | 20*) sleep 1 ;; *) break ;; esac
    done
    case $(nmcli -g GENERAL.STATE device show wlan0 2>/dev/null) in
        "" | 10* | 20*) echo "wlan0 is unavailable (rfkill, country, driver?); the hotspot cannot come up" >&2; exit 1 ;;
    esac
fi

if [ -n "$kiosk_user" ]; then
    step "Kiosk"
    # Raspberry Pi OS desktop (labwc): run a full-screen browser at login, once
    # carchompd answers; Chromium's error page would never retry by itself.
    mkdir -p "$home/.config/labwc"
    autostart=$home/.config/labwc/autostart
    line="sh -c 'until curl -sf http://localhost/api/health >/dev/null; do sleep 1; done; exec chromium http://localhost --kiosk --password-store=basic --noerrdialogs --disable-infobars --no-first-run --enable-features=OverlayScrollbar' &"
    [ ! -f "$autostart" ] || sed -i '\|chromium http://localhost --kiosk|d' "$autostart"
    echo "$line" >>"$autostart"
    chown -R "$kiosk_user": "$home/.config/labwc"
fi

if have_systemd; then
    echo "carchomp is running: http://$(hostname).local/ or http://$(hostname -I | cut -d' ' -f1)/"
    [ -z "$ssid" ] || echo "on the $ssid hotspot: http://10.42.0.1/"
else
    [ -z "$demo" ] || echo "no systemd here; start the simulator with: /usr/local/bin/carchomp-sim --gpsd 12947 --kiss 18001 &"
    echo "no systemd here; start carchompd with: setcap cap_net_bind_service=+ep /usr/local/bin/carchompd && su carchomp -s /bin/sh -c '/usr/local/bin/carchompd /etc/carchomp/$([ -n "$demo" ] && echo demo || echo carchompd).toml'"
fi
