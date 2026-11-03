<script lang="ts">
  import { flushSync } from "svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import type { ColumnWidths, HeaderCol } from "../lib/columns.svelte";

  let {
    cols,
    widths,
    rowsRoot,
  }: {
    cols: HeaderCol[];
    widths: ColumnWidths;
    /** Element containing the `.row`s, measured by double-click auto-fit. */
    rowsRoot?: HTMLElement;
  } = $props();

  let row = $state<HTMLDivElement>();
  let cells = $state<HTMLDivElement[]>([]);
  let menu = $state<{ x: number; y: number } | null>(null);

  /**
   * Applies `w` to column `id` unless growing it would push columns past the list's
   * right edge (flexible and fr columns already at their minimum); returns the width kept.
   */
  function apply(id: string, w: number, prev: number): number {
    flushSync(() => widths.set(id, w));
    if (w > prev && row && row.scrollWidth > row.clientWidth + 1) {
      flushSync(() => widths.set(id, prev));
      return prev;
    }
    return w;
  }

  /** Column a handle after column `k` resizes: itself, or the next one after the flexible column. */
  function targetOf(k: number): number | null {
    const c = cols[k];
    if (c.fixed) return null;
    if (!c.flex) return k;
    const next = cols[k + 1];
    return next && !next.fixed && !next.flex ? k + 1 : null;
  }

  function startResize(e: PointerEvent, k: number) {
    const t = targetOf(k);
    const cell = t != null ? cells[t] : undefined;
    if (t == null || !cell || e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const grip = e.currentTarget as HTMLElement;
    const def = cols[t];
    // Dragging the flexible column's edge right grows it by shrinking the next column.
    const sign = t === k ? 1 : -1;
    const startX = e.clientX;
    const startW = cell.getBoundingClientRect().width;
    grip.setPointerCapture(e.pointerId);
    let kept = Math.round(startW);
    const move = (ev: PointerEvent) => {
      kept = apply(def.id, Math.max(def.min, Math.round(startW + sign * (ev.clientX - startX))), kept);
    };
    const end = () => {
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", end);
      grip.removeEventListener("pointercancel", end);
      widths.save();
    };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", end);
    grip.addEventListener("pointercancel", end);
  }

  /** Double-click: fit the column to its widest rendered cell (header included). */
  function autoFit(k: number) {
    const t = targetOf(k);
    if (t == null) return;
    const def = cols[t];
    const rowCells = rowsRoot ? [...rowsRoot.querySelectorAll<HTMLElement>(`.row > :nth-child(${t + 1})`)] : [];
    if (rowCells.length === 0) {
      widths.clear(def.id);
      return;
    }
    const label = cells[t]?.firstElementChild as HTMLElement | null;
    const widest = Math.ceil(Math.max(def.min, label?.scrollWidth ?? 0, ...rowCells.map((c) => c.scrollWidth))) + 2;
    // Grow towards the fitted width in steps so the result never overflows the list.
    let kept = Math.round(cells[t]?.getBoundingClientRect().width ?? def.min);
    if (widest <= kept) kept = apply(def.id, widest, kept);
    else
      while (kept < widest && apply(def.id, Math.min(widest, kept + 8), kept) !== kept)
        kept = Math.min(widest, kept + 8);
    widths.save();
  }

  function openMenu(e: MouseEvent) {
    e.preventDefault();
    menu = { x: e.clientX, y: e.clientY };
  }
</script>

<div class="cols" role="row" aria-rowindex={1} tabindex="-1" bind:this={row} oncontextmenu={openMenu}>
  {#each cols as c, k (c.id)}
    <div class="hcell" bind:this={cells[k]} role="columnheader" aria-sort={c.direction}>
      {#if c.onsort}
        {@const sort = c.onsort}
        <button class="col {c.align ?? ''}" class:active={c.active} title={c.title} onclick={() => sort()}>
          {c.label}
          {#if c.direction}<span class="arrow" aria-hidden="true">{c.direction === "descending" ? "▾" : "▴"}</span>{/if}
        </button>
      {:else}
        <span class="col {c.align ?? ''}" title={c.title}>{c.label}</span>
      {/if}
      {#if targetOf(k) != null}
        <button
          class="grip"
          tabindex="-1"
          aria-label="Resize column"
          title="Drag to resize · double-click to fit"
          onpointerdown={(e) => startResize(e, k)}
          ondblclick={() => autoFit(k)}
        ></button>
      {/if}
    </div>
  {/each}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} onclose={() => (menu = null)}>
    <button
      role="menuitem"
      onclick={() => {
        menu = null;
        widths.reset();
      }}>Reset column widths</button
    >
  </ContextMenu>
{/if}

<style>
  .cols {
    display: grid;
    grid-template-columns: var(--cols);
    align-items: center;
    column-gap: 6px;
    padding: var(--cols-pad, 0 10px 0 6px);
    height: 28px;
    flex: none;
    border-bottom: 1px solid var(--border);
    background: var(--bg-2);
    font-size: 12px;
    color: var(--fg-2);
    outline: none;
  }
  .hcell {
    position: relative;
    display: flex;
    height: 100%;
    min-width: 0;
  }
  .col {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    border: none;
    background: transparent;
    text-align: left;
    padding: 0;
    height: 100%;
    color: inherit;
    overflow: hidden;
    white-space: nowrap;
  }
  .col.right {
    justify-content: flex-end;
  }
  .col.center {
    justify-content: center;
  }
  .col.active {
    color: var(--fg);
    font-weight: 600;
  }
  .arrow {
    font-size: 10px;
  }
  .grip {
    position: absolute;
    top: 4px;
    bottom: 4px;
    right: -6px;
    width: 6px;
    z-index: 1;
    padding: 0;
    border: none;
    background: transparent;
    cursor: col-resize;
    touch-action: none;
  }
  .grip::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    left: 2px;
    width: 2px;
    border-radius: 1px;
    background: transparent;
  }
  .cols:hover .grip::after {
    background: var(--border);
  }
  .grip:hover::after,
  .grip:active::after {
    background: var(--accent);
  }
</style>
