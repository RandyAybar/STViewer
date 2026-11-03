<script lang="ts">
  import Icon from "./Icon.svelte";
  import VirtualTable from "./VirtualTable.svelte";
  import { store } from "../lib/store.svelte";
  import { messageColumns, type HeaderCol } from "../lib/columns.svelte";
  import { addrShort, dateTime, fullDateTime, lastDay, size, whenFull, whenList } from "../lib/format";
  import { TASK_STATUS_LABEL, labelText } from "../lib/items";
  import type { Row, SortKey } from "../lib/types";

  let table = $state<ReturnType<typeof VirtualTable<Row>>>();

  const list = $derived(store.list);
  const searching = $derived(store.search != null);
  /** "All mail" of a source: compact rows plus a Folder column (rows span folders). */
  const allMail = $derived(searching ? null : store.allMail);
  const allMailProgress = $derived(allMail ? store.progress[allMail.source] : undefined);
  const count = $derived(list?.count ?? 0);
  /** Calendar, contacts and tasks folders list their item fields instead of mail columns. */
  const kind = $derived(store.listKind);
  const widths = $derived(messageColumns[kind]);

  /** Items one PageUp/PageDown moves. */
  export function pageItems(): number {
    return table?.pageItems() ?? 20;
  }

  /** Index snippets are HTML-escaped with `<b>` highlights; allow only those tags through. */
  function safeSnippet(s: string): string {
    return s.replace(/<(?!\/?b>)/gi, "&lt;");
  }

  const columns = $derived.by((): HeaderCol[] => {
    const s = store.activeSort;
    const sortable = (
      id: SortKey,
      label: string,
      track: string,
      min: number,
      extra: Partial<HeaderCol> = {},
    ): HeaderCol => ({
      id,
      label,
      track,
      min,
      ...extra,
      active: s?.key === id,
      direction: s?.key === id ? (s.descending ? "descending" : "ascending") : undefined,
      onsort: () => store.setSort(id),
    });
    const status: HeaderCol = searching
      ? {
          id: "status",
          label: "★",
          track: "22px",
          min: 22,
          fixed: true,
          align: "center",
          title: "Sort by relevance",
          active: store.searchSort == null,
          onsort: () => store.sortByRelevance(),
        }
      : { id: "status", label: "", track: "22px", min: 22, fixed: true };
    const att: HeaderCol = { id: "att", label: "", track: searching ? "0" : "18px", min: 0, fixed: true };
    const subject = (label = "Subject") => sortable("subject", label, "minmax(80px, 2fr)", 80, { flex: true });
    const plain = (id: string, label: string, track: string, extra: Partial<HeaderCol> = {}): HeaderCol => ({
      id,
      label,
      track,
      min: 40,
      ...extra,
    });
    switch (kind) {
      case "calendar":
        return [
          status,
          att,
          subject(),
          sortable("start", "Start", "132px", 40),
          plain("end", "End", "132px"),
          plain("location", "Location", "minmax(60px, 1fr)"),
          plain("organizer", "Organizer", "minmax(60px, 1fr)"),
        ];
      case "contacts":
        return [
          subject("Name"),
          plain("email", "Email", "minmax(80px, 1.2fr)"),
          plain("phone", "Phone", "130px"),
          plain("company", "Company", "minmax(60px, 1fr)"),
        ];
      case "tasks":
        return [
          status,
          att,
          subject(),
          sortable("start", "Due", "132px", 40),
          plain("task_status", "Status", "minmax(70px, 0.6fr)"),
          plain("percent", "% Complete", "84px", { align: "right" }),
        ];
    }
    const cols: HeaderCol[] = [
      status,
      att,
      sortable("from", "From", "minmax(80px, 1fr)", 40),
      subject(),
      sortable("date", "Date", "132px", 40, { align: "right" }),
      sortable("size", "Size", "62px", 40, { align: "right" }),
    ];
    if (allMail) cols.splice(4, 0, { id: "folder", label: "Folder", track: "minmax(60px, 0.7fr)", min: 40 });
    return cols;
  });
</script>

{#snippet placeholder()}
  {#if !list}
    <div class="placeholder muted">Select a folder to list its messages.</div>
  {:else if list.error && count === 0}
    <div class="placeholder error">{list.error}</div>
  {:else if list.loading}
    <div class="placeholder muted">Loading…</div>
  {:else}
    <div class="placeholder muted">
      {searching
        ? "No matching messages."
        : allMail
          ? allMailProgress?.done === false
            ? "No messages indexed yet."
            : "This mailbox has no messages."
          : "This folder is empty."}
    </div>
  {/if}
{/snippet}

{#snippet cells(row: Row)}
  {@const h = row.header}
  {#if searching}
    {@const it = h.item}
    {@const when = it?.type === "appointment" ? it.start : it?.type === "task" ? it.due : null}
    <div class="hit" role="gridcell">
      <div class="line1">
        {#if !h.flags.read}<span class="dot" title="Unread"></span>{/if}
        <span class="from"
          >{addrShort(h.from) ||
            (it?.type === "contact" ? (it.emails[0]?.email ?? it.company ?? "") : "(unknown sender)")}</span
        >
        <span class="badge src" title="Source">{store.sourceName(row.source)}</span>
        {#if h.folder}<span class="badge" title="Folder">{store.folderName(row.source, h.folder)}</span>{/if}
        {#if when}
          <span class="when" title="{it?.type === 'task' ? 'Due' : 'Starts'} {whenFull(when)}"
            ><Icon name={it?.type === "task" ? "tasks" : "calendar"} size={12} />{whenList(when)}</span
          >
        {/if}
        {#if h.has_attachments}<Icon name="paperclip" size={12} title="Has attachments" />{/if}
        {#if h.flags.flagged}<span class="flag" title="Flagged"><Icon name="flag" size={12} /></span>{/if}
        <span class="date" title={fullDateTime(h.date)}>{dateTime(h.date)}</span>
      </div>
      <div class="subject">{h.subject || "(no subject)"}</div>
      <div class="snippet">{@html safeSnippet(row.snippet ?? "")}</div>
    </div>
  {:else if kind === "contacts"}
    {@const c = h.item?.type === "contact" ? h.item : null}
    <span class="c-subject" role="gridcell">{h.subject || c?.display_name || "(no name)"}</span>
    <span role="gridcell" title={c?.emails.map((e) => e.email ?? e.name).join("\n")}>{c?.emails[0]?.email ?? ""}</span>
    <span role="gridcell" title={c?.phones.map((p) => `${labelText(p.label)}: ${p.value}`).join("\n")}
      >{c?.phones[0]?.value ?? ""}</span
    >
    <span role="gridcell">{c?.company ?? ""}</span>
  {:else}
    <span class="c-status" role="gridcell">
      {#if h.flags.flagged}<span class="flag" title="Flagged"><Icon name="flag" size={12} /></span>
      {:else if !h.flags.read}<span class="dot" title="Unread"></span>{/if}
    </span>
    <span class="c-att" role="gridcell"
      >{#if h.has_attachments}<Icon name="paperclip" size={12} title="Has attachments" />{/if}</span
    >
    {#if kind === "calendar"}
      {@const a = h.item?.type === "appointment" ? h.item : null}
      {@const end = lastDay(a?.end)}
      <span class="c-subject" role="gridcell">{h.subject || "(no subject)"}</span>
      <span class="c-when" role="gridcell" title={whenFull(a?.start)}>{whenList(a?.start)}</span>
      <span class="c-when" role="gridcell" title={whenFull(end)}>{whenList(end)}</span>
      <span role="gridcell">{a?.location ?? ""}</span>
      <span role="gridcell" title={a?.organizer?.email ?? ""}>{addrShort(a?.organizer)}</span>
    {:else if kind === "tasks"}
      {@const t = h.item?.type === "task" ? h.item : null}
      <span class="c-subject" role="gridcell">{h.subject || "(no subject)"}</span>
      <span class="c-when" role="gridcell" title={whenFull(t?.due)}>{whenList(t?.due)}</span>
      <span role="gridcell">{t?.status ? TASK_STATUS_LABEL[t.status] : ""}</span>
      <span class="c-size" role="gridcell">{t?.percent_complete != null ? `${t.percent_complete}%` : ""}</span>
    {:else}
      <span class="c-from" role="gridcell" title={h.from?.email ?? ""}>{addrShort(h.from) || addrShort(h.to[0])}</span>
      <span class="c-subject" role="gridcell">{h.subject || "(no subject)"}</span>
      {#if allMail}<span class="c-folder" role="gridcell">{store.folderName(row.source, h.folder)}</span>{/if}
      <span class="c-date" role="gridcell" title={fullDateTime(h.date)}>{dateTime(h.date)}</span>
      <span class="c-size" role="gridcell">{size(h.size)}</span>
    {/if}
  {/if}
{/snippet}

<div class="list" style:--cols={widths.template(columns)}>
  {#if allMail}
    <div class="header">
      <span class="title">{store.sourceName(allMail.source)} — All mail</span>
      {#if !allMail.loading && !allMail.error}<span class="muted">· {count.toLocaleString()} messages</span>{/if}
    </div>
    {#if allMailProgress && !allMailProgress.done}
      <div class="notice">
        <span class="spin"></span>
        Showing {count.toLocaleString()}{allMailProgress.total ? ` of ~${allMailProgress.total.toLocaleString()}` : ""} messages
        — indexing…
      </div>
    {/if}
  {/if}
  <VirtualTable
    bind:this={table}
    {list}
    {columns}
    {widths}
    label={searching ? "Search results" : "Messages"}
    rowHeight={searching ? 66 : 28}
    block={searching}
    {placeholder}
    {cells}
    rowClass={(r) => (r.header.flags.read || kind === "contacts" ? undefined : "unread")}
    onopen={() => (store.maximized = true)}
  />
</div>

<style>
  .list {
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
  .header .title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    padding: 2px 12px 4px;
    font-size: 11.5px;
    color: var(--fg-2);
  }
  :global(.unread) .c-from,
  :global(.unread) .c-subject,
  :global(.unread) .from,
  :global(.unread) .subject {
    font-weight: 600;
  }
  .c-status,
  .c-att {
    display: inline-flex;
    justify-content: center;
    color: var(--fg-3);
  }
  .c-folder {
    color: var(--fg-2);
    font-size: 12px;
  }
  .c-when,
  .c-date,
  .c-size {
    color: var(--fg-2);
    font-variant-numeric: tabular-nums;
    font-size: 12px;
  }
  .c-date,
  .c-size {
    text-align: right;
  }
  .dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
  }
  .flag {
    color: var(--danger);
    display: inline-flex;
  }
  .hit {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .line1 {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    color: var(--fg-3);
  }
  .from {
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 0 1 auto;
  }
  .badge.src {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent);
  }
  .when {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    font-size: 12px;
    color: var(--fg-2);
  }
  .date {
    margin-left: auto;
    font-size: 12px;
    color: var(--fg-2);
    flex: none;
  }
  .subject,
  .snippet {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .snippet {
    color: var(--fg-2);
    font-size: 12px;
  }
</style>
