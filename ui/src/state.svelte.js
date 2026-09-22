// All shared state, and the one WebSocket that feeds it.

/** What the daemon is telling us right now. */
export const live = $state({
  connected: false,
  fix: null, // latest position, stored or not
  road: null, // true = never driven, false = known, null = unknown
  track: null, // id of the track being recorded
  trail: [], // [lon, lat] of the current drive
  stations: {}, // name -> GeoJSON Feature (null once withdrawn), heard since page load
  bulletins: 0, // bumped when a message arrives, so lists know to refresh
});

/** What the person in front of the screen has chosen. */
export const ui = $state({
  panel: null, // 'tracks' | 'aprs' | 'maps' | 'system' | null
  follow: true,
  aprs: localStorage.aprs === 'true',
  metric: localStorage.metric === 'true',
  tracksChanged: 0, // bumped whenever stored tracks change
  mapsChanged: 0, // bumped whenever offline regions change
  bounds: [0, 0, 0, 0], // what the map shows: west, south, east, north
  focus: null, // a GeoJSON Feature the map should zoom to
});

export async function api(path, options) {
  const response = await fetch(`/api/${path}`, options);
  if (!response.ok) throw new Error((await response.text()) || response.statusText);
  return response.headers.get('content-type')?.includes('json') ? response.json() : null;
}

const MAX_TRAIL = 20_000;

function handle(msg) {
  if (msg.status) {
    const { track, road_new } = msg.status;
    live.road = road_new;
    if (track === live.track) return;
    if (live.track !== null) ui.tracksChanged++; // the finished drive is now a stored track
    live.track = track;
    live.trail = [];
    if (track !== null) {
      // Joining a drive in progress: start from what has been stored of it.
      api(`tracks/${track}`).then((f) => (live.trail = [...(f.geometry?.coordinates ?? []), ...live.trail]), () => {});
    }
  } else if (msg.obs.type === 'fix') {
    live.fix = msg.obs;
    if (live.track !== null && live.trail.length < MAX_TRAIL) {
      live.trail.push([msg.obs.lon, msg.obs.lat]);
    }
  } else if (msg.obs.type === 'aprs') {
    const { position, ...packet } = msg.obs;
    if (packet.kind === 'message') live.bulletins++;
    if (!position) return;
    // Same shape as /api/aprs/stations, so the map treats both alike.
    live.stations[packet.name] = packet.alive
      ? {
          type: 'Feature',
          geometry: { type: 'Point', coordinates: [position.lon, position.lat] },
          properties: { ...packet, ...position, callsign: packet.from, time: new Date().toISOString() },
        }
      : null;
  }
}

function connect() {
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  const socket = new WebSocket(`${scheme}://${location.host}/ws`);
  socket.onopen = () => (live.connected = true);
  socket.onmessage = (event) => handle(JSON.parse(event.data));
  socket.onclose = () => {
    live.connected = false;
    setTimeout(connect, 2000);
  };
}

connect();
