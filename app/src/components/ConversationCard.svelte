<script lang="ts">
  import Avatar from "./Avatar.svelte";
  import MessageContent from "./MessageContent.svelte";
  import { api, errMsg } from "../lib/api";
  import { addrFull, addrShort, dateTime, fullDateTime } from "../lib/format";
  import { store } from "../lib/store.svelte";
  import type { ConversationItem, Message, SourceId } from "../lib/types";

  let {
    item,
    /** Mailbox of the open email; a badge marks entries from other mailboxes. */
    openSource,
    expanded,
    ontoggle,
    onnavigate,
  }: {
    item: ConversationItem;
    openSource: SourceId;
    expanded: boolean;
    ontoggle: () => void;
    /** Arrow keys: move focus to the previous / next card. */
    onnavigate: (dir: -1 | 1) => void;
  } = $props();

  let msg = $state.raw<Message | null>(null);
  let error = $state<string | null>(null);

  const h = $derived(item.header);
  const folder = $derived(store.folderName(item.source, item.header.folder));

  // Load the full email the first time the card is expanded; keep it for re-expanding.
  $effect(() => {
    const loc = item.header.locator;
    if (!expanded || msg || error || !loc) return;
    let stale = false;
    api
      .message(item.source, loc)
      .then((m) => !stale && (msg = m))
      .catch((e: unknown) => !stale && (error = errMsg(e)));
    return () => {
      stale = true;
    };
  });

  function openMain() {
    const loc = h.locator;
    if (!loc) return;
    store.reader = [{ source: item.source, locator: loc, label: h.subject || "(no subject)" }];
  }

  function keydown(e: KeyboardEvent) {
    if (e.target !== e.currentTarget) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") onnavigate(e.key === "ArrowDown" ? 1 : -1);
    else if (e.key === "Enter" || e.key === " ") ontoggle();
    else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<article class="card" class:current={item.current} class:expanded>
  <div
    class="head"
    role="button"
    tabindex="0"
    aria-expanded={expanded}
    data-card
    onclick={ontoggle}
    onkeydown={keydown}
  >
    <Avatar addr={h.from} size={32} />
    <div class="meta">
      <div class="line1">
        <span class="from" title={h.from ? addrFull(h.from) : ""}>{addrShort(h.from) || "(unknown sender)"}</span>
        {#if item.current}<span class="viewing">Viewing</span>{/if}
        {#if item.source !== openSource}<span class="badge" title="Mailbox">{store.sourceName(item.source)}</span>{/if}
        <span class="date" title={fullDateTime(h.date)}>{dateTime(h.date)}</span>
      </div>
      {#if folder || item.also_in.length}
        <div class="line2">
          {folder}
          {#if item.also_in.length}<span class="also" title={item.also_in.join("\n")}
              >· also in: {item.also_in.join(", ")}</span
            >{/if}
        </div>
      {/if}
      {#if !expanded}
        <div class="preview">{item.preview || h.subject || "(no text)"}</div>
      {/if}
    </div>
  </div>
  {#if expanded}
    <div class="body">
      <div class="actions">
        <span class="subject">{h.subject || "(no subject)"}</span>
        {#if !item.current}
          <button class="btn small" onclick={openMain} title="Make this the open email (stays on Conversation)"
            >Open</button
          >
        {/if}
      </div>
      {#if error}
        <div class="state error selectable">{error}</div>
      {:else if !msg || !h.locator}
        <div class="state muted">Loading…</div>
      {:else}
        <MessageContent source={item.source} locator={h.locator} {msg} inline />
      {/if}
    </div>
  {/if}
</article>

<style>
  .card {
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--bg);
    overflow: hidden;
  }
  .card.current {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .head {
    display: flex;
    gap: 10px;
    padding: 10px 12px;
    cursor: pointer;
    outline: none;
  }
  .head:hover {
    background: var(--hover);
  }
  .head:focus-visible {
    box-shadow: inset 0 0 0 2px var(--sel-focus);
  }
  .meta {
    flex: 1;
    min-width: 0;
  }
  .line1 {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  .from {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .viewing {
    flex: none;
    padding: 0 6px;
    border-radius: 999px;
    background: var(--accent);
    color: var(--accent-fg);
    font-size: 10.5px;
    font-weight: 600;
    line-height: 16px;
  }
  .date {
    margin-left: auto;
    flex: none;
    color: var(--fg-2);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }
  .line2 {
    color: var(--fg-2);
    font-size: 11.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .also {
    color: var(--fg-3);
  }
  .preview {
    margin-top: 4px;
    color: var(--fg-2);
    font-size: 12.5px;
    line-height: 1.45;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .body {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--border);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 14px;
  }
  .subject {
    flex: 1;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .btn.small {
    padding: 2px 10px;
    font-size: 12px;
  }
  .state {
    padding: 12px 14px;
  }
  .state.error {
    color: var(--danger);
  }
</style>
