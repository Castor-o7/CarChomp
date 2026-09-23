<script>
  import Aprs from './Aprs.svelte';
  import Map from './Map.svelte';
  import Maps from './Maps.svelte';
  import System from './System.svelte';
  import Tracks from './Tracks.svelte';
  import { live, ui } from './state.svelte.js';

  // gpsd goes quiet when it loses the fix, so an old fix means no GPS.
  let fixAt = $state(0);
  let now = $state(Date.now());
  $effect(() => {
    if (live.fix) fixAt = Date.now();
  });
  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });
  const fresh = $derived(live.fix && now - fixAt < 5000);

  const speed = $derived.by(() => {
    if (!fresh) return '–';
    const ms = live.fix.speed;
    if (ms == null) return '–';
    return Math.round(ms * (ui.metric ? 3.6 : 2.23694));
  });

  const road = $derived(
    !live.connected ? ['offline', 'Offline']
    : !fresh ? ['', 'No GPS']
    : live.road === true ? ['new', 'New road']
    : live.road === false ? ['', 'Been here']
    : ['', 'Parked'],
  );

  const toggle = (panel) => (ui.panel = ui.panel === panel ? null : panel);

  // The reTerminal's front buttons F1, F2, F3 and O arrive as these keys.
  const buttons = { a: () => toggle('aprs'), s: () => toggle('tracks'), d: () => toggle('maps'), f: () => ((ui.panel = null), (ui.follow = true)) };
  const pressed = (event) => event.target.tagName !== 'INPUT' && buttons[event.key]?.();
  $effect(() => {
    localStorage.aprs = ui.aprs;
    localStorage.metric = ui.metric;
  });
</script>

<svelte:window onkeydown={pressed} />

<Map />

<div class="road {road[0]}">{road[1]}</div>

<button class="speed" onclick={() => (ui.metric = !ui.metric)}>
  <b>{speed}</b>
  {ui.metric ? 'km/h' : 'mph'}
</button>

<nav>
  <button class:on={ui.follow} onclick={() => (ui.follow = !ui.follow)}>Follow</button>
  <button class:on={ui.panel === 'aprs'} onclick={() => toggle('aprs')}>APRS</button>
  <button class:on={ui.panel === 'tracks'} onclick={() => toggle('tracks')}>Tracks</button>
  <button class:on={ui.panel === 'maps'} onclick={() => toggle('maps')}>Maps</button>
  <button class:on={ui.panel === 'system'} onclick={() => toggle('system')}>System</button>
</nav>

{#if ui.panel}
  <aside>
    {#if ui.panel === 'tracks'}<Tracks />{:else if ui.panel === 'aprs'}<Aprs />{:else if ui.panel === 'maps'}<Maps />{:else}<System />{/if}
  </aside>
{/if}

<style>
  .road,
  .speed,
  nav,
  aside {
    position: absolute;
    z-index: 1;
  }
  .road {
    top: 12px;
    left: 12px;
    padding: 10px 22px;
    border-radius: 12px;
    font-size: 1.5rem;
    font-weight: 700;
    background: var(--panel);
  }
  .road.new {
    background: var(--new);
  }
  .road.offline {
    background: #b91c1c;
  }
  .speed {
    top: 12px;
    right: 12px;
    flex-direction: column;
    padding: 6px 18px;
    color: var(--muted);
  }
  .speed b {
    font-size: 2.2rem;
    line-height: 1;
    color: var(--text);
  }
  nav {
    left: 12px;
    bottom: 12px;
    display: flex;
    flex-wrap: wrap-reverse;
    gap: 8px;
  }
  aside {
    top: 0;
    right: 0;
    bottom: 0;
    width: min(440px, 100%);
    padding: 16px;
    overflow-y: auto;
    background: var(--bg);
    border-left: 1px solid var(--line);
  }
  /* On a phone the panel covers the map, so keep the way out on top. */
  @media (max-width: 700px) {
    nav {
      z-index: 2;
    }
    aside {
      padding-bottom: 84px;
    }
  }
</style>
