<script lang="ts">
  import { onMount } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import Icon from "./Icon.svelte";
  import { Copier } from "../lib/clipboard.svelte";
  import { store } from "../lib/store.svelte";

  const a = $derived(store.agent);

  let port = $state("");
  /** Mailboxes ticked in the form (applied with the switch or Save). */
  const allowed = new SvelteSet<number>();
  let loaded = false;
  let busy = $state(false);
  let showToken = $state(false);
  let confirmRegen = $state(false);
  const copier = new Copier();

  // Seed the form from the backend once its state arrives.
  $effect(() => {
    const s = store.agent;
    if (!s || loaded) return;
    loaded = true;
    port = String(s.port);
    for (const id of s.allowed_sources) allowed.add(id);
  });

  onMount(() => void store.refreshAgent());

  const portNum = $derived(Number(port));
  const portOk = $derived(Number.isInteger(portNum) && portNum >= 1 && portNum <= 65535);
  /** Ticked sources that still exist (removed sources drop out). */
  const shared = $derived(store.sources.filter((s) => allowed.has(s.id)).map((s) => s.id));
  const dirty = $derived(
    a != null &&
      (portNum !== a.port ||
        shared.length !== a.allowed_sources.length ||
        shared.some((id) => !a.allowed_sources.includes(id))),
  );
  const token = $derived(a?.token ?? "");
  const shownToken = $derived(showToken ? token : `${"•".repeat(20)}${token.slice(-4)}`);
  const url = $derived(a?.url ?? `http://127.0.0.1:${port}/mcp`);

  const snippets = $derived.by(() => {
    const make = (t: string): { id: string; title: string; text: string }[] => [
      {
        id: "claude",
        title: "Claude Code (command line)",
        text: `claude mcp add --transport http stviewer ${url} --header "Authorization: Bearer ${t}"`,
      },
      {
        id: "json",
        title: "Cursor / generic JSON (mcp.json)",
        text: JSON.stringify({ mcpServers: { stviewer: { url, headers: { Authorization: `Bearer ${t}` } } } }, null, 2),
      },
      {
        id: "codex",
        title: "Codex (~/.codex/config.toml)",
        text: `[mcp_servers.stviewer]\nurl = "${url}"\nhttp_headers = { Authorization = "Bearer ${t}" }`,
      },
    ];
    const real = make(token);
    const shown = make(shownToken);
    return real.map((s, i) => ({ ...s, shown: shown[i].text }));
  });

  async function apply(enabled: boolean) {
    if (!portOk || busy) return;
    busy = true;
    const ok = await store.setAgent(enabled, portNum, shared);
    busy = false;
    if (!ok && store.agent) port = String(store.agent.port);
  }

  function selectAll(on: boolean) {
    allowed.clear();
    if (on) for (const s of store.sources) allowed.add(s.id);
  }

  async function regenerate() {
    confirmRegen = false;
    busy = true;
    await store.regenerateAgentToken();
    busy = false;
  }
</script>

<section class="agent">
  <div class="title-row">
    <h3>Agent access (MCP)</h3>
    <input
      type="checkbox"
      class="switch"
      role="switch"
      aria-label="Enable agent access"
      checked={a?.enabled ?? false}
      disabled={!a || busy || !portOk}
      onchange={(e) => apply(e.currentTarget.checked)}
    />
  </div>

  {#if a?.error}
    <div class="error selectable" role="alert">Error: {a.error}</div>
  {/if}
  {#if a?.enabled && a.allowed_sources.length === 0}
    <div class="warn">No mailboxes shared — agents will see nothing.</div>
  {/if}

  <div class="grid">
    <div class="field">
      <div class="label-row">
        <span class="label">Mailboxes to share</span>
        <button class="link" onclick={() => selectAll(true)}>Select all</button>
        <span class="muted">/</span>
        <button class="link" onclick={() => selectAll(false)}>none</button>
      </div>
      <div class="sources">
        {#each store.sources as s (s.id)}
          <label class="src">
            <input
              type="checkbox"
              checked={allowed.has(s.id)}
              onchange={(e) => (e.currentTarget.checked ? allowed.add(s.id) : allowed.delete(s.id))}
            />
            <span class="name">{s.display_name}</span>
            <span class="n muted">{store.sourceTotal(s.id).toLocaleString()}</span>
          </label>
        {:else}
          <p class="muted">No mailboxes in the library.</p>
        {/each}
      </div>
    </div>
    <div class="field">
      <label class="label" for="agent-port">Port</label>
      <input id="agent-port" type="text" inputmode="numeric" bind:value={port} class:bad={!portOk} />
      {#if !portOk}<span class="bad-msg">1–65535</span>{/if}
      {#if dirty}
        <button class="btn primary save" disabled={!portOk || busy} onclick={() => apply(a?.enabled ?? false)}>
          Save changes
        </button>
        <span class="muted small">{a?.enabled ? "Restarts the server." : "Applied when you turn access on."}</span>
      {/if}
    </div>
  </div>

  <h4>Access token</h4>
  <div class="token">
    <code class="selectable">{a ? shownToken : "…"}</code>
    <button class="btn" disabled={!a} onclick={() => (showToken = !showToken)}>{showToken ? "Hide" : "Show"}</button>
    <button class="btn" disabled={!a} onclick={() => copier.copy("token", token)}>
      <Icon name="copy" size={13} />{copier.copied === "token" ? "Copied" : "Copy"}
    </button>
    {#if confirmRegen}
      <span class="confirm">
        Agents using the old token will be disconnected.
        <button class="btn danger" disabled={busy} onclick={regenerate}>Regenerate</button>
        <button class="btn" onclick={() => (confirmRegen = false)}>Cancel</button>
      </span>
    {:else}
      <button class="btn" disabled={!a || busy} onclick={() => (confirmRegen = true)}
        ><Icon name="refresh" size={13} /> Regenerate</button
      >
    {/if}
  </div>

  <h4>Connect your agent</h4>
  {#each snippets as s (s.id)}
    <div class="snippet">
      <div class="snippet-head">
        <span>{s.title}</span>
        <button class="btn small" disabled={!a} onclick={() => copier.copy(s.id, s.text)}>
          <Icon name="copy" size={12} />{copier.copied === s.id ? "Copied" : "Copy"}
        </button>
      </div>
      <pre class="selectable">{s.shown}</pre>
    </div>
  {/each}
</section>

<style>
  .title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  h3 {
    margin: 0;
    font-size: 14px;
  }
  h4 {
    margin: 18px 0 6px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-2);
  }
  .switch {
    appearance: none;
    width: 34px;
    height: 20px;
    margin: 0;
    flex: none;
    border-radius: 999px;
    background: var(--bg-3);
    border: 1px solid var(--border);
    position: relative;
    cursor: pointer;
    transition: background 0.15s;
  }
  .switch::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--fg-2);
    transition: transform 0.15s;
  }
  .switch:checked {
    background: var(--accent);
    border-color: var(--accent);
  }
  .switch:checked::after {
    transform: translateX(14px);
    background: var(--accent-fg);
  }
  .switch:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .error {
    margin-top: 8px;
    color: var(--danger);
  }
  code,
  pre {
    font-family: var(--mono);
    font-size: 12px;
  }
  .warn {
    margin-top: 6px;
    color: var(--warn);
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 200px;
    gap: 16px;
    margin-top: 14px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .label-row {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .label {
    font-weight: 600;
    margin-right: auto;
  }
  .sources {
    max-height: 180px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 8px;
  }
  .src {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 0;
  }
  .src .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .n {
    font-size: 11.5px;
    font-variant-numeric: tabular-nums;
  }
  #agent-port {
    width: 100px;
  }
  #agent-port.bad {
    border-color: var(--danger);
  }
  .bad-msg {
    color: var(--danger);
    font-size: 11.5px;
  }
  .save {
    align-self: flex-start;
  }
  .small {
    font-size: 11.5px;
  }
  .token {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
  }
  .token code {
    flex: 1;
    min-width: 200px;
    padding: 4px 8px;
    border-radius: 6px;
    background: var(--bg-2);
    border: 1px solid var(--border);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .confirm {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--warn);
    font-size: 12px;
  }
  .btn.danger {
    color: var(--danger);
  }
  .btn.small {
    padding: 1px 8px;
    font-size: 12px;
  }
  .snippet {
    margin-bottom: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .snippet-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 8px;
    background: var(--bg-2);
    font-size: 12px;
    color: var(--fg-2);
  }
  pre {
    margin: 0;
    padding: 8px 10px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
