<script lang="ts">
  import type { Snippet } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import FileTypeIcon from "./FileTypeIcon.svelte";
  import TextCard from "./TextCard.svelte";
  import { store } from "../lib/store.svelte";
  import type { AttachmentList } from "../lib/lists.svelte";
  import { thumbnailUrl } from "../lib/api";
  import { fileName, size } from "../lib/format";
  import { isDocumentRow, isImageRow, openAttachmentRow, typeLabel } from "../lib/attachments";
  import { VirtualScroll } from "../lib/virtual.svelte";
  import type { AttachmentRow } from "../lib/types";

  let {
    list,
    placeholder,
    oncontext,
  }: {
    list: AttachmentList;
    /** Shown instead of tiles while loading / when empty. */
    placeholder: Snippet;
    oncontext: (e: MouseEvent, i: number) => void;
  } = $props();

  const TILE_W = 140;
  const TILE_H = 156;

  const vs: VirtualScroll<AttachmentRow> = new VirtualScroll<AttachmentRow>({
    list: () => list,
    rowHeight: () => TILE_H,
    perRow: (): number => Math.floor(vs.width / TILE_W),
    overscan: 3,
  });
  /** Thumbnail URLs that failed (nothing could draw it, or not now) on mounted tiles → text card or type icon. */
  const failed = new SvelteSet<string>();

  const thumb = (row: AttachmentRow) =>
    isImageRow(row) || isDocumentRow(row) ? thumbnailUrl(row.source, row.message, row.index) : null;

  /** Items one PageUp/PageDown moves. */
  export function pageItems(): number {
    return vs.pageItems();
  }

  /** Tiles per visual row (↑/↓ step). */
  export function columns(): number {
    return vs.perRow;
  }
</script>

<div
  class="viewport"
  bind:this={vs.viewport}
  bind:clientHeight={vs.height}
  bind:clientWidth={vs.width}
  onscroll={vs.onscroll}
  role="listbox"
  aria-label="Attachments"
  aria-multiselectable="true"
  tabindex="-1"
>
  {#if list.loading || vs.count === 0}
    {@render placeholder()}
  {:else}
    <div class="spacer" style:height="{vs.totalHeight}px">
      <div
        class="window"
        style:transform="translateY({vs.offset}px)"
        style:grid-template-columns="repeat({vs.perRow}, minmax(0, 1fr))"
        style:grid-auto-rows="{TILE_H}px"
      >
        {#each vs.items as { i, row } (i)}
          {@const src = row && thumb(row)}
          <div
            class="tile"
            class:sel={store.selection.has(i)}
            class:focus={store.focus === i}
            role="option"
            aria-posinset={i + 1}
            aria-setsize={vs.count}
            aria-selected={store.selection.has(i)}
            tabindex="-1"
            title={row ? `${fileName(row.filename)}\n${size(row.size)}${row.subject ? `\n${row.subject}` : ""}` : ""}
            onclick={(e) => store.clickItem(i, e)}
            ondblclick={() => row && openAttachmentRow(row)}
            oncontextmenu={(e) => oncontext(e, i)}
            onkeydown={(e) => e.key === " " && store.clickItem(i, e)}
          >
            <!-- A failure may be transient (503: renderer busy); a tile scrolled back into view asks again. -->
            <div
              class="thumb"
              {@attach () => () => {
                if (src) failed.delete(src);
              }}
            >
              {#if row}
                {#if src && !failed.has(src)}
                  <img {src} alt="" loading="lazy" draggable="false" onerror={() => failed.add(src)} />
                {:else if isDocumentRow(row)}
                  <TextCard source={row.source} locator={row.message} index={row.index}>
                    {#snippet fallback()}{@render typeIcon(row)}{/snippet}
                  </TextCard>
                {:else}
                  {@render typeIcon(row)}
                {/if}
                {#if row.inline}<span class="badge inline">inline</span>{/if}
              {/if}
            </div>
            {#if row}
              <div class="cap">{fileName(row.filename)}</div>
              <div class="sz">{size(row.size)}</div>
            {:else}
              <div class="cap"><span class="loading-bar"></span></div>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}
</div>

{#snippet typeIcon(row: AttachmentRow)}
  <FileTypeIcon category={row.category} size={44} />
  {#if typeLabel(row)}<span class="ext">{typeLabel(row)}</span>{/if}
{/snippet}

<style>
  .viewport {
    height: 100%;
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
    display: grid;
    will-change: transform;
  }
  .tile {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    margin: 4px;
    padding: 6px 6px 4px;
    border-radius: 8px;
    border: 1px solid transparent;
    cursor: default;
    min-width: 0;
    contain: strict;
  }
  .tile:hover {
    background: var(--hover);
  }
  .tile.sel {
    background: var(--sel);
  }
  .tile.focus {
    border-color: var(--sel-focus);
  }
  .thumb {
    position: relative;
    width: 100%;
    height: 100px;
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    border-radius: 6px;
    background: var(--bg-2);
    overflow: hidden;
  }
  .thumb img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }
  .ext {
    font: 600 10px/1 var(--mono);
    color: var(--fg-3);
    max-width: 90%;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .inline {
    position: absolute;
    top: 4px;
    right: 4px;
  }
  .cap {
    width: 100%;
    margin-top: 4px;
    text-align: center;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sz {
    font-size: 11px;
    color: var(--fg-3);
    font-variant-numeric: tabular-nums;
  }
  .loading-bar {
    display: inline-block;
    width: 80%;
    height: 8px;
    border-radius: 4px;
    background: var(--bg-3);
  }
</style>
