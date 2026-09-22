<script>
  import { api, live } from './state.svelte.js';
  import Wifi from './Wifi.svelte';

  let health = $state(null);
  $effect(() => {
    const load = () => api('health').then((h) => (health = h), () => (health = null));
    load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });

  const fix = $derived(live.fix);
  const sys = $derived(health?.system ?? {});
  const gb = (bytes) => `${(bytes / 1e9).toFixed(1)} GB`;
  const days = (s) => (s > 86400 ? `${Math.floor(s / 86400)} d ` : '') + `${Math.floor((s % 86400) / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
</script>

<dl>
  <dt>Daemon</dt>
  <dd>{live.connected ? `connected · v${health?.version ?? '?'}` : 'unreachable'}</dd>
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
</style>
