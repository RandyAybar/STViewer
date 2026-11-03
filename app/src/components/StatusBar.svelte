<script lang="ts">
  import { store } from "../lib/store.svelte";

  const indexing = $derived(store.sources.filter((s) => store.progress[s.id] && !store.progress[s.id].done));
  const failed = $derived(store.sources.filter((s) => store.progress[s.id]?.error));
  const totalDocs = $derived(store.sources.reduce((n, s) => n + (store.progress[s.id]?.indexed ?? 0), 0));

  function pct(id: number): string {
    const p = store.progress[id];
    if (!p?.total) return `${(p?.indexed ?? 0).toLocaleString()}`;
    return `${Math.min(100, Math.floor((p.indexed / p.total) * 100))}%`;
  }
</script>

<footer class="status">
  <span class="left">
    {#if store.search}
      {#if !store.search.loading}{store.search.count.toLocaleString()} results{/if}
    {:else if store.attachments}
      {#if !store.attachments.loading}{store.attachments.count.toLocaleString()} attachments{/if}
    {:else if store.tagged}
      {#if !store.tagged.loading}{store.tagged.count.toLocaleString()} tagged items{/if}
    {:else if store.allMail}
      {#if !store.allMail.loading}{store.allMail.count.toLocaleString()} messages{/if}
    {:else if store.folderList}
      {store.folderList.count.toLocaleString()} messages
    {/if}
    {#if store.selection.size > 1}
      · {store.selection.size.toLocaleString()} selected
    {/if}
  </span>
  <span class="right">
    {#if store.agent?.running}
      {@const n = store.agent.allowed_sources.length}
      <!-- Re-mounted on each agent call so the flash animation restarts. -->
      {#key store.agentPulse}
        <button
          class="agent-pill"
          class:flash={store.agentPulse > 0}
          title="A coding agent can read the shared mailboxes via {store.agent.url} — open Settings"
          onclick={() => (store.settingsOpen = true)}>Agent access ON · {n} shared</button
        >
      {/key}
    {/if}
    {#each indexing as s (s.id)}
      {@const p = store.progress[s.id]}
      <span
        class="chip"
        title="{p.indexed.toLocaleString()}{p.total ? ` / ${p.total.toLocaleString()}` : ''} messages indexed"
      >
        <span class="spin"></span>
        Indexing {s.display_name} — {pct(s.id)}
      </span>
    {/each}
    {#each failed as s (s.id)}
      <span class="chip err" title={store.progress[s.id]?.error ?? ""}>⚠ {s.display_name}: index error</span>
    {/each}
    {#if indexing.length === 0 && store.sources.length > 0}
      <span class="muted"
        >{totalDocs.toLocaleString()} messages indexed in {store.sources.length} source{store.sources.length === 1
          ? ""
          : "s"}</span
      >
    {/if}
  </span>
</footer>

<style>
  .status {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 24px;
    flex: none;
    padding: 0 12px;
    border-top: 1px solid var(--border);
    background: var(--bg-2);
    font-size: 11.5px;
    color: var(--fg-2);
    white-space: nowrap;
    overflow: hidden;
  }
  .left {
    flex: 1;
  }
  .right {
    display: flex;
    gap: 10px;
    overflow: hidden;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .chip.err {
    color: var(--danger);
  }
  .agent-pill {
    padding: 0 8px;
    border: 1px solid color-mix(in srgb, var(--accent) 50%, transparent);
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
    font-size: 11px;
    line-height: 16px;
  }
  .agent-pill:hover {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .agent-pill.flash {
    animation: flash 0.9s ease-out;
  }
  @keyframes flash {
    from {
      background: color-mix(in srgb, var(--accent) 45%, transparent);
      box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 25%, transparent);
    }
  }
</style>
