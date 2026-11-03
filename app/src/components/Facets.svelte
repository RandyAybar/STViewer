<script lang="ts">
  import { store } from "../lib/store.svelte";
  import { quoteValue as quote } from "../lib/query";
  import type { FacetCount } from "../lib/types";

  const s = $derived(store.search);

  function refine(token: string) {
    store.runSearch(`${store.query.trim()} ${token}`);
  }

  function pickSource(f: FacetCount) {
    const id = /^\d+$/.test(f.value) ? Number(f.value) : store.sources.find((x) => x.display_name === f.value)?.id;
    if (id != null) store.restrictToSource(id);
  }

  function sourceLabel(v: string): string {
    return /^\d+$/.test(v) ? store.sourceName(Number(v)) : v;
  }

  function pickYear(v: string) {
    const y = Number.parseInt(v, 10);
    if (Number.isFinite(y)) refine(`after:${y}-01-01 before:${y + 1}-01-01`);
  }

  const maxOf = (l: FacetCount[]) => l.reduce((m, f) => Math.max(m, f.count), 1);
</script>

{#if s}
  <aside class="facets">
    <div class="summary">
      {#if s.loading}
        Searching…
      {:else if s.error}
        <span class="error selectable">{s.error}</span>
      {:else}
        <b>{s.count.toLocaleString()}</b> result{s.count === 1 ? "" : "s"}
      {/if}
    </div>

    {#if s.scope.sources.length === 1 && store.searchScope === "all"}
      <button class="chip" onclick={() => store.runSearch()}>Source: {store.sourceName(s.scope.sources[0])} ✕</button>
    {/if}

    {#snippet group(title: string, items: FacetCount[], label: (v: string) => string, pick: (f: FacetCount) => void)}
      {#if items.length}
        {@const max = maxOf(items)}
        <h4>{title}</h4>
        <ul>
          {#each items as f (f.value)}
            <li>
              <button onclick={() => pick(f)} title={f.value}>
                <span class="bar" style:width="{(f.count / max) * 100}%"></span>
                <span class="v">{label(f.value)}</span>
                <span class="c">{f.count.toLocaleString()}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    {/snippet}

    {@render group(
      "Senders",
      s.facetsFrom,
      (v) => v,
      (f) => refine(`from:${quote(f.value)}`),
    )}
    {@render group(
      "Years",
      [...s.facetsYear].sort((a, b) => b.value.localeCompare(a.value)),
      (v) => v,
      (f) => pickYear(f.value),
    )}
    {#if store.sources.length > 1}
      {@render group("Sources", s.facetsSource, sourceLabel, pickSource)}
    {/if}
  </aside>
{/if}

<style>
  .facets {
    height: 100%;
    overflow: auto;
    padding: 10px 8px;
    background: var(--bg-2);
    min-width: 0;
  }
  .summary {
    padding: 0 6px 8px;
    font-size: 12px;
  }
  .error {
    color: var(--danger);
  }
  h4 {
    margin: 12px 6px 4px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-2);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li button {
    position: relative;
    display: flex;
    width: 100%;
    gap: 6px;
    align-items: center;
    padding: 3px 6px;
    border: none;
    border-radius: 5px;
    background: transparent;
    text-align: left;
    font-size: 12px;
    overflow: hidden;
  }
  li button:hover {
    background: var(--hover);
  }
  .bar {
    position: absolute;
    left: 0;
    top: 2px;
    bottom: 2px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-radius: 4px;
    pointer-events: none;
  }
  .v {
    position: relative;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .c {
    position: relative;
    color: var(--fg-3);
    font-variant-numeric: tabular-nums;
  }
  .chip {
    margin: 0 6px 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    font-size: 11px;
    padding: 2px 8px;
  }
</style>
