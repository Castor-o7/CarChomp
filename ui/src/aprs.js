// Turning APRS packets into words.

const COMPASS = ['N', 'NE', 'E', 'SE', 'S', 'SW', 'W', 'NW'];

/** One-line weather summary in the user's units. */
export function weatherLine(w, metric) {
  const speed = (ms) => (metric ? `${Math.round(ms * 3.6)} km/h` : `${Math.round(ms * 2.23694)} mph`);
  return [
    w.temp_c != null && (metric ? `${Math.round(w.temp_c)} °C` : `${Math.round(w.temp_c * 1.8 + 32)} °F`),
    w.wind_speed != null && `wind ${w.wind_dir != null ? COMPASS[Math.round(w.wind_dir / 45) % 8] + ' ' : ''}${speed(w.wind_speed)}`,
    w.gust != null && `gusts ${speed(w.gust)}`,
    w.humidity != null && `${w.humidity} %`,
    w.rain_1h_mm > 0 && (metric ? `rain ${w.rain_1h_mm.toFixed(1)} mm/h` : `rain ${(w.rain_1h_mm / 25.4).toFixed(2)} in/h`),
    w.pressure_hpa != null && `${Math.round(w.pressure_hpa)} hPa`,
  ].filter(Boolean).join(' · ');
}

/** Lines describing a station or object. `p` is a Feature's properties. */
export function describe(p, metric) {
  // MapLibre hands nested properties back as JSON text.
  const weather = typeof p.weather === 'string' ? JSON.parse(p.weather) : p.weather;
  const heard = `${new Date(p.time).toLocaleTimeString([], { timeStyle: 'short' })}${p.direct === false ? '' : ' · heard directly'}`;
  return [p.name === p.callsign ? p.name : `${p.name} (from ${p.callsign})`, p.text, weather && weatherLine(weather, metric), heard].filter(Boolean);
}
