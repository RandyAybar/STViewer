<script lang="ts">
  import { untrack } from "svelte";
  import Icon from "./Icon.svelte";
  import Avatar from "./Avatar.svelte";
  import MessageContent from "./MessageContent.svelte";
  import ConversationView from "./ConversationView.svelte";
  import HeadersView from "./HeadersView.svelte";
  import { api, errMsg } from "../lib/api";
  import { addrFull, fullDateTime, longDate, sameLocator } from "../lib/format";
  import { store } from "../lib/store.svelte";
  import type { Address, ConversationItem, Header, Message } from "../lib/types";

  type Tab = "message" | "thread" | "headers";

  function recipients(h: Header): [string, Address[]][] {
    return [
      ["To", h.to],
      ["Cc", h.cc],
      ["Bcc", h.bcc],
    ];
  }

  const entry = $derived(store.reader.at(-1) ?? null);

  let msg = $state.raw<Message | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  /** `null` while loading. */
  let conv = $state.raw<ConversationItem[] | null>(null);
  let convError = $state<string | null>(null);
  let tab = $state<Tab>("message");
  let showAllRecipients = $state(false);

  // Load the message for the top of the reader stack. Short debounce so holding ↓ in
  // the list does not parse every message it passes.
  $effect(() => {
    const e = entry;
    msg = null;
    error = null;
    showAllRecipients = false;
    convError = null;
    // Keep the timeline while opening one of its own emails (a card's "Open").
    const keep = untrack(() =>
      conv?.some((i) => e != null && i.source === e.source && sameLocator(i.header.locator, e.locator)),
    );
    if (!keep) conv = null;
    if (!e) return;
    loading = true;
    let stale = false;
    const timer = setTimeout(async () => {
      try {
        const m = await api.message(e.source, e.locator);
        if (stale) return;
        msg = m;
        loading = false;
        api
          .conversation(e.source, e.locator)
          .then((c) => !stale && (conv = c))
          .catch((err: unknown) => !stale && (convError = errMsg(err)));
      } catch (err) {
        if (stale) return;
        error = errMsg(err);
        loading = false;
      }
    }, 40);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  });
</script>

<section class="reader">
  {#if !entry}
    <div class="empty muted">
      <Icon name="mail" size={40} />
      <p>Select a message to read it.</p>
    </div>
  {:else}
    <div class="toolbar">
      {#if store.attRow && (store.attachments || store.tagged)}
        <button class="btn back" title="Back to the attachment (Esc)" onclick={() => (store.reader = [])}>
          <Icon name="back" size={13} /> Attachment
        </button>
      {/if}
      {#if store.reader.length > 1}
        <nav class="crumbs">
          {#each store.reader as r, i (i)}
            {#if i > 0}<Icon name="chevronRight" size={12} />{/if}
            {#if i < store.reader.length - 1}
              <button class="crumb" onclick={() => store.popReader(i)}>{r.label}</button>
            {:else}
              <span class="crumb current">{r.label}</span>
            {/if}
          {/each}
        </nav>
      {:else}
        <span class="spacer"></span>
      {/if}
      <div class="tabs" role="tablist">
        <button
          role="tab"
          aria-selected={tab === "message"}
          class:on={tab === "message"}
          onclick={() => (tab = "message")}>Message</button
        >
        <button
          role="tab"
          aria-selected={tab === "thread"}
          class:on={tab === "thread"}
          onclick={() => (tab = "thread")}
        >
          Conversation{conv && conv.length > 1 ? ` (${conv.length})` : ""}
        </button>
        <button
          role="tab"
          aria-selected={tab === "headers"}
          class:on={tab === "headers"}
          onclick={() => (tab = "headers")}>Headers</button
        >
      </div>
      <button
        class="icon-btn"
        title={store.maximized ? "Restore layout (Esc)" : "Expand reading pane (Enter)"}
        onclick={() => (store.maximized = !store.maximized)}
      >
        <Icon name={store.maximized ? "minimize" : "maximize"} />
      </button>
    </div>

    {#if error}
      <div class="empty error selectable">{error}</div>
    {:else if !msg}
      <div class="empty muted">{loading ? "Loading…" : ""}</div>
    {:else}
      {@const h = msg.header}
      {#if tab === "message"}
        <header class="head selectable">
          <h1>{h.subject || "(no subject)"}</h1>
          <!-- A contact has no sender; its card below says who it is. -->
          {#if h.from || h.item?.type !== "contact"}
            <div class="from-row">
              <Avatar addr={h.from} />
              <div class="who">
                <div class="from">{h.from ? addrFull(h.from) : "(unknown sender)"}</div>
                {#each recipients(h) as [label, list] (label)}
                  {#if list.length}
                    <div class="rcpt">
                      <span class="lbl">{label}</span>
                      {#if list.length > 4 && !showAllRecipients}
                        {list.slice(0, 4).map(addrFull).join(", ")}
                        <button class="link" onclick={() => (showAllRecipients = true)}
                          >and {list.length - 4} more</button
                        >
                      {:else}
                        {list.map(addrFull).join(", ")}
                      {/if}
                    </div>
                  {/if}
                {/each}
              </div>
              <div class="date" title={fullDateTime(h.date)}>{longDate(h.date)}</div>
            </div>
          {/if}
          {#if h.labels.length || h.flags.encrypted || h.flags.signed || (h.message_class && h.message_class !== "IPM.Note")}
            <div class="chips">
              {#if h.message_class && h.message_class !== "IPM.Note"}<span class="badge">{h.message_class}</span>{/if}
              {#if h.flags.encrypted}<span class="badge"><Icon name="lock" size={11} /> Encrypted</span>{/if}
              {#if h.flags.signed}<span class="badge"><Icon name="shield" size={11} /> Signed</span>{/if}
              {#each h.labels as l (l)}<span class="badge label">{l}</span>{/each}
            </div>
          {/if}
        </header>

        {#key msg}
          <MessageContent source={entry.source} locator={entry.locator} {msg} />
        {/key}
      {:else if tab === "thread"}
        <div class="scroll">
          <ConversationView items={conv} error={convError} openSource={entry.source} />
        </div>
      {:else}
        <div class="scroll">
          <HeadersView {msg} />
        </div>
      {/if}
    {/if}
  {/if}
</section>

<style>
  .reader {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--bg);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 36px;
    flex: none;
    padding: 0 8px 0 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-2);
  }
  .spacer {
    flex: 1;
  }
  .crumbs {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    overflow: hidden;
    color: var(--fg-3);
  }
  .crumb {
    border: none;
    background: transparent;
    padding: 2px 4px;
    border-radius: 4px;
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--accent);
  }
  .crumb.current {
    color: var(--fg);
  }
  button.crumb:hover {
    background: var(--hover);
  }
  .tabs {
    display: flex;
    gap: 2px;
  }
  .tabs button {
    border: none;
    background: transparent;
    padding: 4px 10px;
    border-radius: 6px;
    color: var(--fg-2);
  }
  .tabs button:hover {
    background: var(--hover);
  }
  .tabs button.on {
    background: var(--bg);
    color: var(--fg);
    box-shadow: 0 0 0 1px var(--border);
  }
  .head {
    padding: 14px 20px 10px;
    border-bottom: 1px solid var(--border);
    flex: none;
    max-height: 40%;
    overflow: auto;
  }
  h1 {
    font-size: 18px;
    font-weight: 600;
    margin: 0 0 10px;
    overflow-wrap: anywhere;
  }
  .from-row {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }
  .who {
    flex: 1;
    min-width: 0;
  }
  .from {
    font-weight: 600;
  }
  .rcpt {
    color: var(--fg-2);
    font-size: 12px;
    overflow-wrap: break-word;
  }
  .lbl {
    color: var(--fg-3);
    margin-right: 4px;
  }
  .date {
    color: var(--fg-2);
    font-size: 12px;
    white-space: nowrap;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 8px;
  }
  .badge.label {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent);
  }
  .scroll {
    flex: 1;
    overflow: auto;
    min-height: 0;
  }
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 24px;
    text-align: center;
  }
  .empty.error {
    color: var(--danger);
  }
</style>
