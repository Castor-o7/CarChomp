# CarChomp

*Take a chomp out of your drive.*

A Raspberry Pi car computer that tracks your drives and tells you when you're
on a road you've never driven. A from-scratch take on the carputer capstone,
built from the stakeholder's original directive ([stakeholder.md](stakeholder.md)). Design and roadmap:
[DESIGN.md](DESIGN.md).

    crates/carchomp-core   pure logic: gpsd, SmartBeaconing, KISS, APRS  (no I/O)
    crates/carchompd       the daemon: sources -> bus -> PostGIS, /ws, /api
    ui                     one Svelte + MapLibre app for the touchscreen and the browser
    deploy                 install script, systemd unit, production config
    tools/sim.py           stand-in for the sensors: pretend gpsd + APRS TNC
    tools/fetch_map_assets.sh   fonts + sprites for the offline map

## Try it

    docker compose -f compose.dev.yml up -d --wait       # PostGIS on :5433
    (cd ui && npm install && npm run build)              # UI -> ui/dist
    python3 tools/sim.py --gpsd 12947 --kiss 18001 &     # pretend GPS + APRS radio
    cat > dev.toml <<'END'
    database_url = "postgres://carchomp:carchomp@localhost:5433/carchomp"
    gpsd = "127.0.0.1:12947"
    aprs_kiss = "127.0.0.1:18001"
    ui_dir = "ui/dist"
    END
    cargo run -p carchompd -- dev.toml                   # then open http://localhost:8000

The simulator drives a city block, parks, and drives it again with a detour,
forever: the badge goes *New road*, then *Been here* / *New road* on the
second lap, then *Been here* from then on. The APRS panel fills with a
red-flag warning, a bulletin, a brush fire and a road closure that is later
lifted. `--speedup 20` hurries it along, `--gpx drive.gpx` replays a real
drive out and back, `--once` exits after one pass. For UI work, `npm run dev`
in `ui/` proxies to the daemon with hot reload. The API by hand:

    curl localhost:8000/api/tracks
    curl localhost:8000/api/tracks/1                     # GeoJSON
    curl 'localhost:8000/api/tracks?near=-122.6587,45.5125,50'

`cargo test` runs the core tests; they need nothing but a Rust toolchain (1.85+).

## Install on a Pi

On Raspberry Pi OS or Debian (bookworm or newer), from a checkout or a
release bundle:

    sudo deploy/install.sh --kiosk "$USER" --hotspot carchomp --gps /dev/ttyACM0 --radio

All options are optional. It installs PostgreSQL/PostGIS, gpsd, chrony, the
`pmtiles` tool, builds `carchompd` and the UI if the bundle has no prebuilt
ones, creates the database, and starts `carchompd.service` on port 80. Run it
again to upgrade; data and `/etc/carchomp/carchompd.toml` are kept. It only
goes online for packages that are missing, so a re-run from a bundle works on
the car's own hotspot.

- `--kiosk USER` opens the UI full-screen when USER's desktop session starts.
  Use the desktop user (`"$USER"` above); current Raspberry Pi OS has no `pi`.
- `--hotspot SSID` hosts a Wi-Fi access point whenever no known network is in
  range (the UI is then at http://10.42.0.1/). The installer asks for the
  passphrase; in a script, pipe it in instead:
  `sudo deploy/install.sh --hotspot carchomp < passphrase.txt`. It is never
  taken from the command line, where `ps` and shell history would keep it.
- `--gps DEVICE` has gpsd read a GPS receiver from boot: a USB one is usually
  `/dev/ttyACM0`, one on the GPIO header `/dev/serial0` (enable the serial
  port and disable the serial console in `raspi-config` first).
- `--radio` receives APRS on 144.39 MHz with an RTL-SDR dongle:
  `carchomp-radio.service` runs `rtl_fm` into Direwolf
  (`/etc/carchomp/direwolf.conf`, receive only: no PTT, no beacons), and
  `aprs_kiss` is pointed at Direwolf's KISS port 8001 unless already set.
  Reboot once so the kernel's DVB-T driver leaves the dongle alone.
- `--demo` runs a simulator in place of the GPS and radio (below); it cannot
  be combined with `--gps` or `--radio`.
- `--admin-net CIDR` also serves the web UI and the `.local` name to an IPv4
  network, /16 or narrower, e.g. `--admin-net 192.168.77.0/24` for home;
  repeat it for more (see *Who can use it*).
- `--reuse` runs again with the options of the last install, recorded in
  `/etc/carchomp/install.env` (never the hotspot passphrase), and leaves the
  hotspot as it is.

The clock comes from the GPS: chrony reads gpsd's time and steps the clock
whenever it is off, since a Pi has no battery-backed clock. Network time
servers are still used when there is a network.

`--gps` and `--radio` (and the chrony setup) have not been tested on real
hardware yet. They are kept simple and are safe to re-run; leaving `--radio`
out of a later run removes the radio service again.

### Who can use it

The web UI and the API are unauthenticated and whoever reaches them can
manage Wi-Fi and install software, so by default only this device and
clients on its hotspot (10.42.0.0/24) can. The car joins networks nobody
controls (eduroam, a cafe), so this is enforced three times over:

- carchompd listens on `127.0.0.1:80` and `10.42.0.1:80` only (`listen` in
  `/etc/carchomp/carchompd.toml`); the hotspot address is taken whenever the
  hotspot is up.
- A firewall (`carchomp-firewall.service`, table `inet carchomp` in
  `/etc/carchomp/firewall.nft`) drops all inbound traffic except: the
  hotspot's clients (web UI, SSH, DHCP, DNS, mDNS), SSH from anywhere, replies
  to the car's own connections, and the ICMP that networks need. The rules
  match addresses, not interfaces, since the one Wi-Fi radio is sometimes
  the hotspot and sometimes a client; a strict reverse-path check drops
  packets whose source address is not routed back out the interface they
  came in on, so a cafe neighbour cannot pose as a hotspot client.
  NetworkManager's own hotspot rules are left alone.
- carchompd answers anyone outside `trusted_networks` with 403.

`--admin-net CIDR` opens all three to another network: it is added to the
firewall (web UI and mDNS), to `trusted_networks`, and `listen` becomes
`["0.0.0.0:80"]`, since the car's address there comes from DHCP. It is
recorded in `/etc/carchomp/install.env`, so `--reuse` and updates keep it; a
run without it closes those networks again. An install from before the
firewall had no `--admin-net`: its first update takes the networks from
`trusted_networks` instead, so nobody who used the UI loses it.

The car trusts the address range, not the network: every network it joins
that uses the same range is an admin network too. `192.168.1.0/24`,
`192.168.0.0/24` and `10.0.0.0/24` are what most routers ship with,
including many cafe and hotel ones, and every client of such a network the
car has saved would get the web UI. Give your home router an unusual subnet
(e.g. `192.168.77.0/24`) and pass that. Networks wider than /16 are refused.

The reverse-path check needs the kernel's nftables fib support; the Raspberry
Pi and Debian kernels have it. Where it is missing, the firewall loads without
that one rule and logs a warning (`journalctl -u carchomp-firewall`) rather
than not at all.

SSH (port 22) stays reachable everywhere, so the installer turns passwords
off (`/etc/ssh/sshd_config.d/50-carchomp.conf`: keys only, no root login),
but only once a user has a key in `~/.ssh/authorized_keys`. Until then it
prints a warning and leaves passwords on rather than lock the owner out:
`ssh-copy-id` a key, then run the installer again or update.

The `.local` name (avahi) is announced only while the car is on its hotspot
or an admin network: a NetworkManager hook
(`/etc/NetworkManager/dispatcher.d/50-carchomp-avahi`) starts and stops
avahi as addresses come and go.

### Staying connected, eduroam

Network access is opportunistic: nothing needs it, but maps, updates and the
clock use it when it is there. Every Wi-Fi network the car knows is retried
forever (NetworkManager gives up after four failures by default), and
`carchomp-wifi.timer` checks every 30 seconds for a known network in range
before falling back to the hotspot. Once up, the hotspot stays up for at
least two minutes, and as long as a client is on it, before the car looks
for known networks again.

eduroam, or any WPA-Enterprise network, is joined from the Wi-Fi panel, and
never without checking the network's certificate, which is what keeps a fake
access point from collecting the password:

- Best: get your institution's `.eap-config` file from
  https://cat.eduroam.org (or the geteduroam app/site) and give it to the
  Wi-Fi panel. It names the institution's CA and server, and geteduroam can
  issue a device certificate (EAP-TLS), so no password is stored at all.
- Otherwise, enter the identity, the password and the RADIUS server's domain
  (your IT department publishes it); the certificate is checked against the
  system's CAs for that domain.
- Prefer a device or app password, where the institution offers one, to your
  main account password: it is stored on the car.

Enterprise networks saved by an older version without any certificate check
no longer join by themselves: the installer (and so an update) turns their
autoconnect off and names them. Forget each one in the Wi-Fi panel and join
it again as above.

The Pi has one Wi-Fi radio, so the car is either on a network or hosting its
hotspot, never both. A second USB Wi-Fi adapter (one for the hotspot, one
for the road) or an LTE modem would give both, and a connection between
Wi-Fi networks; neither is set up by the installer yet.

### Updates

A release bundle (`carchomp-<version>-<arch>.tar.gz`) can be installed from
the web UI: carchompd saves the upload and starts `carchomp-update.service`,
which checks the bundle, unpacks it and runs its `deploy/install.sh --reuse`.
Progress and the installer's last lines are in
`/var/lib/carchomp/update/status.json` and in the UI; the full output is in
`/var/lib/carchomp/update/update.log`. By hand, unpack the bundle and run
`sudo carchomp/deploy/install.sh --reuse`.

Bundles come from `tools/bundle.sh` (arm64 by default; see its header) or
from a release: every `v*` tag builds one, plus a ready-to-flash Raspberry Pi
OS image (`deploy/pi-gen/README.md`).

- An install from before web updates has no updater yet: run the bundle's
  installer by hand once, as above. `--reuse` works out its options.
- An update that adds a package (chrony came after the first installs) must
  reach the internet for apt. On the car's hotspot alone it stops before
  changing anything, and the old version keeps running.

### On a reTerminal, before the GPS and radio arrive

    sudo deploy/install.sh --kiosk "$USER" --hotspot carchomp --demo

`--demo` runs `tools/sim.py` as a service in place of the sensors; everything
else is the real thing, on the real device: PostgreSQL on the CM4, the
systemd units, the kiosk, the touch UI, Wi-Fi and the hotspot. The front
buttons work too: F1 APRS, F2 Tracks, F3 Maps, O back to the map and
following. Run the installer again without `--demo` (with `--gps` and
`--radio`) when the hardware is in; that deletes what the simulator recorded.

The reTerminal's screen and touch panel need Seeed's drivers on a stock
Raspberry Pi OS (`seeed-linux-dtoverlays`, see Seeed's reTerminal wiki); the
installer does not attempt that.

## Offline maps (development)

Add `maps_dir = "/some/dir"` to `dev.toml`, run
`tools/fetch_map_assets.sh /some/dir` once, and have `pmtiles`
(`brew install pmtiles`) on the PATH. Then use the Maps panel: move the map,
name the region, pick a detail level, download.

## APRS on the Pi

`install.sh --radio` sets this up. By hand: point `aprs_kiss` at Direwolf's
KISS port, with Direwolf fed by the SDR:
`rtl_fm -f 144.39M -s 24000 - | direwolf -c direwolf.conf -r 24000 -D 1 -`
(see `deploy/direwolf.conf`).
