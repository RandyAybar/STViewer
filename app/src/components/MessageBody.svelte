<script lang="ts">
  import { openExternal } from "../lib/external";
  import { linkify, rtfToText } from "../lib/format";
  import type { PreparedHtml } from "../lib/html";
  import type { Body } from "../lib/types";

  let {
    body,
    prepared,
    mode,
    autoHeight = false,
  }: {
    body: Body;
    prepared: PreparedHtml | null;
    mode: "html" | "text";
    /** Size to the content instead of filling and scrolling (conversation cards). */
    autoHeight?: boolean;
  } = $props();

  let frame = $state<HTMLIFrameElement>();
  let frameH = $state(0);
  /** Follows the auto-height frame's content; replaced on every (re)load. */
  let resize: ResizeObserver | undefined;
  $effect(() => () => resize?.disconnect());

  const text = $derived(body.text ?? (body.rtf ? rtfToText(body.rtf) : ""));

  const NAV_KEYS: Record<string, true> = {
    ArrowUp: true,
    ArrowDown: true,
    j: true,
    k: true,
    Escape: true,
    Enter: true,
    PageUp: true,
    PageDown: true,
    Home: true,
    End: true,
  };

  /** Routes link clicks to the OS browser and list keys back to the app window. */
  function hookFrame() {
    resize?.disconnect();
    resize = undefined;
    const doc = frame?.contentDocument;
    if (!doc) return;
    if (autoHeight) {
      const fit = () => (frameH = doc.documentElement.scrollHeight);
      fit();
      // Images and fonts change the height after load.
      resize = new ResizeObserver(fit);
      resize.observe(doc.documentElement);
    }
    doc.addEventListener("click", (e) => {
      // Any anchor, including SVG `<a xlink:href>`: the frame itself must never navigate.
      // The target lives in the frame's realm, so `instanceof Element` (this window's) is always false.
      const t = e.target as Element | null;
      const a = typeof t?.closest === "function" ? t.closest("a, area") : null;
      if (!a) return;
      const href = (a.getAttribute("href") ?? a.getAttributeNS("http://www.w3.org/1999/xlink", "href") ?? "").trim();
      if (href.startsWith("#")) return;
      e.preventDefault();
      openExternal(href);
    });
    doc.addEventListener("keydown", (e) => {
      const ctrl = e.ctrlKey || e.metaKey;
      if (!Object.hasOwn(NAV_KEYS, e.key) && !(ctrl && /^[fe]$/i.test(e.key))) return;
      e.preventDefault();
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: e.key, ctrlKey: e.ctrlKey, shiftKey: e.shiftKey, metaKey: e.metaKey }),
      );
    });
  }

  function textClick(e: MouseEvent) {
    const a = e.target instanceof Element ? e.target.closest("a[data-href]") : null;
    if (!a) return;
    e.preventDefault();
    openExternal(a.getAttribute("data-href") ?? "");
  }
</script>

{#if mode === "html" && prepared}
  <iframe
    bind:this={frame}
    class="html"
    class:auto={autoHeight}
    style:height={autoHeight ? `${frameH}px` : undefined}
    title="Message body"
    sandbox="allow-same-origin"
    referrerpolicy="no-referrer"
    srcdoc={prepared.doc}
    onload={hookFrame}
  ></iframe>
{:else if text}
  <!-- linkify() escapes all text and emits only <a>/<span> markup; clicks are delegated to its links -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="text selectable" class:auto={autoHeight} role="document" onclick={textClick} onkeydown={() => {}}>
    {@html linkify(text)}
  </div>
{:else}
  <div class="none muted">This message has no body.</div>
{/if}

<style>
  .html {
    flex: 1;
    width: 100%;
    min-height: 0;
    border: none;
    background: var(--page);
  }
  .html.auto,
  .text.auto {
    flex: none;
  }
  .text.auto {
    overflow: visible;
    padding: 10px 14px;
  }
  .text {
    flex: 1;
    overflow: auto;
    padding: 16px 20px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font: 13.5px/1.5 var(--font);
  }
  .text :global(a) {
    color: var(--accent);
  }
  .text :global(.quote) {
    color: var(--fg-2);
  }
  .none {
    padding: 24px;
  }
</style>
