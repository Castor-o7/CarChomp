<script>
  import { api, live, ui } from './state.svelte.js';
  import Wifi from './Wifi.svelte';

  let health = $state(null);
  $effect(() => {
    const load = () => api('health').then((h) => (health = h), () => (health = null));
    load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });

  // Updating: the bundle is uploaded to the daemon, which has a root-owned
  // unit install it; that restarts the daemon, so the status is polled.
  let update = $state(null); // /var/lib/carchomp/update/status.json, or {state: 'idle'}
  let sent = $state(null); // share of the upload sent, while sending
  let updateError = $state('');
  const PHASE = { running: 'Installing', done: 'Installed', failed: 'Failed' };
  $effect(() => {
    const load = () => api('system/update').then((u) => (update = u), () => {});
    load();
    const timer = setInterval(load, 2000);
    return () => clearInterval(timer);
  });

  function install(event) {
    const file = event.target.files[0];
    event.target.value = '';
    if (!file || !confirm(`Install ${file.name}? CarChomp restarts while it installs, so this screen goes offline for a minute or two.`)) return;
    updateError = '';
    sent = 0;
    // fetch() cannot report upload progress; XMLHttpRequest can.
    const request = new XMLHttpRequest();
    request.open('POST', '/api/system/update');
    request.setRequestHeader('content-type', 'application/gzip');
    request.upload.onprogress = (e) => e.lengthComputable && (sent = e.loaded / e.total);
    request.onload = () => {
      sent = null;
      if (request.status !== 202) updateError = request.responseText || request.statusText;
    };
    request.onerror = () => {
      sent = null;
      updateError = 'Upload failed.';
    };
    request.send(file);
  }

  const fix = $derived(live.fix);
  const sys = $derived(health?.system ?? {});
  const gb = (bytes) => `${(bytes / 1e9).toFixed(1)} GB`;
  const days = (s) => (s > 86400 ? `${Math.floor(s / 86400)} d ` : '') + `${Math.floor((s % 86400) / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
</script>

<dl>
  <dt>Daemon</dt>
  <dd>{live.connected ? `connected · ${health?.version ?? '?'}` : 'unreachable'}</dd>
  <dt>GPS</dt>
  <dd>
    {#if fix}
      {fix.lat.toFixed(5)}, {fix.lon.toFixed(5)}{fix.h_err == null ? '' : ` · ±${Math.round(fix.h_err)} m`}
    {:else}
      no fix
    {/if}
  </dd>
  <dt>Recording</dt>
  <dd>{live.track === null ? 'no' : `track ${live.track}`}</dd>
  {#if health}
    <dt>Tracks</dt>
    <dd>{health.tracks} · {health.fixes} stored fixes</dd>
    <dt>APRS stations</dt>
    <dd>{health.aprs_stations}</dd>
    <dt>Database</dt>
    <dd>{(health.database_bytes / 1e6).toFixed(1)} MB</dd>
    {#if sys.disk_total_bytes}
      <dt>Disk</dt>
      <dd>{gb(sys.disk_free_bytes)} free of {gb(sys.disk_total_bytes)}</dd>
    {/if}
    {#if sys.mem_total_bytes}
      <dt>Memory</dt>
      <dd>{gb(sys.mem_available_bytes)} free of {gb(sys.mem_total_bytes)}</dd>
    {/if}
    {#if sys.load_1m != null}
      <dt>Load · CPU temperature</dt>
      <dd>{sys.load_1m.toFixed(2)}{sys.cpu_temp_c == null ? '' : ` · ${sys.cpu_temp_c.toFixed(0)} °C`}</dd>
    {/if}
    {#if sys.os}
      <dt>System</dt>
      <dd>{sys.os}<br />Linux {sys.kernel} · up {days(sys.uptime_s)}</dd>
    {/if}
  {/if}
</dl>

<h2>Settings</h2>
<button class:on={ui.chirp} onclick={() => (ui.chirp = !ui.chirp)}>Chirp on a new road</button>

<h2>Software update</h2>
<p>Choose a release bundle (carchomp-….tar.gz). The CarChomp service restarts while it installs, so this screen goes offline for a minute or two.</p>
<label class="button" class:disabled={sent !== null || update?.state === 'running'}>
  Install update<input type="file" accept=".gz,.tgz,application/gzip" hidden disabled={sent !== null || update?.state === 'running'} onchange={install} />
</label>
{#if sent !== null}<progress max="1" value={sent}></progress>{/if}
{#if updateError}<p class="error">{updateError}</p>{/if}
{#if update && update.state !== 'idle'}
  <p class:error={update.state === 'failed'}>
    {PHASE[update.state] ?? update.state}{update.version ? ` ${update.version}` : ''}{update.message ? `: ${update.message}` : ''}
  </p>
  {#if update.log}<details open={update.state === 'failed'}><summary>Log</summary><pre>{update.log}</pre></details>{/if}
{/if}

<Wifi />

<style>
  dt {
    color: var(--muted);
    margin-top: 14px;
  }
  dd {
    margin: 0;
    font-size: 1.2rem;
  }
  h2 {
    margin: 28px 0 12px;
    font-size: 1.2rem;
  }
  p {
    color: var(--muted);
  }
  .error {
    color: #fca5a5;
  }
  .disabled {
    opacity: 0.5;
  }
  progress {
    display: block;
    width: 100%;
    margin-top: 12px;
  }
  pre {
    font-size: 0.8rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
