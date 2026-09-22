<script>
  import { describe } from './aprs.js';
  import { api, live, ui } from './state.svelte.js';

  let bulletins = $state([]);
  let stored = $state([]);

  $effect(() => {
    live.bulletins; // refetch when a message is heard
    api('aprs/bulletins').then((b) => (bulletins = b), () => {});
  });
  $effect(() => {
    api('aprs/stations').then((c) => (stored = c.features), () => {});
  });

  // Objects are how incidents arrive: closures, fires, hazards. Live packets
  // override what was stored, including taking withdrawn objects off the list.
  const incidents = $derived(
    [...stored.filter((f) => !(f.properties.name in live.stations)), ...Object.values(live.stations).filter(Boolean)]
      .filter((f) => f.properties.kind === 'object')
      .sort((a, b) => b.properties.time.localeCompare(a.properties.time)),
  );

  const when = (time) => new Date(time).toLocaleTimeString([], { timeStyle: 'short' });
  const show = (feature) => (ui.focus = { geometry: { coordinates: [feature.geometry.coordinates] } });
</script>

<button class:on={ui.aprs} onclick={() => (ui.aprs = !ui.aprs)}>Show on map</button>

<h2>Alerts</h2>
{#each bulletins as b (b.callsign + b.addressee)}
  <section>
    <span>{b.text}<br /><small>{b.addressee} · {b.callsign} · {when(b.time)}</small></span>
  </section>
{:else}
  <p>No bulletins heard today.</p>
{/each}

<h2>Incidents</h2>
{#each incidents as feature (feature.properties.name)}
  {@const [title, ...lines] = describe(feature.properties, ui.metric)}
  <section>
    <span>{title}<br /><small>{lines.join(' · ')}</small></span>
    <button onclick={() => show(feature)}>Show</button>
  </section>
{:else}
  <p>None reported nearby.</p>
{/each}

<style>
  h2 {
    margin: 28px 0 0;
    font-size: 1.2rem;
  }
  section {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid var(--line);
  }
  small,
  p {
    color: var(--muted);
  }
</style>
