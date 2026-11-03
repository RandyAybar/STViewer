<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import Icon from "./Icon.svelte";

  /**
   * Modal dialog: backdrop, titled header with a close button, scrolling body. Focus moves
   * into the dialog on open, Tab cycles inside it, and returns to the previous element on
   * close. Escape or a backdrop click call `onclose`; keys never reach the list shortcuts.
   */
  let {
    title,
    onclose,
    width = "560px",
    children,
  }: {
    title: string;
    onclose: () => void;
    /** CSS width; capped to the window. */
    width?: string;
    children: Snippet;
  } = $props();

  const titleId = $props.id();
  let el = $state<HTMLDivElement>();

  const FOCUSABLE =
    "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea, a[href], [tabindex='0']";

  function focusable(): HTMLElement[] {
    return [...(el?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])].filter((x) => x.offsetParent !== null);
  }

  onMount(() => {
    const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    // `[data-autofocus]`, else the first control after the close button, else the dialog itself.
    const all = focusable();
    (el?.querySelector<HTMLElement>("[data-autofocus]") ?? all.find((x) => !x.closest("header")) ?? el)?.focus();
    return () => prev?.focus();
  });

  function keydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    } else if (e.key === "Tab") {
      const all = focusable();
      if (all.length === 0) return;
      const first = all[0];
      const last = all[all.length - 1];
      if (e.shiftKey && (document.activeElement === first || document.activeElement === el)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
    e.stopPropagation();
  }
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby={titleId}
    tabindex="-1"
    style:width
    bind:this={el}
    onkeydown={keydown}
  >
    <header>
      <h2 id={titleId}>{title}</h2>
      <button class="icon-btn" onclick={onclose} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      {@render children()}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.35);
  }
  .dialog {
    max-width: calc(100vw - 48px);
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: var(--shadow);
    outline: none;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 18px;
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 15px;
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 14px 18px 18px;
  }
</style>
