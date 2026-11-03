<script lang="ts">
  import type { Snippet } from "svelte";

  /**
   * Pop-up menu at a pointer position, kept inside the window. Closes on outside click,
   * Escape, Tab and window blur; ↑/↓/Home/End move between items. Children are
   * `<button role="menuitem">`s, `<hr>` separators and optional `.sub` > `.submenu` groups.
   */
  let { x, y, onclose, children }: { x: number; y: number; onclose: () => void; children: Snippet } = $props();

  let el = $state<HTMLDivElement>();
  let left = $state(0);
  let top = $state(0);

  // Position (and focus the first item) on open and whenever the menu moves.
  $effect(() => {
    left = x;
    top = y;
    if (!el) return;
    const r = el.getBoundingClientRect();
    left = Math.max(4, Math.min(x, window.innerWidth - r.width - 4));
    top = Math.max(4, Math.min(y, window.innerHeight - r.height - 4));
    items()[0]?.focus({ preventScroll: true });
  });

  /** Items currently visible (closed submenus are `display: none`). */
  function items(): HTMLElement[] {
    return [...(el?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])].filter((b) => b.offsetParent !== null);
  }

  function keydown(e: KeyboardEvent) {
    const all = items();
    const at = all.indexOf(document.activeElement as HTMLElement);
    let next: number;
    switch (e.key) {
      case "Escape":
      case "Tab":
        onclose();
        e.preventDefault();
        e.stopPropagation();
        return;
      case "ArrowDown":
        next = (at + 1) % all.length;
        break;
      case "ArrowUp":
        next = (at <= 0 ? all.length : at) - 1;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = all.length - 1;
        break;
      default:
        // Enter/Space activate the focused item; keep them away from the list shortcuts.
        e.stopPropagation();
        return;
    }
    e.preventDefault();
    e.stopPropagation();
    all[next]?.focus();
  }

  function windowClick(e: MouseEvent) {
    if (!(e.target instanceof Node && el?.contains(e.target))) onclose();
  }

  /** Escape while focus is elsewhere (menu opened without moving focus into it). */
  function windowKey(e: KeyboardEvent) {
    if (e.key !== "Escape" || (e.target instanceof Node && el?.contains(e.target))) return;
    onclose();
    e.stopImmediatePropagation();
    e.preventDefault();
  }
</script>

<svelte:window onclick={windowClick} onkeydowncapture={windowKey} onblur={onclose} />

<div class="ctx" role="menu" tabindex="-1" bind:this={el} style:left="{left}px" style:top="{top}px" onkeydown={keydown}>
  {@render children()}
</div>

<style>
  .ctx,
  .ctx :global(.submenu) {
    min-width: 180px;
    padding: 4px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow);
  }
  .ctx {
    position: fixed;
    z-index: 50;
    outline: none;
  }
  .ctx :global(button) {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 5px 10px;
    border: none;
    border-radius: 5px;
    background: transparent;
    text-align: left;
    white-space: nowrap;
    outline: none;
  }
  .ctx :global(button:hover),
  .ctx :global(button:focus-visible) {
    background: var(--hover);
  }
  .ctx :global(hr) {
    margin: 4px 2px;
    border: none;
    border-top: 1px solid var(--border);
  }
  .ctx :global(.sub) {
    position: relative;
  }
  .ctx :global(.caret) {
    margin-left: auto;
    color: var(--fg-3);
  }
  .ctx :global(.submenu) {
    display: none;
    position: absolute;
    left: 100%;
    top: -4px;
    min-width: 160px;
  }
  .ctx :global(.sub:hover .submenu),
  .ctx :global(.sub:focus-within .submenu) {
    display: block;
  }
</style>
