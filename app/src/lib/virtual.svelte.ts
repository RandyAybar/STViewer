import { untrack } from "svelte";
import type { PagedList } from "./lists.svelte";
import { store } from "./store.svelte";

export interface VirtualOptions<T> {
  list: () => PagedList<T> | null;
  /** Height of one visual row, px. */
  rowHeight: () => number;
  /** Items per visual row (icon grids); default 1. */
  perRow?: () => number;
  /** Visual rows rendered above and below the viewport. */
  overscan: number;
}

/**
 * Tallest spacer used, px. Browsers cap element heights (Chromium ≈ 33.5M px, Firefox
 * ≈ 17.9M px); taller lists are scrolled through a spacer of this height instead.
 */
export const MAX_SCROLL_HEIGHT = 15_000_000;

/**
 * Windowing state for a scrolling list over a `PagedList`: only the visible items (plus
 * overscan) are rendered inside a spacer of the full height. Must be created during
 * component initialisation; it registers effects that fetch visible pages, return to the
 * top when the list is replaced and keep `store.focus` scrolled into view.
 *
 * Lists taller than `MAX_SCROLL_HEIGHT` are *scaled*: the spacer is capped and the
 * viewport's `scrollTop` maps linearly onto the virtual offset `virtualTop` over the full
 * list. The wheel, keyboard focus and programmatic scrolls move `virtualTop` exactly (in
 * pixels of the real list); only dragging the scrollbar thumb moves in scaled steps.
 */
export class VirtualScroll<T> {
  /** Bind with `bind:this`. */
  viewport = $state<HTMLDivElement>();
  /** Bind with `bind:clientHeight` / `bind:clientWidth`. */
  height = $state(600);
  width = $state(600);
  /** The viewport's `scrollTop` (spacer coordinates). */
  scrollTop = $state(0);
  /** Offset of the viewport's top edge within the full (unscaled) list, px. */
  virtualTop = $state(0);
  /** `scrollTop` last set programmatically: its scroll event must keep `virtualTop` exact. */
  #expectedScrollTop: number | null = null;

  readonly #opts: VirtualOptions<T>;

  readonly count = $derived.by(() => this.#opts.list()?.count ?? 0);
  readonly rowHeight = $derived.by(() => this.#opts.rowHeight());
  readonly perRow = $derived.by(() => Math.max(1, this.#opts.perRow?.() ?? 1));
  readonly rowCount = $derived(Math.ceil(this.count / this.perRow));
  /** Height of the whole list, px (may exceed what the browser can lay out). */
  readonly listHeight = $derived(this.rowCount * this.rowHeight);
  /** Spacer height, px. */
  readonly totalHeight = $derived(Math.min(this.listHeight, MAX_SCROLL_HEIGHT));
  /** Virtual px per spacer px; 1 unless the list is scaled. */
  readonly #ratio = $derived(
    this.listHeight > MAX_SCROLL_HEIGHT
      ? Math.max(0, this.listHeight - this.height) / Math.max(1, MAX_SCROLL_HEIGHT - this.height)
      : 1,
  );
  /** First rendered visual row. */
  readonly firstRow = $derived.by(() =>
    Math.max(0, Math.floor(this.virtualTop / this.rowHeight) - this.#opts.overscan),
  );
  readonly #lastRow = $derived.by(() =>
    Math.min(this.rowCount, Math.ceil((this.virtualTop + this.height) / this.rowHeight) + this.#opts.overscan),
  );
  /** Rendered item range `[first, last)`. */
  readonly first = $derived(this.firstRow * this.perRow);
  readonly last = $derived(Math.min(this.count, this.#lastRow * this.perRow));
  /** Offset of the rendered window within the spacer, px (equals `firstRow * rowHeight` unless scaled). */
  readonly offset = $derived(this.scrollTop + this.firstRow * this.rowHeight - this.virtualTop);

  /** Rendered items; `row` is `undefined` until its page lands. */
  readonly items = $derived.by(() => {
    const list = this.#opts.list();
    if (!list) return [];
    void list.version; // re-read when a page lands
    const out: { i: number; row: T | undefined }[] = [];
    for (let i = this.first; i < this.last; i++) out.push({ i, row: list.row(i) });
    return out;
  });

  constructor(opts: VirtualOptions<T>) {
    this.#opts = opts;

    // Fetch visible pages, plus the next screenful in the scroll direction.
    $effect(() => {
      const list = this.#opts.list();
      if (list && this.count > 0) list.ensure(this.first, this.last + (this.last - this.first));
    });

    // New list → back to top.
    $effect(() => {
      this.#opts.list();
      untrack(() => this.scrollToVirtual(0));
    });

    // The list grew or shrank (or the viewport resized): keep `virtualTop`, re-derive the
    // spacer position from it and clamp to the new end.
    $effect(() => {
      void this.#ratio;
      void this.listHeight;
      untrack(() => this.scrollToVirtual(this.virtualTop));
    });

    // Keep the focused item visible (keyboard navigation).
    $effect(() => {
      const f = store.focus;
      const vp = this.viewport;
      if (!vp || f < 0) return;
      const h = this.rowHeight;
      const top = Math.floor(f / this.perRow) * h;
      untrack(() => {
        if (top < this.virtualTop) this.scrollToVirtual(top);
        else if (top + h > this.virtualTop + vp.clientHeight) this.scrollToVirtual(top + h - vp.clientHeight);
      });
    });

    // Scaled lists: the wheel moves by real list pixels, not by scaled spacer pixels.
    // (Svelte's `onwheel` is passive, so the listener is added here to cancel the default.)
    $effect(() => {
      const vp = this.viewport;
      if (!vp) return;
      const wheel = (e: WheelEvent) => {
        if (this.#ratio === 1 || e.ctrlKey || e.deltaY === 0) return;
        e.preventDefault();
        const unit = e.deltaMode === 1 ? this.rowHeight : e.deltaMode === 2 ? vp.clientHeight : 1;
        this.scrollToVirtual(this.virtualTop + e.deltaY * unit);
      };
      vp.addEventListener("wheel", wheel, { passive: false });
      return () => vp.removeEventListener("wheel", wheel);
    });
  }

  /** Scrolls so the viewport's top edge is at `top` px of the full list (clamped). */
  scrollToVirtual(top: number): void {
    const max = Math.max(0, this.listHeight - (this.viewport?.clientHeight ?? this.height));
    const v = Math.max(0, Math.min(max, top));
    const vp = this.viewport;
    if (vp) {
      const before = vp.scrollTop;
      vp.scrollTop = v / this.#ratio;
      // Read back: the browser rounds and clamps. No change → no scroll event to absorb.
      this.#expectedScrollTop = vp.scrollTop !== before ? vp.scrollTop : null;
      this.scrollTop = vp.scrollTop;
    } else {
      this.scrollTop = v / this.#ratio;
    }
    this.virtualTop = v;
  }

  /** `onscroll` handler for the viewport. */
  readonly onscroll = (): void => {
    const s = this.viewport?.scrollTop ?? 0;
    const expected = this.#expectedScrollTop;
    this.#expectedScrollTop = null;
    this.scrollTop = s;
    // Our own scroll: `virtualTop` is already exact. Otherwise the user scrolled (thumb,
    // keys on the viewport): map the spacer position onto the list.
    if (expected != null && Math.abs(s - expected) < 1) return;
    this.virtualTop = s * this.#ratio;
  };

  /** Items one PageUp/PageDown moves: the visible rows minus one kept for context. */
  pageItems(): number {
    const rows = Math.floor((this.viewport?.clientHeight ?? this.height) / this.rowHeight);
    return Math.max(1, rows - 1) * this.perRow;
  }
}
