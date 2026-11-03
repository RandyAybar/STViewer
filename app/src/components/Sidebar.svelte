<script lang="ts">
  import Icon from "./Icon.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import { store } from "../lib/store.svelte";
  import { folderIcon, sourceIcon, SOURCE_KIND_LABEL } from "../lib/icons";
  import type { Folder, SourceId } from "../lib/types";

  /** Guards against malformed parent chains (a folder listed as its own descendant). */
  const MAX_DEPTH = 64;

  /** Children by parent id per source (`null` = top level); folders whose parent is unknown become roots. */
  const trees = $derived.by(() => {
    const out: Record<number, Map<string | null, Folder[]>> = {};
    for (const s of store.sources) {
      const list = store.folders[s.id];
      if (!list) continue;
      const ids = new Set(list.map((f) => f.id));
      const m = new Map<string | null, Folder[]>();
      for (const f of list) {
        const key = f.parent != null && f.parent !== f.id && ids.has(f.parent) ? f.parent : null;
        let arr = m.get(key);
        if (!arr) m.set(key, (arr = []));
        arr.push(f);
      }
      out[s.id] = m;
    }
    return out;
  });

  let menu = $state.raw<{ source: SourceId; x: number; y: number } | null>(null);
  let tree = $state<HTMLDivElement>();

  function pct(id: SourceId): number | null {
    const p = store.progress[id];
    if (!p || p.done || !p.total) return null;
    return Math.min(100, Math.round((p.indexed / p.total) * 100));
  }

  function openMenu(e: MouseEvent, source: SourceId) {
    e.stopPropagation();
    if (menu?.source === source) {
      menu = null;
      return;
    }
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu = { source, x: r.left, y: r.bottom + 2 };
  }

  /** Closes the menu, then runs `fn` on its source (read first: the menu block unmounts). */
  function act(fn: (id: SourceId) => Promise<void>) {
    const id = menu?.source;
    menu = null;
    if (id != null) void fn(id);
  }

  /** Tree keyboard model: ↑/↓/Home/End move, →/← expand/collapse, Enter/Space open. */
  function treeKey(e: KeyboardEvent) {
    const items = [...(tree?.querySelectorAll<HTMLElement>("[role=treeitem]") ?? [])];
    const cur = e.target instanceof Element ? e.target.closest<HTMLElement>("[role=treeitem]") : null;
    const i = cur ? items.indexOf(cur) : -1;
    const twisty = () => cur?.querySelector<HTMLElement>("button.twisty")?.click();
    switch (e.key) {
      case "ArrowDown":
        items[Math.min(items.length - 1, i + 1)]?.focus();
        break;
      case "ArrowUp":
        items[Math.max(0, i - 1)]?.focus();
        break;
      case "Home":
        items[0]?.focus();
        break;
      case "End":
        items.at(-1)?.focus();
        break;
      case "ArrowRight":
        if (cur?.getAttribute("aria-expanded") === "false") twisty();
        else items[i + 1]?.focus();
        break;
      case "ArrowLeft":
        if (cur?.getAttribute("aria-expanded") === "true") twisty();
        break;
      case "Enter":
      case " ":
        if (e.target !== cur) return; // a button inside the row handles its own keys
        cur?.click();
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }
</script>

{#snippet folderNode(source: SourceId, f: Folder, depth: number)}
  {@const kids = depth < MAX_DEPTH ? (trees[source]?.get(f.id) ?? []) : []}
  {@const active = store.current?.source === source && store.current.folder === f.id && !store.search}
  {@const collapsed = store.isCollapsed(source, f.id)}
  <!-- Keys for every row are handled by the tree's onkeydown (roving focus). -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="row folder"
    class:active
    style:padding-left="{8 + depth * 14}px"
    role="treeitem"
    aria-level={depth + 1}
    aria-selected={active}
    aria-expanded={kids.length ? !collapsed : undefined}
    tabindex="-1"
    onclick={() => store.selectFolder(source, f.id)}
  >
    {#if kids.length}
      <button
        class="twisty"
        aria-label={collapsed ? "Expand" : "Collapse"}
        onclick={(e) => {
          e.stopPropagation();
          store.toggleFolder(source, f.id);
        }}><Icon name={collapsed ? "chevronRight" : "chevronDown"} size={12} /></button
      >
    {:else}
      <span class="twisty"></span>
    {/if}
    <span class="kind"><Icon name={folderIcon(f.kind, f.name)} size={15} /></span>
    <span class="name" class:bold={(f.unread_count ?? 0) > 0} title={f.name}>{f.name}</span>
    {#if (f.unread_count ?? 0) > 0}
      <span class="count unread">{f.unread_count?.toLocaleString()}</span>
    {:else if f.message_count != null && f.message_count > 0}
      <span class="count">{f.message_count.toLocaleString()}</span>
    {/if}
  </div>
  {#if kids.length && !collapsed}
    {#each kids as k (k.id)}
      {@render folderNode(source, k, depth + 1)}
    {/each}
  {/if}
{/snippet}

<aside class="sidebar">
  <header>
    <span class="title">Sources</span>
    <button class="icon-btn" title="Add mail files (PST, OST, mbox, EML, MSG…)" onclick={() => store.addFilesDialog()}>
      <Icon name="filePlus" />
    </button>
    <button class="icon-btn" title="Add a folder" onclick={() => store.addFolderDialog()}
      ><Icon name="folderPlus" /></button
    >
  </header>

  <div class="tree" role="tree" aria-label="Sources" tabindex="-1" bind:this={tree} onkeydown={treeKey}>
    {#each store.sources as s, si (s.id)}
      {@const expanded = store.expandedSources[s.id] ?? false}
      {@const p = pct(s.id)}
      {@const total = store.sourceTotal(s.id)}
      {@const allActive = store.allMail?.source === s.id && !store.search}
      <div class="source-block">
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="row source"
          class:active={allActive}
          role="treeitem"
          aria-level={1}
          aria-selected={allActive}
          aria-expanded={expanded}
          tabindex={si === 0 ? 0 : -1}
          title={`${SOURCE_KIND_LABEL[s.kind]} — ${s.path}\nClick to list all mail`}
          onclick={() => store.selectAllMail(s.id)}
        >
          <button
            class="twisty"
            aria-label={expanded ? "Collapse" : "Expand"}
            onclick={(e) => {
              e.stopPropagation();
              store.toggleSource(s.id);
            }}><Icon name={expanded ? "chevronDown" : "chevronRight"} size={12} /></button
          >
          <span class="kind src"><Icon name={sourceIcon(s.kind)} size={15} /></span>
          <span class="name">{s.display_name}</span>
          {#if store.progress[s.id]?.error}
            <span class="err" title={store.progress[s.id]?.error ?? ""}>!</span>
          {:else if (store.progress[s.id]?.read_errors ?? 0) > 0}
            {@const n = store.progress[s.id]?.read_errors ?? 0}
            <span
              class="warn"
              title="{n.toLocaleString()} message{n === 1 ? '' : 's'} could not be read; indexed from the header only"
              >!</span
            >
          {/if}
          {#if total > 0}<span class="count">{total.toLocaleString()}</span>{/if}
          <button
            class="icon-btn menu-btn"
            title="Source actions"
            aria-haspopup="menu"
            aria-expanded={menu?.source === s.id}
            onclick={(e) => openMenu(e, s.id)}><Icon name="more" /></button
          >
        </div>
        {#if p != null}
          <div class="progress" title="Indexing {p}%"><div style:width="{p}%"></div></div>
        {/if}
        {#if expanded}
          {@const tagActive = store.tagged?.source === s.id}
          {@const tagTotal = store.taggedTotals[s.id]}
          {@const attActive = store.attSource === s.id}
          {@const attTotal = store.attTotals[s.id]}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            class="row folder"
            class:active={tagActive}
            style:padding-left="22px"
            role="treeitem"
            aria-level={2}
            aria-selected={tagActive}
            tabindex="-1"
            title="Tagged emails and attachments in {s.display_name}"
            onclick={() => store.selectTagged(s.id)}
          >
            <span class="twisty"></span>
            <span class="kind"><Icon name="label" size={15} /></span>
            <span class="name">Tagged</span>
            {#if tagTotal != null && tagTotal > 0}
              <span class="count">{tagTotal.toLocaleString()}</span>
            {/if}
          </div>
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            class="row folder"
            class:active={attActive}
            style:padding-left="22px"
            role="treeitem"
            aria-level={2}
            aria-selected={attActive}
            tabindex="-1"
            title="Every attachment in {s.display_name}"
            onclick={() => store.selectAttachments(s.id)}
          >
            <span class="twisty"></span>
            <span class="kind"><Icon name="paperclip" size={15} /></span>
            <span class="name">Attachments</span>
            {#if attTotal != null && attTotal > 0}
              <span class="count">{attTotal.toLocaleString()}</span>
            {/if}
          </div>
          {#if store.folderErrors[s.id]}
            <div class="folder-error">{store.folderErrors[s.id]}</div>
          {:else if !store.folders[s.id]}
            <div class="folder-error loading">Loading…</div>
          {:else}
            {#each trees[s.id]?.get(null) ?? [] as f (f.id)}
              {@render folderNode(s.id, f, 1)}
            {/each}
          {/if}
        {/if}
      </div>
    {/each}
  </div>
</aside>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} onclose={() => (menu = null)}>
    <button role="menuitem" onclick={() => act((id) => store.reindex(id))}><Icon name="refresh" /> Re-index</button>
    <button role="menuitem" class="danger" onclick={() => act((id) => store.removeSource(id))}
      ><Icon name="x" /> Remove from library</button
    >
  </ContextMenu>
{/if}

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-2);
    min-width: 0;
  }
  header {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 8px 8px 6px 12px;
  }
  .title {
    flex: 1;
    font-weight: 600;
    font-size: 11px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-2);
  }
  .tree {
    flex: 1;
    overflow: auto;
    padding-bottom: 12px;
  }
  .source-block {
    position: relative;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 28px;
    padding: 0 6px 0 4px;
    margin: 0 6px;
    border-radius: 6px;
    cursor: default;
    white-space: nowrap;
    outline: none;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.active {
    background: var(--sel);
  }
  .row:focus-visible {
    box-shadow: inset 0 0 0 2px var(--sel-focus);
  }
  .row.source {
    position: relative;
    font-weight: 600;
    margin-top: 4px;
  }
  .row.source .count {
    font-weight: 400;
  }
  .twisty {
    width: 16px;
    height: 16px;
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    padding: 0;
    background: transparent;
    color: var(--fg-3);
    border-radius: 4px;
  }
  button.twisty:hover {
    background: var(--bg-3);
  }
  .kind {
    color: var(--fg-2);
    display: inline-flex;
  }
  .kind.src {
    color: var(--accent);
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .name.bold {
    font-weight: 600;
  }
  .count {
    color: var(--fg-3);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .count.unread {
    color: var(--accent);
    font-weight: 600;
  }
  /* Overlays the count on hover so source counts line up with folder counts. */
  .menu-btn {
    position: absolute;
    right: 2px;
    width: 22px;
    height: 22px;
    opacity: 0;
  }
  .row:hover .menu-btn,
  .menu-btn:focus-visible,
  .menu-btn[aria-expanded="true"] {
    opacity: 1;
    background: var(--hover);
  }
  .warn {
    color: var(--warn);
    font-weight: 700;
    padding: 0 4px;
  }
  .err {
    color: var(--danger);
    font-weight: 700;
    padding: 0 4px;
  }
  .progress {
    height: 2px;
    margin: 0 14px 2px 34px;
    background: var(--bg-3);
    border-radius: 2px;
    overflow: hidden;
  }
  .progress div {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s;
  }
  .danger {
    color: var(--danger);
  }
  .folder-error {
    padding: 4px 12px 4px 40px;
    color: var(--danger);
    font-size: 12px;
  }
  .folder-error.loading {
    color: var(--fg-3);
  }
</style>
