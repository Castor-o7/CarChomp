// What CarChomp draws over the basemap. Kept free of the browser so that
// check-style.js can validate it in CI: a bad expression passes the build and
// only fails once MapLibre sees it.
import { TRACK_COLORS } from './palette.js';

// Incidents (objects) stand out; weather is calm; everything else is a station.
const APRS_COLOR = ['match', ['get', 'kind'], 'object', '#9333ea', 'weather', '#0ea5e9', '#dc2626'];
const TRACK_COLOR = ['to-color', ['at', ['%', ['get', 'track'], TRACK_COLORS.length], ['literal', TRACK_COLORS]]];
const line = { 'line-cap': 'round', 'line-join': 'round' };

export const OVERLAY_SOURCES = ['tracks', 'trail', 'aprs'];

export const OVERLAY_LAYERS = [
  // Translucent, so roads driven often grow darker than roads driven once.
  { id: 'tracks', type: 'line', source: 'tracks', layout: line, paint: { 'line-color': TRACK_COLOR, 'line-width': 5, 'line-opacity': 0.45 } },
  { id: 'trail', type: 'line', source: 'trail', layout: line, paint: { 'line-color': '#f97316', 'line-width': 5 } },
  { id: 'aprs', type: 'circle', source: 'aprs', paint: { 'circle-radius': 9, 'circle-color': APRS_COLOR, 'circle-stroke-color': '#fff', 'circle-stroke-width': 2 } },
];
