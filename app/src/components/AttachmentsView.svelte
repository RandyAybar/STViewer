<script lang="ts">
  import Icon from "./Icon.svelte";
  import FileTypeIcon from "./FileTypeIcon.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import AttachmentTable from "./AttachmentTable.svelte";
  import AttachmentGrid from "./AttachmentGrid.svelte";
  import AttachmentFilters from "./AttachmentFilters.svelte";
  import { store } from "../lib/store.svelte";
  import { CATEGORIES, CATEGORY_LABEL } from "../lib/icons";
  import { openAttachmentRow, saveAttachmentRow } from "../lib/attachments";
  import type { AttachmentRow, AttachmentSortKey } from "../lib/types";

  const SORT_LABEL: Record<AttachmentSortKey, string> = {
    date: "Date",
    size: "Size",
    name: "Name",
    type: "Type",
    from: "From",
  };

  let table = $state<ReturnType<typeof AttachmentTable>>();
  let grid = $state<ReturnType<typeof AttachmentGrid>>();
  let input = $state<HTMLInputElement>();
  let filtersOpen = $state(false);
  let menu = $state.raw<{ x: number; y: number; row: AttachmentRow } | null>(null);

  const list = $derived(store.attachments);
  const view = $derived(store.attPrefs.view);
  const progress = $derived(store.attSource != null ? store.progress[store.attSource] : undefined);
  const indexing = $derived(progress != null && !progress.done);
  const pct = $derived(progress?.total ? Math.min(100, Math.floor((progress.indexed / progress.total) * 100)) : null);

  /** Chips: categories with hits, plus selected ones (so they can be deselected). */
  const chips = $derived.by(() => {
    const counts = new Map(list?.categoryCounts ?? []);
    return CATEGORIES.filter((c) => (counts.get(c) ?? 0) > 0 || store.attCategories.includes(c)).map((c) => ({
      c,
      n: counts.get(c) ?? 0,
    }));
  });
  const allCount = $derived(chips.reduce((n, x) => n + x.n, 0));
  const narrowed = $derived(store.attText.trim() !== "" || store.attCategories.length > 0 || store.attFilterCount > 0);

  /** Items one PageUp/PageDown moves. */
  export function pageItems(): number {
    return (view === "icons" ? grid?.pageItems() : table?.pageItems()) ?? 20;
  }

  /** Items per visual row: the ↑/↓ step. */
  export function columns(): number {
    return (view === "icons" ? grid?.columns() : 1) ?? 1;
  }

  function filterKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (store.attText) store.setAttText("");
      else input?.blur();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      input?.blur();
      void store.setFocus(Math.max(0, store.focus));
    }
  }

  async function oncontext(e: MouseEvent, i: number) {
    e.preventDefault();
    const l = list;
    if (!l) return;
    if (!store.selection.has(i)) void store.setFocus(i);
    const row = await l.rowAsync(i);
    if (!row || store.attachments !== l) return;
    menu = { x: e.clientX, y: e.clientY, row };
  }

  /** Closes the menu, then runs `fn` on its row (captured first: `{@const r}` dies with the menu). */
  function act(fn: (row: AttachmentRow) => void) {
    const m = menu;
    menu = null;
    if (m) fn(m.row);
  }

  function windowClick(e: MouseEvent) {
    if (filtersOpen && !(e.target instanceof Element && e.target.closest(".filters-wrap"))) filtersOpen = false;
  }

  function windowKey(e: KeyboardEvent) {
    if (e.key !== "Escape" || !filtersOpen) return;
    filtersOpen = false;
    e.stopImmediatePropagation();
    e.preventDefault();
  }
</script>

<svelte:window onclick={windowClick} onkeydowncapture={windowKey} />

{#snippet placeholder()}
  {#if !list}
    <div class="placeholder muted">Loading…</div>
  {:else if list.error && list.count === 0}
    <div class="placeholder error selectable">{list.error}</div>
  {:else if list.loading}
    <div class="placeholder muted">Loading…</div>
  {:else if indexing}
    <div class="placeholder muted">
      <Icon name="paperclip" size={32} />
      <p>Attachments appear as the mailbox is indexed.</p>
      {#if progress}
        <div class="bar" title="{pct ?? 0}%"><div style:width="{pct ?? 0}%"></div></div>
        <p class="small">
          {progress.indexed.toLocaleString()}{progress.total ? ` of ${progress.total.toLocaleString()}` : ""} messages indexed
        </p>
      {/if}
    </div>
  {:else if narrowed}
    <div class="placeholder muted">
      <p>No matching attachments.</p>
      <button
        class="btn"
        onclick={() => {
          store.setAttText("");
          store.clearAttFilters();
        }}>Clear filters</button
      >
    </div>
  {:else if progress?.error}
    <div class="placeholder error selectable">Indexing failed: {progress.error}</div>
  {:else}
    <div class="placeholder muted">
      <Icon name="paperclip" size={32} />
      <p>No attachments in this mailbox{store.attPrefs.includeInline ? "" : " (inline images are hidden)"}.</p>
    </div>
  {/if}
{/snippet}

<div class="atts">
  <div class="toolbar">
    <div class="field">
      <Icon name="search" size={13} />
      <input
        bind:this={input}
        type="text"
        spellcheck="false"
        autocomplete="off"
        placeholder="Search"
        title="Matches file name, sender and subject; prefix a word with - to exclude it (e.g. -logo)"
        value={store.attText}
        oninput={(e) => store.setAttText(e.currentTarget.value)}
        onkeydown={filterKey}
      />
      {#if store.attText}
        <button class="icon-btn clear" title="Clear (Esc)" onclick={() => store.setAttText("")}
          ><Icon name="x" size={12} /></button
        >
      {/if}
    </div>
    <div class="filters-wrap">
      <button
        class="btn filters"
        class:on={filtersOpen}
        title="Hide types, extensions and sizes"
        aria-expanded={filtersOpen}
        onclick={() => (filtersOpen = !filtersOpen)}
      >
        <Icon name="filter" size={13} /> Filters
        {#if store.attFilterCount > 0}<span class="count">{store.attFilterCount}</span>{/if}
      </button>
      {#if filtersOpen}<AttachmentFilters onclose={() => (filtersOpen = false)} />{/if}
    </div>
    {#if view === "icons"}
      <div class="sort">
        <select
          title="Sort by"
          value={store.attPrefs.sort.key}
          onchange={(e) => {
            const key = e.currentTarget.value as AttachmentSortKey;
            store.setAttPrefs({ sort: { key, descending: key === "date" || key === "size" } });
          }}
        >
          {#each Object.entries(SORT_LABEL) as [k, label] (k)}
            <option value={k}>{label}</option>
          {/each}
        </select>
        <button
          class="icon-btn"
          title={store.attPrefs.sort.descending
            ? "Descending — click for ascending"
            : "Ascending — click for descending"}
          onclick={() =>
            store.setAttPrefs({ sort: { ...store.attPrefs.sort, descending: !store.attPrefs.sort.descending } })}
        >
          <Icon name={store.attPrefs.sort.descending ? "sortDesc" : "sortAsc"} size={15} />
        </button>
      </div>
    {/if}
    <button class="icon-btn" title="Refresh" onclick={() => store.refreshAttachments()}
      ><Icon name="refresh" size={14} /></button
    >
    <div class="seg" role="radiogroup" aria-label="View">
      <button
        role="radio"
        aria-checked={view === "details"}
        class:on={view === "details"}
        title="Details"
        onclick={() => store.setAttPrefs({ view: "details" })}
      >
        <Icon name="list" size={14} />
      </button>
      <button
        role="radio"
        aria-checked={view === "icons"}
        class:on={view === "icons"}
        title="Icons"
        onclick={() => store.setAttPrefs({ view: "icons" })}
      >
        <Icon name="grid" size={14} />
      </button>
    </div>
  </div>

  <div class="filter-chips">
    <button
      class="filter-chip"
      class:on={store.attCategories.length === 0}
      aria-pressed={store.attCategories.length === 0}
      onclick={() => store.toggleAttCategory(null)}
    >
      All <span class="n">{allCount.toLocaleString()}</span>
    </button>
    {#each chips as { c, n } (c)}
      <button
        class="filter-chip"
        class:on={store.attCategories.includes(c)}
        aria-pressed={store.attCategories.includes(c)}
        onclick={() => store.toggleAttCategory(c)}
      >
        <FileTypeIcon category={c} size={12} />
        {CATEGORY_LABEL[c]} <span class="n">{n.toLocaleString()}</span>
      </button>
    {/each}
  </div>

  <div class="summary">
    {#if list && !list.loading && !list.error}
      <span>{list.count.toLocaleString()} attachment{list.count === 1 ? "" : "s"}</span>
      {#if indexing}<span class="muted">· indexing{pct != null ? ` ${pct}%` : ""}…</span>{/if}
      {#if store.selection.size > 1}<span class="muted">· {store.selection.size.toLocaleString()} selected</span>{/if}
    {/if}
    <span class="grow"></span>
    <span class="muted">{store.attSource != null ? store.sourceName(store.attSource) : ""}</span>
  </div>

  <div class="body">
    {#if list}
      {#if view === "icons"}
        <AttachmentGrid bind:this={grid} {list} {placeholder} {oncontext} />
      {:else}
        <AttachmentTable bind:this={table} {list} {placeholder} {oncontext} />
      {/if}
    {:else}
      {@render placeholder()}
    {/if}
  </div>
</div>

{#if menu}
  {@const r = menu.row}
  <ContextMenu x={menu.x} y={menu.y} onclose={() => (menu = null)}>
    <button role="menuitem" onclick={() => act((row) => void openAttachmentRow(row))}
      ><Icon name="external" size={13} /> Open</button
    >
    <button role="menuitem" onclick={() => act((row) => void saveAttachmentRow(row))}
      ><Icon name="download" size={13} /> Save as…</button
    >
    <button role="menuitem" onclick={() => act((row) => store.showCarryingMessage(row))}
      ><Icon name="mail" size={13} /> Show message</button
    >
    <hr />
    {#if r.extension}
      <button role="menuitem" onclick={() => act((row) => store.hideExtension(row.extension))}>
        <Icon name="eyeOff" size={13} /> Hide .{r.extension} files
      </button>
    {/if}
    <button role="menuitem" onclick={() => act((row) => store.hideCategory(row.category))}>
      <Icon name="eyeOff" size={13} /> Hide {CATEGORY_LABEL[r.category].toLowerCase()}
    </button>
  </ContextMenu>
{/if}

<style>
  .atts {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--bg);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 40px;
    flex: none;
    padding: 0 8px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-2);
  }
  .field {
    flex: 1;
    min-width: 120px;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 2px 0 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg-3);
  }
  .field:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 25%, transparent);
  }
  .field input {
    flex: 1;
    min-width: 0;
    border: none;
    background: transparent;
    padding: 0;
    color: var(--fg);
    box-shadow: none;
  }
  .clear {
    width: 22px;
    height: 22px;
  }
  .filters-wrap {
    position: relative;
  }
  .btn.filters {
    height: 28px;
  }
  .btn.filters.on {
    background: var(--hover);
  }
  .count {
    min-width: 16px;
    padding: 0 4px;
    border-radius: 999px;
    background: var(--accent);
    color: var(--accent-fg);
    font-size: 10.5px;
    line-height: 16px;
    text-align: center;
    font-weight: 600;
  }
  .sort {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .sort select {
    height: 28px;
  }
  .seg {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 7px;
    background: var(--bg-3);
  }
  .seg button {
    display: inline-flex;
    align-items: center;
    border: none;
    background: transparent;
    padding: 3px 7px;
    border-radius: 5px;
    color: var(--fg-2);
  }
  .seg button.on {
    background: var(--bg);
    color: var(--fg);
    box-shadow: 0 0 0 1px var(--border);
  }
  .filter-chips {
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
  }
  .summary {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    flex: none;
    padding: 0 10px;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
  }
  .grow {
    flex: 1;
  }
  .body {
    flex: 1;
    min-height: 0;
    border-top: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
  }
  .small {
    font-size: 12px;
  }
  .bar {
    width: 200px;
    height: 4px;
    border-radius: 2px;
    background: var(--bg-3);
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s;
  }
</style>
