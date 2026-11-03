<script lang="ts" module>
  import { api } from "../lib/api";
  import type { Locator, SourceId } from "../lib/types";

  /** Extracted text per attachment, shared by grid tiles and the preview pane; oldest dropped first. */
  const texts = new Map<string, Promise<string | null>>();
  const TEXT_CACHE_MAX = 500;
  /** Extractions running at once; the rest wait so a scroll through a large grid cannot flood the backend. */
  const MAX_RUNNING = 4;
  let running = 0;
  /** Requests waiting for a slot, oldest first. */
  const waiting: (() => void)[] = [];

  function pump(): void {
    while (running < MAX_RUNNING && waiting.length) waiting.shift()?.();
  }

  function start(key: string, source: SourceId, locator: Locator, index: number): Promise<string | null> {
    running++;
    const p = api
      .attachmentText(source, locator, index)
      .catch(() => null)
      .finally(() => {
        running--;
        pump();
      });
    if (texts.size >= TEXT_CACHE_MAX) texts.delete(texts.keys().next().value as string);
    texts.set(key, p);
    return p;
  }

  /**
   * Text of one attachment. A request still waiting for a slot when `signal` aborts (the
   * tile scrolled away) is dropped without reaching the backend, and its promise never settles.
   */
  function load(source: SourceId, locator: Locator, index: number, signal: AbortSignal): Promise<string | null> {
    const key = `${source}/${JSON.stringify(locator)}/${index}`;
    const hit = texts.get(key);
    if (hit) return hit;
    return new Promise((resolve) => {
      const go = () => resolve(texts.get(key) ?? start(key, source, locator, index));
      signal.addEventListener("abort", () => {
        const i = waiting.indexOf(go);
        if (i >= 0) waiting.splice(i, 1);
      });
      waiting.push(go);
      pump();
    });
  }
</script>

<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    source,
    locator,
    index,
    large = false,
    fallback,
  }: {
    source: SourceId;
    locator: Locator;
    index: number;
    /** Preview-pane size instead of a grid tile. */
    large?: boolean;
    /** Shown while loading and when the attachment has no extractable text. */
    fallback: Snippet;
  } = $props();

  /** Grid tiles must stay on screen this long before asking (skips tiles a fast scroll flies past). */
  const SETTLE_MS = 150;

  let text = $state<string | null>(null);

  $effect(() => {
    const ctl = new AbortController();
    text = null;
    const timer = setTimeout(
      () =>
        void load(source, locator, index, ctl.signal).then((t) => {
          if (!ctl.signal.aborted) text = t?.trim() || null;
        }),
      large ? 0 : SETTLE_MS,
    );
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  });
</script>

{#if text}
  <div class="card" class:large aria-hidden={!large}>{text}</div>
{:else}
  {@render fallback()}
{/if}

<style>
  /* A page, like the rendered previews: dark text on white in both themes. */
  .card {
    width: 100%;
    height: 100%;
    box-sizing: border-box;
    padding: 6px 7px;
    overflow: hidden;
    background: var(--page);
    color: var(--page-fg);
    font: 6.5px/1.3 var(--font);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    text-align: left;
    user-select: none;
    mask-image: linear-gradient(to bottom, #000 75%, transparent);
  }
  .card.large {
    max-height: 60vh;
    min-height: 180px;
    padding: 18px 22px;
    font-size: 12px;
    line-height: 1.45;
    user-select: text;
  }
</style>
