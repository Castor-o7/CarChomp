<script>
  import { api } from './state.svelte.js';

  let wifi = $state(null); // null: not available on this host
  let joining = $state(null); // the secured network being typed into
  let password = $state('');
  let identity = $state(''); // user name, for WPA-Enterprise networks
  let busy = $state(false);
  let error = $state('');

  const load = () => api('wifi').then((w) => (wifi = w), () => (wifi = null));
  $effect(() => {
    load();
    const timer = setInterval(load, 10_000);
    return () => clearInterval(timer);
  });

  async function run(request) {
    busy = true;
    error = '';
    await request.catch((e) => (error = e.message));
    busy = false;
    joining = null;
    password = '';
    identity = '';
    await load();
  }
  const post = (body) => api('wifi', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });

  function join(network) {
    if (network.secure && !wifi.saved.includes(network.ssid)) joining = network.ssid;
    else run(post({ ssid: network.ssid }));
  }
  const forget = (ssid) => confirm(`Forget ${ssid}?`) && run(api(`wifi/${encodeURIComponent(ssid)}`, { method: 'DELETE' }));
</script>

{#if wifi}
  <h2>Wi-Fi</h2>
  {#if error}<p class="error">{error}</p>{/if}
  {#each wifi.networks as network (network.ssid)}
    <section>
      <span class:active={network.active}>
        {network.ssid}<br /><small>{network.active ? 'connected · ' : ''}{network.signal}%{network.secure ? ' · secured' : ''}</small>
      </span>
      {#if wifi.saved.includes(network.ssid)}<button disabled={busy} onclick={() => forget(network.ssid)}>Forget</button>{/if}
      {#if !network.active}<button disabled={busy} onclick={() => join(network)}>Join</button>{/if}
    </section>
    {#if joining === network.ssid}
      <!-- Joining from a phone on the hotspot drops that phone: one radio. -->
      <form onsubmit={(e) => (e.preventDefault(), run(post(network.enterprise ? { ssid: network.ssid, identity, password } : { ssid: network.ssid, password })))}>
        {#if network.enterprise}<input bind:value={identity} placeholder="Identity" autocomplete="username" required />{/if}
        <input type="password" bind:value={password} placeholder="Password" minlength={network.enterprise ? 1 : 8} required />
        <button disabled={busy}>Connect</button>
      </form>
    {/if}
  {:else}
    <p>
      {wifi.hotspot
        ? 'The car cannot scan for networks while it is hosting its hotspot. Nearby networks appear here when no one is connected to it.'
        : 'No networks in range.'}
    </p>
  {/each}
{/if}

<style>
  h2 {
    margin: 28px 0 0;
    font-size: 1.2rem;
  }
  section,
  form {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-top: 12px;
  }
  form {
    flex-wrap: wrap;
  }
  span,
  input {
    flex: 1;
  }
  .active {
    color: #86efac;
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
