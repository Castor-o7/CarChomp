# Roadmap

Stories for the [Muster board](https://github.com/Castor-o7/muster/blob/main/docs/board.md).
IDs are permanent: don't reuse or renumber them, even when a story is dropped.
Name the story in each commit or PR it covers, as a `Story: CHOMP-1` trailer.

A box is ticked only when someone has checked the story's "done when" line.
Code existing is not enough, and a story that needs hardware isn't done until
it has run on that hardware.

**Progress: 8 of 17 ticked.** Ticks here mean checked on simulators or in CI (for
CHOMP-13, by reading this file). None has run on real hardware. Still open: the six that need hardware
(CHOMP-3, 5, 7, 8, 10, 12), and CHOMP-6, 9 and 14, which don't.

## Stories from DESIGN.md's order of work

- [x] **CHOMP-1: Core types, beaconing, KISS and APRS parsing.** Done when:
  `cargo test -p carchomp-core` passes in CI.
- [x] **CHOMP-2: gpsd to database, `/ws`, `/api/tracks`.** Done when: a
  `tools/sim.py` drive creates a track in PostGIS, returned by `/api/tracks` and
  streamed live on `/ws`.
- [ ] **CHOMP-3: New-road detection, APRS source.** Done when: repeating a sim
  route clears the new-road flag, and a real TNC stores a position, weather
  report, object and bulletin. Needs hardware.
- [x] **CHOMP-4: Map UI and track manager.** Done when: in a browser, a sim drive
  shows live position, track overlay and new-road cue, and a track can be managed
  from the UI.
- [ ] **CHOMP-5: Import/export, health, web updates.** Done when: a GPX file
  round-trips through import and export, `/api/health` responds, and a
  `tools/bundle.sh` bundle uploaded on the reTerminal updates it and the daemon
  comes back. Needs hardware.
- [ ] **CHOMP-6: Offline maps.** Done when: a region downloads and draws with the
  network off, and cancel and delete leave no partial files.
- [ ] **CHOMP-7: Wi-Fi and hotspot fallback.** Done when: on the reTerminal, scan,
  join and forget work, and the hotspot comes up when no known network is in
  range. Needs hardware.
- [ ] **CHOMP-8: Installer, kiosk, release bundle, image, CI.** Done when: a `v*`
  tag produces a bundle and image, and that image boots on the reTerminal into the
  kiosk with `carchompd` running. Needs hardware.
- [ ] **CHOMP-9: Heading-aware matching, per-track colours.** Done when: an old
  track that only crosses the current road does not count as having used it, the
  same road driven in either direction does, and different tracks draw in
  different colours.
- [ ] **CHOMP-10: GPS time through chrony.** Done when: with no RTC or network,
  `chronyc sources` shows gpsd selected and the clock is correct. Needs hardware.
- [x] **CHOMP-11: Network access rule.** Done when: localhost and hotspot requests
  get full access, others are refused, and a test covers each case.
- [ ] **CHOMP-12: `--gps` and `--radio` installer options.** Done when: with a real
  receiver and dongle, a fresh install records GPS fixes and live APRS packets.
  Needs hardware.

## Found along the way

- [x] **CHOMP-13: A roadmap the board can track.** Done when: this file gives every
  item in DESIGN.md's order of work a permanent ID and a "done when" line.
- [ ] **CHOMP-14: Clear the Dependabot backlog.** Done when: no Dependabot PR opened
  by 2026-10-30 is still open, each merged with passing CI or closed with a reason,
  and every open Dependabot alert has a recorded decision.
- [x] **CHOMP-15: CI catches an invalid map style.** Done when: a `line-color`
  expression MapLibre rejects, like the one first written for CHOMP-9, fails CI.
- [x] **CHOMP-16: A repo test for the known-road query.** Done when: a test that
  CI runs against PostGIS exercises `KNOWN_ROAD` and fails if the same road in
  either direction stops counting as known, or a crossing beyond `road_heading`
  starts to.
- [x] **CHOMP-17: No sprite 404 on a README dev run.** Done when: following the
  README's "Try it" steps gives a map with no 404 for `sprites/v4/light`, or those
  steps say that the 404 is expected and how to clear it.
