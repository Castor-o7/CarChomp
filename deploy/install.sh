#!/bin/sh
# Install carputer on Raspberry Pi OS or Debian (bookworm or newer).
# Run as root from a checkout or release bundle:
#
#   sudo deploy/install.sh [--kiosk USER] [--hotspot SSID PASSPHRASE] [--demo]
#
#   --kiosk USER     open the UI full-screen when USER's desktop session starts
#   --hotspot ...    become a Wi-Fi access point whenever no known network is
#                    in range, so the web UI is always reachable
#   --demo           no GPS or radio yet: run a simulator in their place, so
#                    everything else can be used and tested on the device.
#                    Run again without --demo once the hardware is there.
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
root=$(cd "$(dirname "$0")/.." && pwd)
step() { printf '\n==> %s\n' "$*"; }
have_systemd() { [ -d /run/systemd/system ]; }

step "Packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -qy --no-install-recommends postgresql postgresql-postgis gpsd ca-certificates curl git unzip

step "carputerd"
if [ ! -x "$root/target/release/carputerd" ]; then
    # No prebuilt binary in the bundle: build one. Slow on a Pi (~20 min), once.
    apt-get install -qy --no-install-recommends build-essential
    command -v cargo >/dev/null || [ -x "$HOME/.cargo/bin/cargo" ] ||
        curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    PATH="$HOME/.cargo/bin:$PATH" cargo build --release --locked --manifest-path "$root/Cargo.toml" -p carputerd
fi
install -m 755 "$root/target/release/carputerd" /usr/local/bin/carputerd

step "UI"
if [ ! -f "$root/ui/dist/index.html" ]; then
    apt-get install -qy --no-install-recommends npm
    (cd "$root/ui" && npm ci && npm run build)
fi
rm -rf /usr/share/carputer/ui
mkdir -p /usr/share/carputer
cp -r "$root/ui/dist" /usr/share/carputer/ui

step "pmtiles (offline map downloads)"
if ! command -v pmtiles >/dev/null; then
    case $(uname -m) in
        aarch64 | arm64) arch=arm64 ;;
        *) arch=x86_64 ;;
    esac
    # The release page redirect names the newest version; GitHub's API would
    # too, but it rate-limits anonymous callers.
    releases=https://github.com/protomaps/go-pmtiles/releases
    version=$(curl -sIL -o /dev/null -w '%{url_effective}' $releases/latest | sed 's|.*/v||')
    curl -sfL "$releases/download/v$version/go-pmtiles_${version}_Linux_$arch.tar.gz" | tar -xz -C /usr/local/bin pmtiles ||
        echo "warning: could not install pmtiles; downloading offline maps will not work until it is on the PATH" >&2
fi

step "User, directories, configuration"
id carputer >/dev/null 2>&1 || useradd --system --home-dir /var/lib/carputer --shell /usr/sbin/nologin carputer
mkdir -p /var/lib/carputer/maps /etc/carputer
[ -f /etc/carputer/carputerd.toml ] || install -m 644 "$root/deploy/carputerd.toml" /etc/carputer/carputerd.toml
[ -d /var/lib/carputer/maps/assets ] || "$root/tools/fetch_map_assets.sh" /var/lib/carputer/maps
chown -R carputer: /var/lib/carputer

step "Database"
if have_systemd; then systemctl enable --now postgresql; else service postgresql start; fi
as_postgres() { su postgres -c "psql -qtAX $1"; }
[ "$(as_postgres "-c \"SELECT 1 FROM pg_roles WHERE rolname = 'carputer'\"")" = 1 ] || su postgres -c "createuser carputer"
[ "$(as_postgres "-c \"SELECT 1 FROM pg_database WHERE datname = 'carputer'\"")" = 1 ] || su postgres -c "createdb -O carputer carputer"
# Extensions need a superuser; everything else is carputerd's own migrations.
as_postgres "-d carputer -c 'CREATE EXTENSION IF NOT EXISTS postgis'"

step "Wi-Fi permissions"
# Let the daemon (and nobody else new) manage networks from the web UI.
mkdir -p /etc/polkit-1/rules.d
cat >/etc/polkit-1/rules.d/50-carputer.rules <<'RULES'
polkit.addRule(function (action, subject) {
    if (subject.user == "carputer" && action.id.indexOf("org.freedesktop.NetworkManager.") == 0) {
        return polkit.Result.YES;
    }
});
RULES

if [ -n "$ssid" ]; then
    step "Wi-Fi hotspot fallback"
    # No code involved: NetworkManager tries known networks first because
    # they outrank this profile, and brings the access point up otherwise.
    nmcli connection delete carputer-hotspot >/dev/null 2>&1 || true
    nmcli connection add type wifi ifname wlan0 con-name carputer-hotspot \
        autoconnect yes connection.autoconnect-priority -100 \
        ssid "$ssid" mode ap ipv4.method shared \
        wifi-sec.key-mgmt wpa-psk wifi-sec.psk "$psk"
fi

if [ -n "$kiosk_user" ]; then
    step "Kiosk"
    # Raspberry Pi OS desktop (labwc): run a full-screen browser at login.
    home=$(getent passwd "$kiosk_user" | cut -d: -f6)
    mkdir -p "$home/.config/labwc"
    line='chromium http://localhost --kiosk --password-store=basic --noerrdialogs --disable-infobars --no-first-run --enable-features=OverlayScrollbar &'
    grep -qsF "$line" "$home/.config/labwc/autostart" || echo "$line" >>"$home/.config/labwc/autostart"
    chown -R "$kiosk_user": "$home/.config/labwc"
fi

step "Sensors: $([ -n "$demo" ] && echo simulated || echo real)"
# The simulator listens where carputerd is told to look, away from the real
# gpsd (2947) and Direwolf (8001), so switching is only ever these two lines.
config=/etc/carputer/carputerd.toml
if [ -n "$demo" ]; then
    apt-get install -qy --no-install-recommends python3
    install -m 755 "$root/tools/sim.py" /usr/local/bin/carputer-sim
    install -m 644 "$root/deploy/carputer-sim.service" /etc/systemd/system/carputer-sim.service
    sed -i -e 's|^gpsd = .*|gpsd = "127.0.0.1:12947"|' -e 's|^#* *aprs_kiss = .*|aprs_kiss = "127.0.0.1:18001"|' "$config"
    have_systemd && systemctl daemon-reload && systemctl enable --now carputer-sim
elif [ -f /etc/systemd/system/carputer-sim.service ]; then
    have_systemd && systemctl disable --now carputer-sim
    rm -f /etc/systemd/system/carputer-sim.service /usr/local/bin/carputer-sim
    sed -i -e 's|^gpsd = "127.0.0.1:12947"|gpsd = "127.0.0.1:2947"|' -e 's|^aprs_kiss = "127.0.0.1:18001"|# aprs_kiss = "127.0.0.1:8001"|' "$config"
fi

step "Service"
install -m 644 "$root/deploy/carputerd.service" /etc/systemd/system/carputerd.service
if have_systemd; then
    systemctl daemon-reload
    systemctl enable carputerd
    systemctl restart carputerd
    echo "carputer is running: http://$(hostname).local/"
else
    echo "no systemd here; start it with: su carputer -s /bin/sh -c '/usr/local/bin/carputerd /etc/carputer/carputerd.toml'"
fi
