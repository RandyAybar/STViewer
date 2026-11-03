<script lang="ts">
  import { tick } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import ConversationCard from "./ConversationCard.svelte";
  import { store } from "../lib/store.svelte";
  import type { ConversationItem, SourceId } from "../lib/types";

  let {
    items,
    error,
    openSource,
  }: {
    /** `null` while loading. */
    items: ConversationItem[] | null;
    error: string | null;
    openSource: SourceId;
  } = $props();

  let list = $state<HTMLOListElement>();
  const expanded = new SvelteSet<string>();

  const key = (it: ConversationItem) => `${it.source}\u0000${JSON.stringify(it.header.locator)}`;
  const allExpanded = $derived(items != null && items.length > 0 && items.every((it) => expanded.has(key(it))));
  const indexing = $derived(store.sources.some((s) => store.progress[s.id] && !store.progress[s.id].done));

  function toggle(it: ConversationItem) {
    const k = key(it);
    if (expanded.has(k)) expanded.delete(k);
    else expanded.add(k);
  }

  function toggleAll() {
    if (!items) return;
    if (allExpanded) expanded.clear();
    else for (const it of items) expanded.add(key(it));
  }

  function navigate(i: number, dir: -1 | 1) {
    const heads = list?.querySelectorAll<HTMLElement>("[data-card]");
    heads?.[Math.max(0, Math.min(heads.length - 1, i + dir))]?.focus();
  }

  async function revealCurrent() {
    await tick();
    list?.querySelector(".card.current")?.scrollIntoView({ block: "center" });
  }

  // On mount and for each new conversation: bring the highlighted card into view.
  $effect(() => {
    void items;
    void revealCurrent();
  });
</script>

{#if error}
  <div class="empty error selectable">{error}</div>
{:else if items == null}
  <ol class="timeline" aria-busy="true">
    {#each [0, 1, 2] as i (i)}
      <li class="skeleton">
        <span class="sk-avatar"></span>
        <span class="sk-lines"><span></span><span></span><span class="short"></span></span>
      </li>
    {/each}
  </ol>
{:else if items.length <= 1}
  <div class="empty muted">
    This email has no other messages in its conversation.
    {#if indexing}<br /><span class="small">Indexing is still running; more may appear.</span>{/if}
  </div>
{:else}
  <div class="bar">
    <span class="muted">{items.length} emails, oldest first</span>
    <button class="link" onclick={toggleAll}>{allExpanded ? "Collapse all" : "Expand all"}</button>
  </div>
  <ol class="timeline" bind:this={list}>
    {#each items as it, i (key(it))}
      <li>
        <ConversationCard
          item={it}
          {openSource}
          expanded={expanded.has(key(it))}
          ontoggle={() => toggle(it)}
          onnavigate={(dir) => navigate(i, dir)}
        />
      </li>
    {/each}
  </ol>
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 16px 0;
    font-size: 12px;
  }
  .link {
    font-size: 12px;
  }
  .timeline {
    list-style: none;
    margin: 0;
    padding: 10px 16px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .skeleton {
    display: flex;
    gap: 10px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
  }
  .sk-avatar {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background: var(--bg-3);
    flex: none;
  }
  .sk-lines {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .sk-lines span {
    height: 9px;
    border-radius: 4px;
    background: var(--bg-3);
  }
  .sk-lines .short {
    width: 60%;
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-height: 200px;
    padding: 24px;
    text-align: center;
  }
  .empty.error {
    color: var(--danger);
  }
  .small {
    font-size: 12px;
  }
</style>
