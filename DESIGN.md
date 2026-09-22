# CarChomp: the carputer, reimagined

Source of truth for requirements: [stakeholder.md](stakeholder.md).

## Principles

1. **One daemon, one database, one UI.** A single Rust binary (`carchompd`) plus
   PostgreSQL/PostGIS. No per-team services, no second API, no logic in stored
   procedures.
2. **Everything is an observation from a source.** GPS is just the first
   source. APRS over RTL-SDR is the second. RF geolocation and stereo dash
   cameras are later ones. Adding a source must not touch the recorder, the
   API, or the schema of existing sources.
3. **Store little, store it well.** Smart beaconing decides which fixes are
   worth keeping. Bulk data (video) lives on disk; the database only indexes it.
4. **Pure logic is separate from I/O** so it can be tested without hardware.

## Shape

```
  gpsd ──────┐
  direwolf ──┤ (KISS/TCP, fed by rtl_fm; receive only)
  rf-geoloc ─┤ (future)            ┌─► recorder ─► PostgreSQL/PostGIS
  cameras ───┘ (future)            │
        Source tasks ──► bus ──────┼─► new-road detector ─► bus
                     (broadcast)   └─► WebSocket hub ─► UI (kiosk + remote)
                                       REST (axum) ◄──► PostgreSQL
```

- `carchomp-core` — no I/O, no async. gpsd JSON types, KISS framing, APRS
  position parsing, the smart-beaconing filter, the `Observation` type.
- `carchompd` — tokio + axum + sqlx. Source tasks, bus, recorder, HTTP/WS,
  static UI serving. Each source is a module enabled by config; a source that
  fails to connect retries forever and never takes the daemon down.
- `ui/` — one Svelte app; kiosk and remote are layouts, not separate projects.

## Smart beaconing as the storage policy

The APRS SmartBeaconing algorithm (HamHUD) decides when a moving station is
worth reporting: slowly when stopped, fast when moving, and immediately when
turning ("corner pegging"). We apply it to *storage* instead of transmission:
gpsd delivers 1 Hz fixes, the UI sees all of them live, but only the fixes the
algorithm selects are written. A straight highway costs one row every ~½ mile;
a switchback keeps every bend. Tracks stay geometrically faithful at a small
fraction of the rows.

## Data model

```
source        id, kind ('gps' | 'aprs' | 'rf' | 'camera'), name, config jsonb
track         id, name, started, ended, visible
fix           time, source_id, track_id, geom Point, alt, speed, course, h_err
track_segment track_id, geom LineString          -- ≤32-vertex pieces, GiST
aprs_packet   time, source_id, callsign, direct, kind, name, alive, geom Point NULL,
              symbol, speed, course, weather jsonb, addressee, text, raw
media         (future) source_id, span tstzrange, path, bytes, meta jsonb
```

- `fix` carries `source_id` and `h_err` (horizontal error, metres) so several
  positioning methods can coexist and be fused or compared later. A GPS fix and
  an RF-derived fix are the same row shape with different uncertainty.
- **"Have I been here?"** = `ST_DWithin(track_segment.geom, here, r)` excluding
  the current track. Tracks are cut into short segments because one long
  LineString has a bounding box the size of the whole drive, which makes a
  spatial index useless. The same query powers "relevant tracks" on the map.
- A track only counts as *using* the current road if, where it is nearest, it
  runs the way we are heading (either direction, within `road_heading`
  degrees). Crossing an old track at an intersection is not "been here". No
  road-network data is needed for any of this.
- APRS is receive-only and is there for situational awareness: positions,
  weather reports, objects/items (road closures, fires, hazards: a *named
  thing at a place* that its sender can later withdraw) and bulletins /
  weather-service messages. What is on the map (`aprs_station`) and what is
  an alert (`aprs_bulletin`) are views over the one packet log, not tables to
  keep in sync. Every packet keeps its full TNC2 text for re-parsing.
- No partitioning: a year of daily driving is ~10⁵–10⁶ rows. BRIN on `time`,
  GiST on geometry. Revisit only if a high-rate source demands it.
- Camera data never enters PostgreSQL. `media` rows point at files and are
  joined to `fix` by time.

## API (small on purpose)

```
GET  /ws                          live; see below
GET  /api/health                  version, counts, database size
GET  /api/tracks[?near=lon,lat,m] list, optionally only tracks passing nearby
GET  /api/tracks/:id              GeoJSON Feature
GET  /api/tracks/:id/gpx          GPX download
POST /api/tracks/import           body is a GPX or GeoJSON document
PATCH  /api/tracks/:id            {"name": ..., "visible": ...}
DELETE /api/tracks/:id
GET  /api/segments?bbox=w,s,e,n   visible track geometry in a map viewport
GET  /api/aprs/stations[?minutes] stations + live objects, GeoJSON
GET  /api/aprs/bulletins[?minutes] bulletins and weather-service alerts
GET  /api/maps                    offline regions + running download
POST /api/maps                    {"name", "bbox", "maxzoom"[, "source"]}
DELETE /api/maps/:name
GET  /maps/*                      the archives themselves (range requests)
GET  /api/wifi                    networks in range + saved profiles
POST /api/wifi                    {"ssid"[, "password"]}
DELETE /api/wifi/:ssid            forget
```

The WebSocket carries two kinds of message. *Events* are the raw observation
stream, `{"source": 1, "obs": {"type": "fix" | "aprs", ...}}`, at whatever
rate the source produces. *Status* is recorder state, `{"status": {"track":
7, "road_new": true}}`, sent on connect and on every change, so a client that
joins mid-drive is immediately correct.

## Order of work

1. ~~core: gpsd types, smart beaconing, KISS + APRS parsing~~
2. ~~daemon: gpsd source → bus → recorder → PostgreSQL; `/ws`, `/api/tracks`~~
3. ~~new-road detection; APRS source: positions, weather, objects/items,
   bulletins~~ (tested against `tools/sim.py`, not yet a real TNC)
4. ~~UI: map, live position, track overlay, new-road indicator, track manager~~
5. ~~import/export, system health~~; system updates from the web UI
6. ~~offline maps: PMTiles regions served by the daemon, download manager~~
7. ~~Wi-Fi: scan / join / forget; hotspot fallback as a NetworkManager profile~~
   (untested on hardware)
8. ~~install script, systemd unit, kiosk autostart~~; pi-gen stage, image CI
9. ~~heading-aware "uses this road" matching~~; per-track colours
10. GPS-disciplined time (chrony + gpsd) for a Pi with no RTC and no network

## Position from RF (stretch goal)

One receiver behind a KISS TNC reports neither signal strength nor bearing,
so true triangulation is out of reach for now. What *is* available: a packet
that no digipeater relayed was heard from the sender itself, so that sender is
within radio range. `aprs_packet.direct` records this for every packet. A
weighted centroid of recently, directly heard stations with known positions
is a coarse fix (kilometres, not metres), and fits the existing model as one
more `source` writing `fix` rows with a large `h_err`. With an SDR source
that reports signal strength, the same rows gain a weight and the estimate
tightens. Nothing to build until then, but the data is already being kept.

## Shelling out

Two jobs are delegated to existing tools rather than reimplemented, each
behind one small module: cutting map regions out of a remote planet archive
(`pmtiles extract`) and talking to NetworkManager (`nmcli`). Arguments are
always passed as an argument vector, never through a shell, and anything that
becomes a file name or could be read as an option is validated first.

## Offline maps

A region is one `.pmtiles` file in `maps_dir`. The browser reads tiles out of
it with HTTP range requests, so the daemon only serves static files. The map
style always has online OpenStreetMap raster tiles at the bottom and draws
downloaded regions over them, least detailed first: where a region exists the
network is not needed; elsewhere it is used if present. Fonts and sprites
live in `maps_dir/assets` (`tools/fetch_map_assets.sh`).
