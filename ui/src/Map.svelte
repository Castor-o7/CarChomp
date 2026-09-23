<script>
  import { layers, namedFlavor } from '@protomaps/basemaps';
  import maplibregl from 'maplibre-gl';
  import 'maplibre-gl/dist/maplibre-gl.css';
  import { PMTiles, Protocol } from 'pmtiles';
  import { onMount, untrack } from 'svelte';
  import { describe } from './aprs.js';
  import { api, live, ui } from './state.svelte.js';

  const EMPTY = { type: 'FeatureCollection', features: [] };
  // Incidents (objects) stand out; weather is calm; everything else is a station.
  const APRS_COLOR = ['match', ['get', 'kind'], 'object', '#9333ea', 'weather', '#0ea5e9', '#dc2626'];

  // Keeps one reader per archive URL; a region downloaded again under the
  // same name has the same URL, so readers are dropped on every rebuild.
  const protocol = new Protocol();
  maplibregl.addProtocol('pmtiles', protocol.tile);

  // Online OpenStreetMap tiles are always the bottom layer. Offline regions
  // are drawn over them, least detailed first, so wherever a region has been
  // downloaded the network is not needed, and elsewhere it is used if there.
  // Coarse regions step aside a little past their detail, so they do not
  // cover the online streets at street zoom.
  async function buildStyle() {
    const { archives } = await api('maps').catch(() => ({ archives: [] }));
    protocol.tiles.clear();
    const regions = await Promise.all(
      archives.map(async ({ name }) => {
        const url = `${location.origin}/maps/${name}.pmtiles`;
        const header = await new PMTiles(url).getHeader().catch(() => null);
        return header && { name, url, maxZoom: header.maxZoom };
      }),
    );
    const usable = regions.filter(Boolean).sort((a, b) => a.maxZoom - b.maxZoom);
    const flavor = namedFlavor('light');
    return {
      version: 8,
      glyphs: `${location.origin}/maps/assets/fonts/{fontstack}/{range}.pbf`,
      sprite: `${location.origin}/maps/assets/sprites/v4/light`,
      sources: {
        osm: { type: 'raster', tiles: ['https://tile.openstreetmap.org/{z}/{x}/{y}.png'], tileSize: 256, maxzoom: 19, attribution: '© OpenStreetMap contributors' },
        ...Object.fromEntries(usable.map((r) => [`region:${r.name}`, { type: 'vector', url: `pmtiles://${r.url}` }])),
      },
      layers: [
        { id: 'osm', type: 'raster', source: 'osm' },
        ...usable.flatMap((r) =>
          layers(`region:${r.name}`, flavor, { lang: 'en' })
            .filter((layer) => layer.type !== 'background')
            .map((layer) => ({
              ...layer,
              id: `${r.name}/${layer.id}`,
              ...(r.maxZoom < 15 && { maxzoom: Math.min(layer.maxzoom ?? 24, r.maxZoom + 3) }),
            })),
        ),
      ],
    };
  }

  let container;
  let map;
  let marker;
  let ready = $state(false);

  onMount(() => {
    const blank = { version: 8, sources: {}, layers: [] };
    map = new maplibregl.Map({ container, style: blank, center: [-122.66, 45.51], zoom: 13, attributionControl: { compact: true }, dragRotate: false, touchPitch: false, pitchWithRotate: false });
    map.touchZoomRotate.disableRotation(); // a stray second finger must not leave north lost

    const arrow = document.createElement('div');
    arrow.innerHTML =
      '<svg width="36" height="36" viewBox="0 0 24 24"><path d="M12 2 21 22 12 17 3 22Z" fill="#2563eb" stroke="#fff" stroke-width="1.5" stroke-linejoin="round"/></svg>';
    marker = new maplibregl.Marker({ element: arrow, rotationAlignment: 'map' });

    // Runs again after every basemap change, since a new style starts empty.
    map.on('style.load', () => {
      if (!mapStyle) return;
      for (const id of ['tracks', 'trail', 'aprs']) map.addSource(id, { type: 'geojson', data: EMPTY });
      const line = { 'line-cap': 'round', 'line-join': 'round' };
      // Translucent, so roads driven often grow darker than roads driven once.
      map.addLayer({ id: 'tracks', type: 'line', source: 'tracks', layout: line, paint: { 'line-color': '#2563eb', 'line-width': 5, 'line-opacity': 0.45 } });
      map.addLayer({ id: 'trail', type: 'line', source: 'trail', layout: line, paint: { 'line-color': '#f97316', 'line-width': 5 } });
      map.addLayer({ id: 'aprs', type: 'circle', source: 'aprs', paint: { 'circle-radius': 9, 'circle-color': APRS_COLOR, 'circle-stroke-color': '#fff', 'circle-stroke-width': 2 } });
      ready = true;
    });

    const moved = () => {
      const b = map.getBounds();
      ui.bounds = [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()];
      if (ready) loadTracks(false);
    };
    map.on('moveend', moved);
    moved();
    map.on('dragstart', () => (ui.follow = false));
    map.on('click', 'aprs', (e) => {
      const content = document.createElement('div');
      for (const line of describe(e.features[0].properties, ui.metric)) content.appendChild(document.createElement('div')).textContent = line;
      new maplibregl.Popup({ closeButton: false }).setLngLat(e.lngLat).setDOMContent(content).addTo(map);
    });
    return () => map.remove();
  });

  let mapStyle;
  $effect(() => {
    ui.mapsChanged;
    buildStyle().then((built) => {
      ready = false; // the effects below re-run once the new style is in
      mapStyle = built;
      map.setStyle(built, { diff: false });
    });
  });

  // Fetches a padded box so small moves (Follow eases every fix) reuse it,
  // or the one still on its way; an answer older than the one drawn is dropped.
  let fetched = null;
  let pending = null;
  let requests = 0;
  let drawn = 0;
  const inside = (b, [w, s, e, n]) => b && w >= b[0] && s >= b[1] && e <= b[2] && n <= b[3];
  async function loadTracks(force) {
    if (!force && (inside(fetched, ui.bounds) || inside(pending, ui.bounds))) return;
    const [w, s, e, n] = ui.bounds;
    const dx = (e - w) / 2;
    const dy = (n - s) / 2;
    const box = [w - dx, s - dy, e + dx, n + dy];
    const request = ++requests;
    pending = box;
    const data = await api(`segments?bbox=${box.map((x) => x.toFixed(5))}`).catch(() => null);
    if (pending === box) pending = null;
    if (request < drawn) return;
    drawn = request;
    fetched = data && box;
    map.getSource('tracks')?.setData(data ?? EMPTY);
  }

  $effect(() => {
    ui.tracksChanged;
    // untrack: loadTracks reads ui.bounds, and moved() already reloads on every move.
    if (ready) untrack(() => loadTracks(true));
  });

  $effect(() => {
    const fix = live.fix;
    if (!ready || !fix) return;
    marker.setLngLat([fix.lon, fix.lat]).setRotation(fix.course ?? 0).addTo(map);
    if (ui.follow) map.easeTo({ center: [fix.lon, fix.lat], duration: 900, easing: (t) => t });
  });

  $effect(() => {
    const coordinates = $state.snapshot(live.trail);
    if (!ready) return;
    const data = coordinates.length > 1 ? { type: 'Feature', geometry: { type: 'LineString', coordinates } } : EMPTY;
    map.getSource('trail').setData(data);
  });

  // Stations heard before the page loaded come from the database; after
  // that, live packets keep the layer current.
  let stored = $state([]);
  $effect(() => {
    if (ui.aprs) api('aprs/stations').then((c) => (stored = c.features), () => {});
  });
  $effect(() => {
    if (!ready) return;
    const heard = $state.snapshot(live.stations);
    const features = ui.aprs ? [...stored.filter((f) => !(f.properties.name in heard)), ...Object.values(heard).filter(Boolean)] : [];
    map.getSource('aprs').setData({ type: 'FeatureCollection', features });
  });

  // An open side panel covers part of the map; centre things in what is left.
  // At once, not eased: Follow's next ease would stop a padding ease halfway.
  $effect(() => {
    const right = ui.panel && innerWidth > 700 ? 440 : 0;
    map.setPadding({ right });
  });

  // Zoom to a feature once, when asked; style rebuilds must not repeat it.
  $effect(() => {
    const line = ui.focus?.geometry?.coordinates;
    if (!ready || !line?.length) return;
    const bounds = line.reduce((b, c) => b.extend(c), new maplibregl.LngLatBounds(line[0], line[0]));
    ui.follow = false;
    ui.focus = null;
    map.fitBounds(bounds, { padding: 60, maxZoom: 16 });
  });
</script>

<div class="map" bind:this={container}></div>

<style>
  .map {
    position: absolute;
    inset: 0;
  }
</style>
