<script>
  import { untrack } from 'svelte';
  import { api, live, ui } from './state.svelte.js';

  const NEARBY_METRES = 200;

  let tracks = $state([]);
  let nearby = $state(false);
  let error = $state('');

  $effect(() => {
    ui.tracksChanged;
    // Deliberately not reactive to every fix: "nearby" means near where we
    // were when the list was opened or last refreshed.
    const fix = nearby ? untrack(() => live.fix) : null;
    api(fix ? `tracks?near=${fix.lon},${fix.lat},${NEARBY_METRES}` : 'tracks').then((t) => (tracks = t), report);
  });

  const report = (e) => (error = e.message);
  const changed = () => ((error = ''), ui.tracksChanged++);
  const patch = (id, body) =>
    api(`tracks/${id}`, { method: 'PATCH', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }).then(changed, report);
  const remove = (t) => confirm(`Delete ${title(t)}?`) && api(`tracks/${t.id}`, { method: 'DELETE' }).then(changed, report);
  const show = (t) => api(`tracks/${t.id}`).then((feature) => (ui.focus = feature), report);

  async function upload(event) {
    for (const file of event.target.files) {
      await api('tracks/import', { method: 'POST', body: await file.text() }).then(changed, report);
    }
    event.target.value = '';
  }

  const title = (t) => t.name || new Date(t.started).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
  const length = (t) => (ui.metric ? `${(t.metres / 1000).toFixed(1)} km` : `${(t.metres / 1609.344).toFixed(1)} mi`);
</script>

<header>
  <button class:on={nearby} onclick={() => (nearby = !nearby)}>Nearby</button>
  <label class="button">Import<input type="file" accept=".gpx,.json,.geojson" multiple hidden onchange={upload} /></label>
</header>

{#if error}<p class="error">{error}</p>{/if}

{#each tracks as t (t.id)}
  <section class:hidden={!t.visible}>
    <input value={t.name ?? ''} placeholder={title(t)} onchange={(e) => patch(t.id, { name: e.target.value })} />
    <small>{t.ended ? length(t) : 'recording'}</small>
    <div>
      <button onclick={() => show(t)}>Show</button>
      <button class:on={t.visible} onclick={() => patch(t.id, { visible: !t.visible })}>On map</button>
      <a class="button" href="/api/tracks/{t.id}/gpx" download>GPX</a>
      <button onclick={() => remove(t)}>Delete</button>
    </div>
  </section>
{:else}
  <p>No tracks {nearby ? 'near here' : 'yet'}.</p>
{/each}

<style>
  header,
  section div {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  section {
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--line);
    display: grid;
    gap: 8px;
  }
  section.hidden input,
  small {
    color: var(--muted);
  }
  input {
    width: 100%;
  }
  .error {
    color: #fca5a5;
  }
</style>
