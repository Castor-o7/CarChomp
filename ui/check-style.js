// Fails if MapLibre would reject the map's overlay layers.
import { validateStyleMin } from '@maplibre/maplibre-gl-style-spec';
import { OVERLAY_LAYERS, OVERLAY_SOURCES } from './src/overlays.js';

const empty = { type: 'geojson', data: { type: 'FeatureCollection', features: [] } };
const errors = validateStyleMin({
  version: 8,
  sources: Object.fromEntries(OVERLAY_SOURCES.map((id) => [id, empty])),
  layers: OVERLAY_LAYERS,
});

for (const { message } of errors) console.error(message);
if (errors.length) process.exit(1);
console.log(`${OVERLAY_LAYERS.length} overlay layers valid`);
