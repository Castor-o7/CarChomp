<script>
  import { api } from './state.svelte.js';

  let wifi = $state(null); // null: not available on this host
  let joining = $state(null); // the secured network being typed into
  let password = $state('');
  let identity = $state(''); // user name, for WPA-Enterprise networks
  let profile = $state(null); // { name, text } of the chosen .eap-config file
  let domain = $state(''); // RADIUS server domain, when there is no profile
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
    const ok = await request.then(() => true, (e) => ((error = e.message), false));
    busy = false;
    // On failure the form stays filled in, so a wrong domain or profile is a quick fix.
    if (ok) {
      joining = null;
      password = identity = domain = '';
      profile = null;
    }
    await load();
  }
  const post = (body) => api('wifi', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });

  function join(network) {
    if (network.secure && !wifi.saved.includes(network.ssid)) {
      if (joining !== network.ssid) {
        password = identity = domain = '';
        profile = null;
      }
      joining = network.ssid;
    } else run(post({ ssid: network.ssid }));
  }
  // Enterprise: the profile (or the domain) is what lets the car check it talks to the real server.
  const enterprise = (ssid) =>
    profile
      ? { ssid, identity, eap_config: profile.text, ...(password && { password }) }
      : { ssid, identity, password, domain: domain.trim() };
  async function pick(event) {
    const file = event.currentTarget.files[0];
    event.currentTarget.value = ''; // picking the same file again still fires
    if (file) profile = { name: file.name, text: await file.text() };
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
      <form onsubmit={(e) => (e.preventDefault(), run(post(network.enterprise ? enterprise(network.ssid) : { ssid: network.ssid, password })))}>
        {#if network.enterprise}
          <p class="note">
            The car only joins after checking the network's server, so a fake {network.ssid} cannot capture your password. Load
            your institution's profile, or enter its server domain.
          </p>
          <input bind:value={identity} placeholder="Username" autocomplete="username" required />
          <input
            type="password"
            bind:value={password}
            placeholder={profile ? 'Password (if the profile needs one)' : 'Password'}
            autocomplete="current-password"
            required={!profile}
          />
          <div class="row">
            <label class="button">
              {profile ? profile.name : 'Institution profile (.eap-config)'}<!-- no accept filter: phones do not know .eap-config and would grey it out -->
              <input type="file" hidden onchange={pick} />
            </label>
            {#if profile}<button type="button" onclick={() => (profile = null)}>Remove</button>{/if}
          </div>
          {#if !profile}
            <input bind:value={domain} placeholder="Server domain" autocapitalize="off" spellcheck="false" required />
            <small class="note">From your institution's eduroam setup instructions, e.g. radius.example.edu</small>
          {/if}
        {:else}
          <input type="password" bind:value={password} placeholder="Password" minlength="8" required />
        {/if}
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
  .note,
  .row,
  .row label {
    flex: 1 1 100%;
    margin: 0;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .row label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
