<script lang="ts">
  import Icon from "./Icon.svelte";
  import FileTypeIcon from "./FileTypeIcon.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import VirtualTable from "./VirtualTable.svelte";
  import { store } from "../lib/store.svelte";
  import { taggedColumns, type HeaderCol } from "../lib/columns.svelte";
  import { addrShort, dateTime, fileName, fullDateTime, size } from "../lib/format";
  import { openAttachmentRow } from "../lib/attachments";
  import type { TaggedItem } from "../lib/types";

  let table = $state<ReturnType<typeof VirtualTable<TaggedItem>>>();
  let menu = $state.raw<{ x: number; y: number; item: TaggedItem } | null>(null);

  const list = $derived(store.tagged);
  const count = $derived(list?.count ?? 0);

  /** Chips: every tag used in this mailbox with its item count. */
  const chips = $derived.by(() => {
    const m = new Map<string, number>();
    for (const it of list?.all ?? []) for (const t of it.tags) m.set(t, (m.get(t) ?? 0) + 1);
    return [...m].sort((a, b) => a[0].localeCompare(b[0]));
  });

  /** Items one PageUp/PageDown moves. */
  export function pageItems(): number {
    return table?.pageItems() ?? 20;
  }

  const columns: HeaderCol[] = [
    { id: "type", label: "", track: "28px", min: 28, fixed: true },
    { id: "subject", label: "Subject / name", track: "minmax(80px, 2fr)", min: 80, flex: true },
    { id: "from", label: "From", track: "minmax(60px, 1fr)", min: 40 },
    { id: "date", label: "Date", track: "132px", min: 40, align: "right" },
    { id: "size", label: "Size", track: "62px", min: 40, align: "right" },
    { id: "folder", label: "Folder", track: "minmax(50px, 0.8fr)", min: 40 },
    { id: "tags", label: "Tags", track: "minmax(80px, 1fr)", min: 40 },
  ];

  /** Carrying email in the reading pane; for attachments the preview stays one Esc away. */
  function showMessage(it: TaggedItem) {
    if (it.attachment) store.attRow = it.attachment;
    store.reader = [
      { source: it.target.source, locator: it.target.message, label: it.header.subject || "(no subject)" },
    ];
  }

  function open(it: TaggedItem) {
    if (it.attachment) void openAttachmentRow(it.attachment);
    else {
      showMessage(it);
      store.maximized = true;
    }
  }

  async function oncontext(e: MouseEvent, i: number) {
    e.preventDefault();
    const l = list;
    if (!l) return;
    if (!store.selection.has(i)) void store.setFocus(i);
    const item = await l.rowAsync(i);
    if (!item || store.tagged !== l) return;
    menu = { x: e.clientX, y: e.clientY, item };
  }

  /** Closes the menu, then runs `fn` on its item (captured before the menu block unmounts). */
  function act(fn: (item: TaggedItem) => void) {
    const m = menu;
    menu = null;
    if (m) fn(m.item);
  }
</script>

{#snippet placeholder()}
  {#if !list || list.loading}
    <div class="placeholder muted">Loading…</div>
  {:else if list.error && count === 0}
    <div class="placeholder error">{list.error}</div>
  {:else}
    <div class="placeholder muted">
      <Icon name="label" size={32} />
      <p>
        {list.tag ? `Nothing tagged “${list.tag}” in this mailbox.` : "Nothing tagged in this mailbox yet."}<br />
        Select emails or attachments and press <kbd>T</kbd> or the Tag button.
      </p>
    </div>
  {/if}
{/snippet}

{#snippet cells(item: TaggedItem)}
  {@const a = item.attachment}
  {@const h = item.header}
  {@const date = a?.date ?? h.date}
  <span class="c-type" role="gridcell" title={a ? "Attachment" : "Email"}>
    {#if a}<FileTypeIcon category={a.category} size={15} />{:else}<Icon name="mail" size={15} />{/if}
  </span>
  <span class="c-subject" role="gridcell" title={a ? `${fileName(a.filename)}\nin “${h.subject}”` : h.subject}>
    {#if a}{fileName(a.filename)} <span class="in">· {h.subject || "(no subject)"}</span>{:else}{h.subject ||
        "(no subject)"}{/if}
  </span>
  <span class="c-from" role="gridcell" title={h.from?.email ?? ""}>{addrShort(h.from)}</span>
  <span class="c-date" role="gridcell" title={fullDateTime(date)}>{dateTime(date)}</span>
  <span class="c-size" role="gridcell">{size(a?.size ?? h.size)}</span>
  <span class="c-folder" role="gridcell">{store.folderName(item.target.source, h.folder)}</span>
  <span class="c-tags" role="gridcell">
    {#each item.tags as t (t)}<span class="pill" style:--tag={store.tagColor(t)}>{t}</span>{/each}
  </span>
{/snippet}

<div class="tagged" style:--cols={taggedColumns.template(columns)}>
  {#if list}
    <div class="header">
      <span class="title">{store.sourceName(list.source)} — Tagged</span>
      {#if !list.loading && !list.error}<span class="muted"
          >· {count.toLocaleString()} item{count === 1 ? "" : "s"}</span
        >{/if}
    </div>
    <div class="filter-chips">
      <button
        class="filter-chip"
        class:on={list.tag == null}
        aria-pressed={list.tag == null}
        onclick={() => store.selectTagged(list.source, null)}
      >
        All <span class="n">{list.all.length.toLocaleString()}</span>
      </button>
      {#each chips as [name, n] (name)}
        <button
          class="filter-chip"
          class:on={list.tag === name}
          aria-pressed={list.tag === name}
          onclick={() => store.selectTagged(list.source, name)}
        >
          <span class="dot" style:background={store.tagColor(name)}></span>
          {name} <span class="n">{n.toLocaleString()}</span>
        </button>
      {/each}
    </div>
  {/if}
  <VirtualTable
    bind:this={table}
    {list}
    {columns}
    widths={taggedColumns}
    label="Tagged items"
    {placeholder}
    {cells}
    onopen={open}
    {oncontext}
  />
</div>

{#if menu}
  {@const it = menu.item}
  <ContextMenu x={menu.x} y={menu.y} onclose={() => (menu = null)}>
    <button role="menuitem" onclick={() => act(open)}><Icon name="external" size={13} /> Open</button>
    <div class="sub">
      <button role="menuitem" aria-haspopup="menu"
        ><Icon name="label" size={13} /> Remove tag <span class="caret">▸</span></button
      >
      <div class="submenu" role="menu">
        {#each it.tags as t (t)}
          <button role="menuitem" onclick={() => act((x) => void store.setTag([x.target], t, false))}>
            <span class="dot" style:background={store.tagColor(t)}></span>
            {t}
          </button>
        {/each}
      </div>
    </div>
    <button role="menuitem" onclick={() => act(showMessage)}><Icon name="mail" size={13} /> Show message</button>
  </ContextMenu>
{/if}

<style>
  .tagged {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--bg);
  }
  .header {
    display: flex;
    align-items: baseline;
    gap: 6px;
    height: 30px;
    flex: none;
    padding: 6px 12px 0;
    white-space: nowrap;
    overflow: hidden;
  }
  .title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .filter-chips {
    padding: 4px 10px 8px;
  }
  .dot {
    width: 9px;
    height: 9px;
    flex: none;
    border-radius: 50%;
  }
  .c-type {
    display: inline-flex;
    justify-content: center;
    color: var(--fg-2);
  }
  .in {
    color: var(--fg-3);
    font-size: 12px;
  }
  .c-from,
  .c-folder {
    color: var(--fg-2);
    font-size: 12px;
  }
  .c-date,
  .c-size {
    text-align: right;
    color: var(--fg-2);
    font-variant-numeric: tabular-nums;
    font-size: 12px;
  }
  .c-tags {
    display: flex;
    gap: 3px;
  }
  .pill {
    flex: none;
    padding: 0 6px;
    border-radius: 999px;
    font-size: 11px;
    line-height: 16px;
    background: color-mix(in srgb, var(--tag) 22%, transparent);
    color: color-mix(in srgb, var(--tag) 70%, var(--fg));
    border: 1px solid color-mix(in srgb, var(--tag) 45%, transparent);
  }
  kbd {
    font-family: var(--mono);
    font-size: 11px;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
  }
</style>
