<script lang="ts" generics="T">
  import type { Snippet } from "svelte";
  import ColumnHeader from "./ColumnHeader.svelte";
  import type { ColumnWidths, HeaderCol } from "../lib/columns.svelte";
  import type { PagedList } from "../lib/lists.svelte";
  import { store } from "../lib/store.svelte";
  import { VirtualScroll } from "../lib/virtual.svelte";

  /**
   * Sortable, resizable column header plus a virtualised row list over a `PagedList`.
   * Selection, focus and row clicks go through `store`; parents supply the cells.
   */
  let {
    list,
    columns,
    widths,
    label,
    rowHeight = 28,
    block = false,
    placeholder,
    cells,
    rowClass,
    onopen,
    oncontext,
  }: {
    list: PagedList<T> | null;
    columns: HeaderCol[];
    widths: ColumnWidths;
    /** Accessible name of the grid. */
    label: string;
    rowHeight?: number;
    /** Rows hold one free-form block (search hits) instead of one cell per column. */
    block?: boolean;
    /** Shown instead of rows while there is no list, it is loading, or it is empty. */
    placeholder: Snippet;
    /** One `role="gridcell"` element per column (or one block). */
    cells: Snippet<[T]>;
    rowClass?: (row: T) => string | undefined;
    /** Double-click. */
    onopen?: (row: T) => void;
    oncontext?: (e: MouseEvent, i: number) => void;
  } = $props();

  const vs = new VirtualScroll<T>({ list: () => list, rowHeight: () => rowHeight, overscan: 10 });

  /** Items one PageUp/PageDown moves. */
  export function pageItems(): number {
    return vs.pageItems();
  }
</script>

<div class="table" role="grid" aria-label={label} aria-rowcount={vs.count + 1} aria-multiselectable="true">
  <ColumnHeader cols={columns} {widths} rowsRoot={vs.viewport} />

  <div class="viewport" role="rowgroup" bind:this={vs.viewport} bind:clientHeight={vs.height} onscroll={vs.onscroll}>
    {#if !list || list.loading || vs.count === 0}
      {@render placeholder()}
    {:else}
      <div class="spacer" style:height="{vs.totalHeight}px">
        <div class="window" style:transform="translateY({vs.offset}px)">
          {#each vs.items as { i, row } (i)}
            {@const selected = store.selection.has(i)}
            <div
              class={["row", row && rowClass?.(row), { sel: selected, focus: store.focus === i, block }]}
              style:height="{rowHeight}px"
              role="row"
              aria-rowindex={i + 2}
              aria-selected={selected}
              tabindex="-1"
              onclick={(e) => store.clickItem(i, e)}
              ondblclick={() => row && onopen?.(row)}
              oncontextmenu={(e) => oncontext?.(e, i)}
              onkeydown={(e) => e.key === " " && store.clickItem(i, e)}
            >
              {#if row}
                {@render cells(row)}
              {:else}
                <span class="loading-bar" role="gridcell"></span>
              {/if}
            </div>
          {/each}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .table {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
  }
  .viewport {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    outline: none;
    position: relative;
    contain: strict;
  }
  .spacer {
    position: relative;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    will-change: transform;
  }
  .row {
    display: grid;
    grid-template-columns: var(--cols);
    align-items: center;
    column-gap: 6px;
    width: 100%;
    padding: 0 10px 0 6px;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
    cursor: default;
    white-space: nowrap;
    contain: strict;
  }
  .row.block {
    display: block;
    padding: 6px 12px 6px 10px;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.sel {
    background: var(--sel);
  }
  .row.focus {
    box-shadow: inset 2px 0 0 var(--sel-focus);
  }
  .row > :global(*) {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .loading-bar {
    grid-column: 2 / -2;
    height: 8px;
    border-radius: 4px;
    background: var(--bg-3);
  }
</style>
