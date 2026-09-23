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

The web UI and the API answer only this device itself and clients on its
hotspot (10.42.0.0/24); anyone else gets 403. Whoever can reach the UI can
manage Wi-Fi and install software, so widen this with care: add networks to
`trusted_networks` in `/etc/carchomp/carchompd.toml`, e.g.
`trusted_networks = ["10.42.0.0/24", "192.168.1.0/24"]`, and restart
`carchompd`.

### Updates

A release bundle (`carchomp-<version>-<arch>.tar.gz`) can be installed from
the web UI: carchompd saves the upload and starts `carchomp-update.service`,
which checks the bundle, unpacks it and runs its `deploy/install.sh --reuse`.
Progress and the installer's last lines are in
`/var/lib/carchomp/update/status.json` and in the UI; the full output is in
`/var/lib/carchomp/update/update.log`. By hand, unpack the bundle and run
`sudo carchomp/deploy/install.sh --reuse`.

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
