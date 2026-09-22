# CarChomp

*Take a chomp out of your drive.*

A Raspberry Pi car computer that tracks your drives and tells you when you're
on a road you've never driven. A from-scratch take on the carputer capstone,
built from the stakeholder's original directive ([stakeholder.md](stakeholder.md)). Design and roadmap:
[DESIGN.md](DESIGN.md).

    crates/carputer-core   pure logic: gpsd, SmartBeaconing, KISS, APRS  (no I/O)
    crates/carputerd       the daemon: sources -> bus -> PostGIS, /ws, /api
    ui                     one Svelte + MapLibre app for the touchscreen and the browser
    deploy                 install script, systemd unit, production config
    tools/sim.py           stand-in for the sensors: pretend gpsd + APRS TNC
    tools/fetch_map_assets.sh   fonts + sprites for the offline map

## Try it

    docker compose -f compose.dev.yml up -d --wait       # PostGIS on :5433
    (cd ui && npm install && npm run build)              # UI -> ui/dist
    python3 tools/sim.py --gpsd 12947 --kiss 18001 &     # pretend GPS + APRS radio
    cat > dev.toml <<'END'
    database_url = "postgres://carputer:carputer@localhost:5433/carputer"
    gpsd = "127.0.0.1:12947"
    aprs_kiss = "127.0.0.1:18001"
    ui_dir = "ui/dist"
    END
    cargo run -p carputerd -- dev.toml                   # then open http://localhost:8000

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

On Raspberry Pi OS or Debian (bookworm or newer), from a checkout:

    sudo deploy/install.sh --kiosk pi --hotspot carputer 'a passphrase'

All options are optional. It installs PostgreSQL/PostGIS, gpsd, the `pmtiles`
tool, builds `carputerd` and the UI if the bundle has no prebuilt ones, creates
the database, and starts `carputerd.service` on port 80. Run it again to
upgrade; data and `/etc/carputer/carputerd.toml` are kept.

### On a reTerminal, before the GPS and radio arrive

    sudo deploy/install.sh --kiosk pi --hotspot carputer 'a passphrase' --demo

`--demo` runs `tools/sim.py` as a service in place of the sensors; everything
else is the real thing, on the real device: PostgreSQL on the CM4, the
systemd units, the kiosk, the touch UI, Wi-Fi and the hotspot. The front
buttons work too: F1 APRS, F2 Tracks, F3 Maps, O back to the map and
following. Run the installer again without `--demo` when the hardware is in.

The reTerminal's screen and touch panel need Seeed's drivers on a stock
Raspberry Pi OS (`seeed-linux-dtoverlays`, see Seeed's reTerminal wiki); the
installer does not attempt that.

## Offline maps (development)

Add `maps_dir = "/some/dir"` to `dev.toml`, run
`tools/fetch_map_assets.sh /some/dir` once, and have `pmtiles`
(`brew install pmtiles`) on the PATH. Then use the Maps panel: move the map,
name the region, pick a detail level, download.

## APRS on the Pi

Point `aprs_kiss` at Direwolf's KISS port, with Direwolf fed by the SDR:
`rtl_fm -f 144.39M - | direwolf -r 24000 -D 1 -`.
