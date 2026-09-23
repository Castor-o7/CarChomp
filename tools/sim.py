#!/usr/bin/env python3
"""Stand-in for the sensors: a gpsd server and a KISS TNC, from one script.

It drives a route over and over, parking in between so tracks end, and plays
a small APRS scene around the route's start: a moving car, a weather station,
a road closure that is later lifted, a brush fire, and bulletins. carchompd
cannot tell the difference, so everything downstream of the sensors can be
developed and demonstrated without them.

    sim.py                          the built-in city-block route, real time
    sim.py --gpx drive.gpx          replay a real drive, out and back
    sim.py --speedup 20 --once      one fast pass, then exit (for tests)

Built-in route: a block, park, then the block's first half again plus a
detour. So the first lap shows new road, the second known road then new road,
and every lap after that is all known.
"""
import argparse, json, math, socket, threading, time
import xml.etree.ElementTree as ET
from datetime import datetime, timedelta, timezone

EARTH = 111_320  # metres per degree of latitude
PARK = 400  # seconds parked between drives; longer than carchompd's track_idle
CRUISE = 13.0  # m/s, for GPX files without (usable) timestamps


def step(lat, lon, course, metres):
    lat2 = lat + metres * math.cos(math.radians(course)) / EARTH
    lon2 = lon + metres * math.sin(math.radians(course)) / (EARTH * math.cos(math.radians(lat)))
    return lat2, lon2


def block_route():
    """One (lat, lon) per second."""
    e, n, w, s = 90, 0, 270, 180
    lat, lon = 45.5122, -122.6587
    for course in [e, n, w, s, None, e, n, n, s, s, w, None]:
        for _ in range(PARK if course is None else 60):
            if course is not None:
                lat, lon = step(lat, lon, course, 15.0)
            yield lat, lon


def gpx_route(path):
    """The file's points resampled to one per second, there and back. Uses
    the points' <time> when every point has one, otherwise drives at CRUISE."""
    points = [p for p in ET.parse(path).getroot().iter() if p.tag.endswith("trkpt")]  # any GPX namespace
    if len(points) < 2:
        raise SystemExit(f"{path}: need at least two track points")
    track = [(float(p.get("lat")), float(p.get("lon"))) for p in points]
    stamps = [next((c.text for c in p if c.tag.endswith("time")), None) for p in points]
    out = []
    if all(stamps):
        t = [datetime.fromisoformat(s.strip().replace("Z", "+00:00")).timestamp() for s in stamps]
        if all(a <= b for a, b in zip(t, t[1:])) and t[-1] > t[0]:
            i = 0
            for k in range(int(t[-1] - t[0]) + 1):
                now = t[0] + k
                while t[i + 1] < now:
                    i += 1
                f = (now - t[i]) / (t[i + 1] - t[i]) if t[i + 1] > t[i] else 0
                (lat1, lon1), (lat2, lon2) = track[i], track[i + 1]
                out.append((lat1 + (lat2 - lat1) * f, lon1 + (lon2 - lon1) * f))
    if not out:
        for (lat1, lon1), (lat2, lon2) in zip(track, track[1:]):
            metres = math.hypot((lat2 - lat1) * EARTH, (lon2 - lon1) * EARTH * math.cos(math.radians(lat1)))
            seconds = max(1, round(metres / CRUISE))
            out += [(lat1 + (lat2 - lat1) * i / seconds, lon1 + (lon2 - lon1) * i / seconds) for i in range(seconds)]
    parked = [out[-1]] * PARK
    return out + parked + out[::-1] + [out[0]] * PARK


class Gps:
    """Walks the route once a (possibly shortened) second; clients wait on it."""

    def __init__(self, route, speedup, once):
        self.route, self.speedup, self.once = route, speedup, once
        self.tick = threading.Condition()
        self.report = None
        self.done = False
        self.listener = threading.Event()

    def run(self):
        # Sped up, the clock has to run ahead of the wall too, or the fixes
        # would all carry nearly the same time.
        self.listener.wait()  # don't drive off before anyone is watching
        now = datetime.now(timezone.utc)
        while not self.done:
            prev = None
            for lat, lon in self.route():
                now = datetime.now(timezone.utc) if self.speedup == 1 else now + timedelta(seconds=1)
                north, east = ((lat - prev[0]) * EARTH, (lon - prev[1]) * EARTH * math.cos(math.radians(lat))) if prev else (0, 0)
                speed = math.hypot(north, east)
                tpv = {"class": "TPV", "mode": 3, "time": now.isoformat(timespec="milliseconds").replace("+00:00", "Z"),
                       "lat": round(lat, 7), "lon": round(lon, 7), "altHAE": 15.0, "eph": 4.0, "speed": round(speed, 2)}
                if speed > 0.5:
                    tpv["track"] = round(math.degrees(math.atan2(east, north)) % 360, 1)
                prev = (lat, lon)
                with self.tick:
                    self.report = json.dumps(tpv) + "\n"
                    self.tick.notify_all()
                time.sleep(1 / self.speedup)
            self.done = self.once
        with self.tick:
            self.tick.notify_all()

    def serve(self, conn):
        with conn, conn.makefile("rw") as f:
            f.readline()  # ?WATCH=...
            f.write('{"class":"VERSION","release":"carchomp-sim"}\n')
            self.listener.set()
            while not self.done:
                with self.tick:
                    self.tick.wait()
                    f.write(self.report)
                f.flush()


def aprs_position(lat, lon, table="/", code=">"):
    def dm(value, width, hemispheres):
        degrees, minutes = divmod(abs(value) * 60, 60)
        return f"{int(degrees):0{width}d}{minutes:05.2f}{hemispheres[value < 0]}"
    return f"{dm(lat, 2, 'NS')}{table}{dm(lon, 3, 'EW')}{code}"


def scene(lat, lon):
    """(sender, path, info), placed around the route's start. A trailing * on
    a path entry marks a digipeater that relayed the packet."""
    at = lambda north, east, *symbol: aprs_position(*step(*step(lat, lon, 0, north), 90, east), *symbol)
    stamp = "171800z"
    for i in range(6):
        yield "W7CAR-9", ["WIDE1-1"], f"!{at(900, -1500 + 500 * i)}090/025 Heading east"
        if i == 0:
            yield "K7WX", ["WIDE2-1"], f"@{stamp}{at(-400, 1300, '/', '_')}220/004g010t068r000p012h55b10132 Davis VP2"
            yield "ODOT-1", ["WIDE2-1*"], f";HWY26 CLS*{stamp}{at(-1100, -1600, '/', chr(39))}Closed at the tunnel: crash"
            yield "PFR-1", ["WIDE1-1*", "WIDE2-1"], f")FIRE!{at(3100, 4500, '/', ':')} Brush fire, avoid area"
            yield "NWSPQR", ["WIDE2-1*"], ":NWS-WARN :Red flag warning until 8PM PDT for the Willamette Valley{a1"
            yield "K7BLN", ["WIDE2-1"], ":BLN1     :ARES net tonight 7pm on 146.84"
            yield "N0CALL", [], ":W7CAR-9  :private chatter is not a bulletin{7"
    yield "ODOT-1", ["WIDE2-1*"], f";HWY26 CLS_{stamp}{at(-1100, -1600, '/', chr(39))}"


def kiss_frame(sender, path, info):
    def address(call, last):
        name, _, ssid = call.rstrip("*").partition("-")
        flags = 0x60 | (int(ssid or 0) << 1) | (0x80 if call.endswith("*") else 0) | (1 if last else 0)
        return bytes(ord(c) << 1 for c in name.ljust(6)) + bytes([flags])

    calls = ["APRS", sender, *path]
    frame = b"".join(address(c, i == len(calls) - 1) for i, c in enumerate(calls)) + b"\x03\xf0" + info.encode()
    return b"\xc0\x00" + frame.replace(b"\xdb", b"\xdb\xdd").replace(b"\xc0", b"\xdb\xdc") + b"\xc0"


def serve_kiss(conn, start, pause, gps):
    with conn:
        while not gps.done:
            for packet in scene(*start):
                conn.sendall(kiss_frame(*packet))
                time.sleep(pause)


def listen(port, handler):
    server = socket.create_server(("127.0.0.1", port))

    def accept():
        while True:
            conn, _ = server.accept()
            threading.Thread(target=lambda: quietly(handler, conn), daemon=True).start()

    threading.Thread(target=accept, daemon=True).start()


def quietly(handler, conn):
    try:
        handler(conn)
    except OSError:
        pass  # the client went away


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--gpsd", type=int, default=2947, metavar="PORT")
    parser.add_argument("--kiss", type=int, default=8001, metavar="PORT")
    parser.add_argument("--gpx", metavar="FILE", help="replay this track instead of the built-in route")
    parser.add_argument("--speedup", type=float, default=1, help="run this many times faster than real time")
    parser.add_argument("--once", action="store_true", help="exit after one pass over the route")
    args = parser.parse_args()

    replay = gpx_route(args.gpx) if args.gpx else None
    route = (lambda: iter(replay)) if replay else block_route
    gps = Gps(route, args.speedup, args.once)
    start = next(route())
    listen(args.gpsd, gps.serve)
    listen(args.kiss, lambda conn: serve_kiss(conn, start, 10 / args.speedup, gps))
    print(f"carchomp-sim: gpsd on :{args.gpsd}, KISS on :{args.kiss}, {args.speedup:g}x", flush=True)
    gps.run()
