<script>
  import { layers, namedFlavor } from '@protomaps/basemaps';
  import maplibregl from 'maplibre-gl';
  import 'maplibre-gl/dist/maplibre-gl.css';
  import { PMTiles, Protocol } from 'pmtiles';
  import { onMount } from 'svelte';
  import { describe } from './aprs.js';
  import { api, live, ui } from './state.svelte.js';

  const EMPTY = { type: 'FeatureCollection', features: [] };
  // Incidents (objects) stand out; weather is calm; everything else is a station.
  const APRS_COLOR = ['match', ['get', 'kind'], 'object', '#9333ea', 'weather', '#0ea5e9', '#dc2626'];

  maplibregl.addProtocol('pmtiles', new Protocol().tile);

  // Online OpenStreetMap tiles are always the bottom layer. Offline regions
  // are drawn over them, least detailed first, so wherever a region has been
  // downloaded the network is not needed, and elsewhere it is used if there.
  async function buildStyle() {
    const { archives } = await api('maps').catch(() => ({ archives: [] }));
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
        ...Object.fromEntries(usable.map((r) => [r.name, { type: 'vector', url: `pmtiles://${r.url}` }])),
      },
      layers: [
        { id: 'osm', type: 'raster', source: 'osm' },
        ...usable.flatMap((r) =>
          layers(r.name, flavor, { lang: 'en' })
            .filter((layer) => layer.type !== 'background')
            .map((layer) => ({ ...layer, id: `${r.name}/${layer.id}` })),
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
    map = new maplibregl.Map({ container, style: blank, center: [-122.66, 45.51], zoom: 13, attributionControl: { compact: true } });

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
      if (ready) loadTracks();
    };
    map.on('moveend', moved);
    moved();
    map.on('dragstart', () => (ui.follow = false));
    map.on('click', 'aprs', (e) => {
      const lines = describe(e.features[0].properties, ui.metric);
      new maplibregl.Popup({ closeButton: false }).setLngLat(e.lngLat).setText(lines.join('\n')).addTo(map);
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

  async function loadTracks() {
    const bbox = ui.bounds.map((n) => n.toFixed(5));
    map.getSource('tracks')?.setData(await api(`segments?bbox=${bbox}`).catch(() => EMPTY));
  }

  $effect(() => {
    ui.tracksChanged;
    if (ready) loadTracks();
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
  $effect(() => {
    const right = ui.panel && innerWidth > 700 ? 440 : 0;
    map.easeTo({ padding: { right }, duration: 200 });
  });

  $effect(() => {
    const line = ui.focus?.geometry?.coordinates;
    if (!ready || !line?.length) return;
    const bounds = line.reduce((b, c) => b.extend(c), new maplibregl.LngLatBounds(line[0], line[0]));
    ui.follow = false;
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
