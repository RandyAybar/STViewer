<script lang="ts">
  import type { Snippet } from "svelte";
  import FileTypeIcon from "./FileTypeIcon.svelte";
  import VirtualTable from "./VirtualTable.svelte";
  import { attachmentColumns, type HeaderCol } from "../lib/columns.svelte";
  import { store } from "../lib/store.svelte";
  import type { AttachmentList } from "../lib/lists.svelte";
  import { addrShort, dateTime, fileName, fullDateTime, size } from "../lib/format";
  import { openAttachmentRow, typeLabel } from "../lib/attachments";
  import type { AttachmentRow, AttachmentSortKey } from "../lib/types";

  let {
    list,
    placeholder,
    oncontext,
  }: {
    list: AttachmentList;
    /** Shown instead of rows while loading / when empty. */
    placeholder: Snippet;
    oncontext: (e: MouseEvent, i: number) => void;
  } = $props();

  let table = $state<ReturnType<typeof VirtualTable<AttachmentRow>>>();

  /** Items one PageUp/PageDown moves. */
  export function pageItems(): number {
    return table?.pageItems() ?? 20;
  }

  const columns = $derived.by((): HeaderCol[] => {
    const s = store.attPrefs.sort;
    const sortable = (
      id: AttachmentSortKey,
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
      active: s.key === id,
      direction: s.key === id ? (s.descending ? "descending" : "ascending") : undefined,
      onsort: () => store.setAttSort(id),
    });
    return [
      sortable("name", "Name", "minmax(80px, 2.2fr)", 80, { flex: true }),
      sortable("type", "Type", "52px", 40),
      sortable("size", "Size", "60px", 40, { align: "right" }),
      sortable("date", "Date", "132px", 40, { align: "right" }),
      sortable("from", "From", "minmax(60px, 1fr)", 40),
      { id: "subject", label: "Subject", track: "minmax(60px, 1.4fr)", min: 60 },
      { id: "folder", label: "Folder", track: "minmax(50px, 0.8fr)", min: 40 },
    ];
  });
</script>

{#snippet cells(row: AttachmentRow)}
  <span class="c-name" role="gridcell" title={fileName(row.filename)}>
    <FileTypeIcon category={row.category} size={15} />
    <span class="nm">{fileName(row.filename)}</span>
    {#if row.inline}<span class="badge">inline</span>{/if}
  </span>
  <span class="c-type" role="gridcell" title={row.mime_type ?? ""}>{typeLabel(row)}</span>
  <span class="c-size" role="gridcell">{size(row.size)}</span>
  <span class="c-date" role="gridcell" title={fullDateTime(row.date)}>{dateTime(row.date)}</span>
  <span class="c-from" role="gridcell" title={row.from?.email ?? ""}>{addrShort(row.from)}</span>
  <span class="c-subject" role="gridcell" title={row.subject}>{row.subject || "(no subject)"}</span>
  <span class="c-folder" role="gridcell">{store.folderName(row.source, row.folder)}</span>
{/snippet}

<div class="table" style:--cols={attachmentColumns.template(columns)}>
  <VirtualTable
    bind:this={table}
    {list}
    {columns}
    widths={attachmentColumns}
    label="Attachments"
    {placeholder}
    {cells}
    onopen={openAttachmentRow}
    {oncontext}
  />
</div>

<style>
  .table {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
  }
  .c-name {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .nm {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .c-type,
  .c-folder,
  .c-subject {
    color: var(--fg-2);
    font-size: 12px;
  }
  .c-type {
    font-family: var(--mono);
    font-size: 11px;
  }
  .c-date,
  .c-size {
    text-align: right;
    color: var(--fg-2);
    font-variant-numeric: tabular-nums;
    font-size: 12px;
  }
</style>
