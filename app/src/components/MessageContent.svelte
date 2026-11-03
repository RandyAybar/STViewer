<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { SvelteSet } from "svelte/reactivity";
  import Icon from "./Icon.svelte";
  import ItemCard from "./ItemCard.svelte";
  import MessageBody from "./MessageBody.svelte";
  import { api, errMsg, thumbnailUrl } from "../lib/api";
  import { openAttachment, opensAsMessage, reportSaved, saveAttachmentAs } from "../lib/attachments";
  import { fileName, size } from "../lib/format";
  import { normalizeCid, prepareHtml } from "../lib/html";
  import { store } from "../lib/store.svelte";
  import type { AttachmentMeta, Locator, Message, SourceId } from "../lib/types";

  /**
   * Calendar/contact/task details, attachment chips, remote-content / HTML-text bar and
   * body of one message. Key the component on the message so per-message choices reset.
   */
  let {
    source,
    locator,
    msg,
    inline = false,
  }: {
    source: SourceId;
    locator: Locator;
    msg: Message;
    /** Compact, content-sized layout for conversation cards. */
    inline?: boolean;
  } = $props();

  let allowRemote = $state(false);
  let preferText = $state(false);
  /** Attachment indexes whose thumbnail failed (not a decodable image) → file icon. */
  const noThumb = new SvelteSet<number>();

  const prepared = $derived(msg.body.html ? prepareHtml(msg.body.html, source, locator, allowRemote) : null);
  const bodyMode = $derived<"html" | "text">(prepared && !(preferText && msg.body.text) ? "html" : "text");

  const visibleAttachments = $derived.by(() => {
    const used = prepared?.usedCids ?? new Set<string>();
    return msg.attachments.filter((a) => !(a.content_id && used.has(normalizeCid(a.content_id))));
  });

  /** Display name (bidi controls escaped); also the suggested name when saving. */
  function attName(a: AttachmentMeta): string {
    return a.filename
      ? fileName(a.filename)
      : a.kind === "embedded_message"
        ? "Attached message"
        : `attachment-${a.index}`;
  }

  function isImage(a: AttachmentMeta): boolean {
    return (
      /^image\/(png|jpe?g|gif|bmp|webp)$/i.test(a.mime_type ?? "") ||
      /\.(png|jpe?g|gif|bmp|webp)$/i.test(a.filename ?? "")
    );
  }

  function openAtt(a: AttachmentMeta) {
    if (opensAsMessage(a)) store.openEmbedded(source, locator, a.index, attName(a));
    else void openAttachment(source, locator, a.index, attName(a));
  }

  async function saveAll() {
    const dir = await open({ directory: true, title: "Save all attachments to…" });
    if (!dir) return;
    try {
      reportSaved(await api.saveAttachments(source, locator, dir));
    } catch (e) {
      store.toast(`Could not save attachments: ${errMsg(e)}`, "error");
    }
  }
</script>

{#if msg.header.item}<ItemCard item={msg.header.item} {inline} />{/if}

{#if visibleAttachments.length}
  <div class="atts" class:inline>
    {#each visibleAttachments as a (a.index)}
      {@const embedded = opensAsMessage(a)}
      <div class="att" title="{attName(a)} — {size(a.size)}{a.mime_type ? ` — ${a.mime_type}` : ''}">
        {#if isImage(a) && !noThumb.has(a.index)}
          <img
            class="thumb"
            src={thumbnailUrl(source, locator, a.index)}
            alt=""
            loading="lazy"
            onerror={() => noThumb.add(a.index)}
          />
        {:else}
          <span class="att-icon"><Icon name={embedded ? "mail" : "file"} size={16} /></span>
        {/if}
        <button class="att-name" onclick={() => openAtt(a)}>
          <span class="nm">{attName(a)}</span>
          <span class="sz">{embedded ? "message" : size(a.size)}</span>
        </button>
        <button
          class="icon-btn small"
          title={embedded ? "Open message" : "Open with default app"}
          aria-label={embedded ? `Open message ${attName(a)}` : `Open ${attName(a)}`}
          onclick={() => openAtt(a)}
        >
          <Icon name={embedded ? "chevronRight" : "external"} size={13} />
        </button>
        <button
          class="icon-btn small"
          title="Save as…"
          aria-label="Save {attName(a)}"
          onclick={() => saveAttachmentAs(source, locator, a.index, attName(a))}
          ><Icon name="download" size={13} /></button
        >
      </div>
    {/each}
    {#if visibleAttachments.length > 1}
      <button class="btn save-all" onclick={saveAll}><Icon name="download" size={13} /> Save all</button>
    {/if}
  </div>
{/if}

{#if prepared && ((prepared.remoteCount > 0 && !allowRemote) || msg.body.text)}
  <div class="notice" class:inline>
    {#if prepared.remoteCount > 0 && !allowRemote}
      <span><Icon name="image" size={13} /> Remote content blocked to protect your privacy.</span>
      <button class="link" onclick={() => (allowRemote = true)}>Load remote images</button>
    {/if}
    <span class="grow"></span>
    {#if msg.body.text}
      <div class="seg">
        <button class:on={bodyMode === "html"} aria-pressed={bodyMode === "html"} onclick={() => (preferText = false)}
          >HTML</button
        >
        <button class:on={bodyMode === "text"} aria-pressed={bodyMode === "text"} onclick={() => (preferText = true)}
          >Text</button
        >
      </div>
    {/if}
  </div>
{/if}

<MessageBody body={msg.body} {prepared} mode={bodyMode} autoHeight={inline} />

<style>
  .atts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 8px 20px;
    border-bottom: 1px solid var(--border);
    flex: none;
    max-height: 120px;
    overflow: auto;
  }
  .atts.inline {
    padding: 6px 14px;
    max-height: none;
  }
  .att {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 4px 2px 6px;
    border: 1px solid var(--border);
    border-radius: 8px;
    max-width: 280px;
    background: var(--bg-2);
  }
  .thumb {
    width: 24px;
    height: 24px;
    object-fit: cover;
    border-radius: 4px;
  }
  .att-icon {
    color: var(--fg-2);
    display: inline-flex;
  }
  .att-name {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    border: none;
    background: transparent;
    padding: 0 4px;
    min-width: 0;
    text-align: left;
  }
  .nm {
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .sz {
    font-size: 11px;
    color: var(--fg-3);
  }
  .icon-btn.small {
    width: 22px;
    height: 22px;
  }
  .save-all {
    align-self: center;
    font-size: 12px;
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 20px;
    font-size: 12px;
    background: var(--bg-2);
    border-bottom: 1px solid var(--border);
    color: var(--fg-2);
    flex: none;
  }
  .notice.inline {
    flex-wrap: wrap;
    row-gap: 2px;
    padding: 4px 14px;
  }
  .link {
    white-space: nowrap;
  }
  .grow {
    flex: 1;
  }
  .seg {
    display: flex;
    gap: 2px;
  }
  .seg button {
    border: none;
    background: transparent;
    padding: 2px 8px;
    border-radius: 6px;
    font-size: 12px;
    color: var(--fg-2);
  }
  .seg button:hover {
    background: var(--hover);
  }
  .seg button.on {
    background: var(--bg);
    color: var(--fg);
    box-shadow: 0 0 0 1px var(--border);
  }
</style>
