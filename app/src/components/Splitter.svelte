<script lang="ts">
  /** Vertical drag handle resizing the pane on its left. */
  let {
    value = $bindable(),
    min,
    max,
    onchange,
  }: { value: number; min: number; max: number; onchange?: (v: number) => void } = $props();

  let dragging = $state(false);
  let startX = 0;
  let startV = 0;

  function down(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement; // the handle element itself
    el.setPointerCapture(e.pointerId);
    dragging = true;
    startX = e.clientX;
    startV = value;
  }
  function move(e: PointerEvent) {
    if (!dragging) return;
    value = Math.round(Math.max(min, Math.min(max, startV + e.clientX - startX)));
  }
  function up() {
    if (!dragging) return;
    dragging = false;
    onchange?.(value);
  }
  function key(e: KeyboardEvent) {
    const step = e.shiftKey ? 40 : 10;
    if (e.key === "ArrowLeft") value = Math.max(min, value - step);
    else if (e.key === "ArrowRight") value = Math.min(max, value + step);
    else return;
    e.preventDefault();
    e.stopPropagation();
    onchange?.(value);
  }
</script>

<!-- A focusable separator is an interactive ARIA widget (arrow keys resize); Svelte treats every separator as static. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
<div
  class="splitter"
  class:dragging
  role="separator"
  aria-orientation="vertical"
  aria-label="Resize pane"
  aria-valuenow={value}
  aria-valuemin={min}
  aria-valuemax={max}
  tabindex="0"
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  onkeydown={key}
></div>

<style>
  .splitter {
    flex: none;
    width: 5px;
    margin: 0 -2px;
    z-index: 5;
    cursor: col-resize;
    position: relative;
    outline: none;
  }
  .splitter::after {
    content: "";
    position: absolute;
    inset: 0 2px;
    background: var(--border);
  }
  .splitter:hover::after,
  .splitter:focus-visible::after,
  .splitter.dragging::after {
    inset: 0 1px;
    background: var(--accent);
  }
</style>
