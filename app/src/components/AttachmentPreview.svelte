<script lang="ts">
  import { SvelteSet } from "svelte/reactivity";
  import Icon from "./Icon.svelte";
  import FileTypeIcon from "./FileTypeIcon.svelte";
  import TextCard from "./TextCard.svelte";
  import { attachmentUrl, previewUrl, thumbnailUrl } from "../lib/api";
  import { addrFull, fileName, longDate, size } from "../lib/format";
  import { CATEGORY_LABEL } from "../lib/icons";
  import type { AttachmentRow } from "../lib/types";
  import {
    isDocumentRow,
    isImageRow,
    openAttachmentRow,
    opensAsMessage,
    saveAttachmentRow,
    saveAttachmentRows,
    typeLabel,
  } from "../lib/attachments";
  import { store } from "../lib/store.svelte";

  const KIND_LABEL: Record<string, string> = {
    embedded_message: "Attached message",
    tnef: "TNEF (winmail.dat)",
    link: "Link",
    ole: "OLE object",
  };

  const row = $derived(store.attRow);
  /** Picture URLs that failed to load: full image → thumbnail, rendered page → text card, then the type icon. */
  const failed = new SvelteSet<string>();
  let saving = $state(false);
  /** Multi-select actions exist only in the attachments view (Tagged rows mix emails). */
  const multi = $derived(store.attachments != null && store.selection.size > 1);

  const imageSrc = $derived.by(() => {
    const r = row;
    if (!r) return null;
    const candidates = isImageRow(r)
      ? [attachmentUrl(r.source, r.message, r.index), thumbnailUrl(r.source, r.message, r.index)]
      : isDocumentRow(r)
        ? [previewUrl(r.source, r.message, r.index)]
        : [];
    return candidates.find((u) => !failed.has(u)) ?? null;
  });

  async function saveSelected() {
    saving = true;
    try {
      await saveAttachmentRows(await store.selectedAttachments());
    } finally {
      saving = false;
    }
  }
</script>

<section class="preview">
  <div class="toolbar">
    <span class="title">{multi ? `${store.selection.size.toLocaleString()} attachments selected` : "Attachment"}</span>
    {#if multi}
      <button class="btn" disabled={saving} onclick={saveSelected}
        ><Icon name="download" size={13} /> Save selected…</button
      >
    {/if}
    <button
      class="icon-btn"
      title={store.maximized ? "Restore layout (Esc)" : "Expand preview"}
      onclick={() => (store.maximized = !store.maximized)}
    >
      <Icon name={store.maximized ? "minimize" : "maximize"} />
    </button>
  </div>

  {#if !row}
    <div class="empty muted">
      <Icon name="paperclip" size={40} />
      <p>Select an attachment to preview it.</p>
    </div>
  {:else}
    <div class="scroll">
      <div class="stage" class:image={imageSrc != null}>
        {#if imageSrc}
          <img src={imageSrc} alt={fileName(row.filename)} draggable="false" onerror={() => failed.add(imageSrc)} />
        {:else if isDocumentRow(row)}
          <TextCard source={row.source} locator={row.message} index={row.index} large>
            {#snippet fallback()}{@render bigIcon(row)}{/snippet}
          </TextCard>
        {:else}
          {@render bigIcon(row)}
        {/if}
      </div>

      <div class="actions">
        <button class="btn primary" onclick={() => openAttachmentRow(row)}>
          <Icon name={opensAsMessage(row) ? "mail" : "external"} size={13} />
          {opensAsMessage(row) ? "Open message" : "Open"}
        </button>
        <button class="btn" onclick={() => saveAttachmentRow(row)}><Icon name="download" size={13} /> Save…</button>
        <button class="btn" onclick={() => store.showCarryingMessage(row)}
          ><Icon name="mail" size={13} /> Show message</button
        >
      </div>

      <h1 class="selectable">{fileName(row.filename)}</h1>
      <dl class="meta selectable">
        <dt>Type</dt>
        <dd>
          {[
            ...new Set([CATEGORY_LABEL[row.category].replace(/s$/, ""), typeLabel(row), row.mime_type].filter(Boolean)),
          ].join(" · ")}
        </dd>
        <dt>Size</dt>
        <dd>{size(row.size)}{row.size >= 1024 ? ` (${row.size.toLocaleString()} bytes)` : ""}</dd>
        <dt>Inline</dt>
        <dd>{row.inline ? "Yes — shown inside the message body" : "No"}</dd>
        {#if KIND_LABEL[row.kind]}
          <dt>Kind</dt>
          <dd>{KIND_LABEL[row.kind]}</dd>
        {/if}
      </dl>

      <h2>Message</h2>
      <dl class="meta selectable">
        <dt>Subject</dt>
        <dd>{row.subject || "(no subject)"}</dd>
        <dt>From</dt>
        <dd>{row.from ? addrFull(row.from) : "(unknown sender)"}</dd>
        <dt>Date</dt>
        <dd>{longDate(row.date)}</dd>
        {#if row.folder}
          <dt>Folder</dt>
          <dd>{store.folderName(row.source, row.folder)}</dd>
        {/if}
        <dt>Mailbox</dt>
        <dd>{store.sourceName(row.source)}</dd>
      </dl>
    </div>
  {/if}
</section>

{#snippet bigIcon(r: AttachmentRow)}
  <div class="big-icon">
    <FileTypeIcon category={r.category} size={96} />
    {#if typeLabel(r)}<span class="ext">{typeLabel(r)}</span>{/if}
  </div>
{/snippet}

<style>
  .preview {
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
    padding: 0 8px 0 14px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-2);
  }
  .title {
    flex: 1;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scroll {
    flex: 1;
    overflow: auto;
    min-height: 0;
    padding: 16px 20px 24px;
  }
  .stage {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 180px;
    max-height: 60vh;
    border-radius: 10px;
    background: var(--bg-2);
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .stage.image {
    background-color: var(--bg-2);
    background-image:
      linear-gradient(45deg, var(--bg-3) 25%, transparent 25%),
      linear-gradient(-45deg, var(--bg-3) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, var(--bg-3) 75%),
      linear-gradient(-45deg, transparent 75%, var(--bg-3) 75%);
    background-size: 16px 16px;
    background-position:
      0 0,
      0 8px,
      8px -8px,
      -8px 0;
  }
  .stage img {
    display: block;
    max-width: 100%;
    max-height: 60vh;
    object-fit: contain;
  }
  .big-icon {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 28px;
  }
  .ext {
    font: 600 13px/1 var(--mono);
    color: var(--fg-2);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 12px;
  }
  h1 {
    font-size: 17px;
    font-weight: 600;
    margin: 16px 0 8px;
    overflow-wrap: anywhere;
  }
  h2 {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-2);
    margin: 18px 0 6px;
  }
  .meta {
    display: grid;
    grid-template-columns: 80px 1fr;
    gap: 4px 10px;
    margin: 0;
    font-size: 12.5px;
  }
  dt {
    color: var(--fg-3);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
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
</style>
