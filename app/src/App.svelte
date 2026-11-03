<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import Sidebar from "./components/Sidebar.svelte";
  import MessageList from "./components/MessageList.svelte";
  import ReadingPane from "./components/ReadingPane.svelte";
  import AttachmentsView from "./components/AttachmentsView.svelte";
  import AttachmentPreview from "./components/AttachmentPreview.svelte";
  import TaggedView from "./components/TaggedView.svelte";
  import TagButton from "./components/TagButton.svelte";
  import ManageTagsDialog from "./components/ManageTagsDialog.svelte";
  import SearchBar from "./components/SearchBar.svelte";
  import Facets from "./components/Facets.svelte";
  import StatusBar from "./components/StatusBar.svelte";
  import Splitter from "./components/Splitter.svelte";
  import ExportDialog from "./components/ExportDialog.svelte";
  import SettingsDialog from "./components/SettingsDialog.svelte";
  import Icon from "./components/Icon.svelte";
  import { store } from "./lib/store.svelte";
  import { openAttachmentRow } from "./lib/attachments";
  import { shortcut } from "./lib/format";

  function stored(key: string, fallback: number): number {
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v > 0 ? v : fallback;
  }

  let leftW = $state(stored("stv.leftW", 260));
  let listW = $state(stored("stv.listW", 560));
  let facetW = $state(stored("stv.facetW", 200));
  let dropping = $state(false);
  let searchBar = $state<ReturnType<typeof SearchBar>>();
  let messageList = $state<ReturnType<typeof MessageList>>();
  let attachmentsView = $state<ReturnType<typeof AttachmentsView>>();
  let taggedView = $state<ReturnType<typeof TaggedView>>();
  let tagButton = $state<ReturnType<typeof TagButton>>();

  /** Attachment preview fills the reading pane (attachments view, or an attachment row in Tagged). */
  const showPreview = $derived(
    store.reader.length === 0 && (store.attachments != null || (store.tagged != null && store.attRow != null)),
  );

  onMount(() => {
    void store.init();
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        if (e.payload.type === "enter" || e.payload.type === "over") dropping = true;
        else if (e.payload.type === "leave") dropping = false;
        else if (e.payload.type === "drop") {
          dropping = false;
          void store.addPaths(e.payload.paths);
        }
      })
      .then((u) => (unlisten = u));
    return () => unlisten?.();
  });

  function isTyping(t: EventTarget | null): boolean {
    return t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
  }

  /** Focused control that Enter activates itself (toolbar buttons, tabs, links). */
  function isControl(t: EventTarget | null): boolean {
    return t instanceof Element && t.closest("button, a[href], summary, [role=button], [role=tab]") != null;
  }

  function keydown(e: KeyboardEvent) {
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.key.toLowerCase() === "f") {
      e.preventDefault();
      searchBar?.focus();
      return;
    }
    if (ctrl && e.key.toLowerCase() === "e") {
      e.preventDefault();
      if (store.list || store.tagged) store.exportOpen = true;
      return;
    }
    if (isTyping(e.target)) return;
    if (e.key === "Enter" && isControl(e.target)) return;
    const list = store.nav;
    const att = store.attachments != null;
    const mode = e.shiftKey ? "range" : "single";
    const pageItems =
      (att ? attachmentsView?.pageItems() : store.tagged ? taggedView?.pageItems() : messageList?.pageItems()) ?? 20;
    /** The reading pane shows a message opened from an attachment preview. */
    const fromPreview = store.attRow != null && (att || store.tagged != null);
    /** Icons view moves by whole rows on ↑/↓ and by one tile on ←/→. */
    const step = att ? (attachmentsView?.columns() ?? 1) : 1;
    switch (e.key) {
      case "Escape":
        if (store.maximized) store.maximized = false;
        else if (store.reader.length > 1) store.popReader(store.reader.length - 2);
        else if (fromPreview && store.reader.length) store.reader = [];
        else if (store.search || store.query) store.clearSearch();
        else return;
        break;
      case "ArrowRight":
      case "ArrowLeft":
        if (!list || step === 1) return;
        void store.setFocus(Math.max(0, store.focus + (e.key === "ArrowRight" ? 1 : -1)), mode);
        break;
      case "ArrowDown":
      case "j":
        if (!list) return;
        void store.setFocus(store.focus < 0 ? 0 : store.focus + step, mode);
        break;
      case "ArrowUp":
      case "k":
        if (!list) return;
        void store.setFocus(store.focus >= step ? store.focus - step : Math.max(0, store.focus), mode);
        break;
      case "PageDown":
        if (!list) return;
        void store.setFocus(store.focus + pageItems, mode);
        break;
      case "PageUp":
        if (!list) return;
        void store.setFocus(Math.max(0, store.focus - pageItems), mode);
        break;
      case "Home":
        if (!list) return;
        void store.setFocus(0, mode);
        break;
      case "End":
        if (!list) return;
        void store.setFocus(list.count - 1, mode);
        break;
      case "Enter":
        if (fromPreview && !store.reader.length) {
          if (store.attRow && store.focus >= 0) void openAttachmentRow(store.attRow);
        } else if (store.reader.length) store.maximized = !store.maximized;
        else if (list && store.focus >= 0) void store.setFocus(store.focus);
        break;
      case "t":
      case "T":
        if (ctrl || e.altKey || !list) return;
        void tagButton?.show();
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  const save = (k: string) => (v: number) => localStorage.setItem(k, String(v));
</script>

<svelte:window onkeydown={keydown} />

<div class="app">
  <div class="topbar">
    <div class="brand">
      <img
        class="logo"
        src="/logo-64.png"
        srcset="/logo-128.png 2x"
        alt="STViewer"
        title="STViewer"
        width="36"
        height="36"
      />
    </div>
    <div class="tools">
      <SearchBar bind:this={searchBar} />
      <TagButton bind:this={tagButton} />
      <button
        class="btn"
        title="Export messages ({shortcut('E')})"
        disabled={!store.list && !store.tagged}
        onclick={() => (store.exportOpen = true)}><Icon name="export" size={14} /> Export</button
      >
      <button class="icon-btn" title="Settings" aria-label="Settings" onclick={() => (store.settingsOpen = true)}
        ><Icon name="settings" /></button
      >
    </div>
  </div>

  <div class="main">
    {#if store.sources.length === 0}
      <div class="pane" style:width="{leftW}px"><Sidebar /></div>
      <Splitter bind:value={leftW} min={180} max={520} onchange={save("stv.leftW")} />
      <div class="welcome">
        <div class="hero">
          <img class="logo" src="/logo-128.png" srcset="/logo-256.png 2x" alt="" width="72" height="72" />
          <h1>Open your mail archives</h1>
          <div class="actions">
            <button class="btn primary" onclick={() => store.addFilesDialog()}
              ><Icon name="filePlus" size={15} /> Add files</button
            >
            <button class="btn" onclick={() => store.addFolderDialog()}
              ><Icon name="folderPlus" size={15} /> Add folder</button
            >
          </div>
          <p class="hint muted">…or drop files and folders anywhere on this window.</p>
        </div>
      </div>
    {:else}
      {#if !store.maximized}
        <div class="pane" style:width="{leftW}px"><Sidebar /></div>
        <Splitter bind:value={leftW} min={180} max={520} onchange={save("stv.leftW")} />
        <div class="pane middle" style:width="{listW + (store.search ? facetW : 0)}px">
          {#if store.search}
            <div class="facet-col" style:width="{facetW}px"><Facets /></div>
            <Splitter bind:value={facetW} min={140} max={360} onchange={save("stv.facetW")} />
          {/if}
          <div class="list-col">
            {#if store.attachments}
              <AttachmentsView bind:this={attachmentsView} />
            {:else if store.tagged}
              <TaggedView bind:this={taggedView} />
            {:else}
              <MessageList bind:this={messageList} />
            {/if}
          </div>
        </div>
        <Splitter bind:value={listW} min={320} max={1400} onchange={save("stv.listW")} />
      {/if}
      <div class="pane reading">
        {#if showPreview}<AttachmentPreview />{:else}<ReadingPane />{/if}
      </div>
    {/if}
  </div>

  <StatusBar />

  {#if dropping}
    <div class="drop">
      <div><Icon name="plus" size={36} /><br />Drop to add to the library</div>
    </div>
  {/if}

  {#if store.exportOpen}<ExportDialog />{/if}
  {#if store.manageTagsOpen}<ManageTagsDialog />{/if}
  {#if store.settingsOpen}<SettingsDialog />{/if}

  <div class="toasts" aria-live="polite">
    {#each store.toasts as t (t.id)}
      <div class="toast {t.kind}" role={t.kind === "error" ? "alert" : "status"}>
        <span class="selectable">{t.text}</span>
        <button class="icon-btn" aria-label="Dismiss" onclick={() => store.dismiss(t.id)}
          ><Icon name="x" size={12} /></button
        >
      </div>
    {/each}
  </div>
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .topbar {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 46px;
    flex: none;
    padding: 0 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-2);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    flex: none;
  }
  /* Every control but the logo, right-aligned as one group. */
  .tools {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }
  .main {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .pane {
    flex: none;
    min-width: 0;
    height: 100%;
    overflow: hidden;
  }
  .middle {
    display: flex;
  }
  .facet-col {
    flex: none;
    height: 100%;
    border-right: 1px solid var(--border);
  }
  .list-col {
    flex: 1;
    min-width: 0;
    height: 100%;
  }
  .reading {
    flex: 1;
  }
  .welcome {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
  }
  .hero {
    max-width: 520px;
    text-align: center;
  }
  .hero h1 {
    font-size: 22px;
    margin: 12px 0 8px;
  }
  .hero p {
    line-height: 1.55;
  }
  .actions {
    display: flex;
    justify-content: center;
    gap: 10px;
    margin-top: 18px;
  }
  .actions .btn {
    padding: 8px 16px;
    font-size: 14px;
  }
  .hint {
    font-size: 12px;
    margin-top: 14px;
  }
  .drop {
    position: fixed;
    inset: 8px;
    z-index: 60;
    border: 3px dashed var(--accent);
    border-radius: 14px;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    display: flex;
    align-items: center;
    justify-content: center;
    text-align: center;
    font-size: 18px;
    color: var(--accent);
    pointer-events: none;
  }
  .toasts {
    position: fixed;
    right: 14px;
    bottom: 34px;
    z-index: 70;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: 440px;
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 8px 8px 8px 12px;
    border-radius: 8px;
    background: var(--bg);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
    overflow-wrap: anywhere;
  }
  .toast span {
    flex: 1;
  }
  .toast.error {
    border-color: var(--danger);
    color: var(--danger);
  }
</style>
