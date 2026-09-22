<script>
  import { api, ui } from './state.svelte.js';

  const BUILDS = 'https://build-metadata.protomaps.dev/builds.json';
  const DETAIL = [
    ['Streets', 15],
    ['Roads', 12],
    ['Highways', 8],
    ['World', 5],
  ];

  let maps = $state({ archives: [], job: null, source: null });
  let name = $state('');
  let maxzoom = $state(15);
  let error = $state('');

  const downloading = $derived(maps.job && !maps.job.error);
  const mb = (bytes) => `${(bytes / 1e6).toFixed(1)} MB`;
  const report = (e) => (error = e.message);

  async function load() {
    const was = downloading;
    maps = await api('maps');
    if (was && !downloading) ui.mapsChanged++;
  }

  $effect(() => {
    load().catch(report);
    const timer = setInterval(() => load().catch(report), 2000);
    return () => clearInterval(timer);
  });

  async function download() {
    error = '';
    try {
      // The planet archive is rebuilt daily under a new name; use the newest
      // unless the daemon has been told where to look.
      const source = maps.source ?? `https://build.protomaps.com/${(await (await fetch(BUILDS)).json()).at(-1).key}`;
      await api('maps', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ name, bbox: $state.snapshot(ui.bounds), maxzoom, source }),
      });
      name = '';
      await load();
    } catch (e) {
      report(e);
    }
  }

  const remove = (archive) =>
    confirm(`Delete ${archive.name}?`) && api(`maps/${archive.name}`, { method: 'DELETE' }).then(() => (ui.mapsChanged++, load()), report);
</script>

<p>Offline regions. Move the map to the area you want, then download it.</p>

<form onsubmit={(e) => (e.preventDefault(), download())}>
  <input bind:value={name} placeholder="Name, e.g. portland" pattern="[A-Za-z0-9_\-]+" maxlength="64" required />
  <div>
    {#each DETAIL as [label, zoom]}
      <button type="button" class:on={maxzoom === zoom} onclick={() => (maxzoom = zoom)}>{label}</button>
    {/each}
  </div>
  <button disabled={downloading}>Download this view</button>
</form>

{#if error}<p class="error">{error}</p>{/if}
{#if maps.job}
  <p class:error={maps.job.error}>
    {maps.job.name}: {maps.job.error ?? `downloading… ${mb(maps.job.bytes)}`}
  </p>
{/if}

{#each maps.archives as archive (archive.name)}
  <section>
    <span>{archive.name}<br /><small>{mb(archive.bytes)}</small></span>
    <button onclick={() => remove(archive)}>Delete</button>
  </section>
{/each}

<style>
  form {
    display: grid;
    gap: 8px;
  }
  form div {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  section {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--line);
  }
  small,
  p {
    color: var(--muted);
  }
  .error {
    color: #fca5a5;
  }
  button:disabled {
    opacity: 0.5;
  }
</style>
