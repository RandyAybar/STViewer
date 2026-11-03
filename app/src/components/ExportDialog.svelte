<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import Dialog from "./Dialog.svelte";
  import { api, errMsg } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import type { ExportFormat, ExportReport } from "../lib/types";

  type Scope = "selection" | "folder" | "search" | "all";

  const FORMATS: { id: ExportFormat; label: string; desc: string }[] = [
    { id: "eml", label: "EML files", desc: "One .eml per message (opens in any mail client)" },
    { id: "mbox", label: "mbox", desc: "A single export.mbox (Thunderbird, Apple Mail, Gmail import)" },
    { id: "html", label: "HTML", desc: "One self-contained .html per message, images inlined" },
    { id: "text", label: "Plain text", desc: "One .txt per message" },
    { id: "csv", label: "CSV", desc: "export.csv with one row per message (date, from, to, subject, …)" },
    { id: "attachments", label: "Attachments only", desc: "Every attachment saved as its own file" },
    { id: "vcf", label: "vCard (contacts)", desc: "export.vcf with every contact; other items are skipped" },
    {
      id: "ics",
      label: "iCalendar (appointments & tasks)",
      desc: "export.ics with appointments, meeting requests and tasks (one per UID); other items are skipped",
    },
  ];

  /** Item formats (one `export.<format>` file holding every item of that type) → noun the report counts. */
  const SINGLE_FILE: Partial<Record<ExportFormat, string>> = { vcf: "contact", ics: "item" };

  const canSelection = $derived(store.selection.size > 0);
  const canFolder = $derived(store.current != null);
  const canSearch = $derived(store.search != null && store.search.count > 0);
  const allMail = $derived(store.search ? null : store.allMail);
  const canAll = $derived(allMail != null && allMail.count > 0);

  let scope = $state<Scope>(
    store.selection.size > 1
      ? "selection"
      : store.search
        ? "search"
        : store.allMail
          ? "all"
          : store.selection.size
            ? "selection"
            : "folder",
  );
  let format = $state<ExportFormat>("eml");
  let dest = $state("");
  let running = $state(false);
  let report = $state<ExportReport | null>(null);
  /** Format `report` was produced with (the radio may change afterwards). */
  let reportFormat = $state<ExportFormat>("eml");
  let error = $state<string | null>(null);

  const scopeCount = $derived(
    scope === "selection"
      ? store.selection.size
      : scope === "search"
        ? (store.search?.count ?? 0)
        : scope === "all"
          ? (allMail?.count ?? 0)
          : (store.folderList?.count ?? 0),
  );

  async function pickDest() {
    const d = await open({ directory: true, title: "Export to folder" });
    if (d) dest = d;
  }

  async function run() {
    if (!dest) return;
    running = true;
    error = null;
    report = null;
    reportFormat = format;
    try {
      if (scope === "selection") {
        report = await api.export(await store.selectedItems(), format, dest);
      } else if (scope === "search" && store.search) {
        const s = store.search;
        report = await api.exportSearch(s.query, s.options(0, 0, false), format, dest);
      } else if (scope === "all" && allMail) {
        report = await api.exportSearch("", allMail.options(0, 0), format, dest);
      } else if (scope === "folder" && store.current) {
        report = await api.exportFolder(store.current.source, store.current.folder, format, dest);
      }
    } catch (e) {
      error = errMsg(e);
    } finally {
      running = false;
    }
  }

  function close() {
    if (!running) store.exportOpen = false;
  }
</script>

<Dialog title="Export messages" onclose={close}>
  <fieldset>
    <legend>Messages</legend>
    <label class:off={!canSelection}
      ><input type="radio" bind:group={scope} value="selection" disabled={!canSelection} /> Selected messages {canSelection
        ? `(${store.selection.size.toLocaleString()})`
        : ""}</label
    >
    <label class:off={!canFolder}>
      <input type="radio" bind:group={scope} value="folder" disabled={!canFolder} /> Current folder
      {#if store.current}<span class="muted"
          >— {store.folderName(store.current.source, store.current.folder)}{store.folderList
            ? ` (${store.folderList.count.toLocaleString()})`
            : ""}</span
        >{/if}
    </label>
    <label class:off={!canSearch}
      ><input type="radio" bind:group={scope} value="search" disabled={!canSearch} /> All search results {canSearch
        ? `(${store.search?.count.toLocaleString()})`
        : ""}</label
    >
    {#if allMail}
      <label class:off={!canAll}>
        <input type="radio" bind:group={scope} value="all" disabled={!canAll} /> All mail
        <span class="muted">— {store.sourceName(allMail.source)} ({allMail.count.toLocaleString()})</span>
      </label>
    {/if}
  </fieldset>

  <fieldset>
    <legend>Format</legend>
    <div class="formats">
      {#each FORMATS as f (f.id)}
        <label class="fmt" class:on={format === f.id}>
          <input type="radio" bind:group={format} value={f.id} />
          <span><b>{f.label}</b><br /><span class="muted">{f.desc}</span></span>
        </label>
      {/each}
    </div>
  </fieldset>

  <fieldset>
    <legend>Destination folder</legend>
    <div class="dest">
      <input type="text" readonly value={dest} placeholder="Choose a folder…" />
      <button class="btn" onclick={pickDest}>Browse…</button>
    </div>
  </fieldset>

  {#if error}<div class="msg error selectable">{error}</div>{/if}
  {#if report}
    {@const one = SINGLE_FILE[reportFormat]}
    <div class="msg selectable">
      Exported <b>{report.written.toLocaleString()}</b>
      {one ?? "file"}{report.written === 1 ? "" : "s"}{one && report.written ? ` to export.${reportFormat}` : ""}.
      {#if report.skipped}
        <span class="muted">{report.skipped.toLocaleString()} other item{report.skipped === 1 ? "" : "s"} skipped.</span
        >
      {/if}
      {#if report.duplicates}
        <span class="muted"
          >{report.duplicates.toLocaleString()} repeat{report.duplicates === 1 ? "" : "s"} of an exported item (same UID)
          skipped.</span
        >
      {/if}
      {#if report.failed.length}
        <details>
          <summary>{report.failed.length} message{report.failed.length === 1 ? "" : "s"} failed</summary>
          <ul>
            {#each report.failed.slice(0, 200) as [, why], i (i)}<li>{why}</li>{/each}
          </ul>
        </details>
      {/if}
      <button class="link" onclick={() => api.openFolder(dest).catch((e) => store.toast(errMsg(e), "error"))}
        >Open folder</button
      >
    </div>
  {/if}

  <footer>
    <button class="btn" onclick={close} disabled={running}>{report ? "Close" : "Cancel"}</button>
    <button class="btn primary" onclick={run} disabled={running || !dest || scopeCount === 0}>
      {running ? "Exporting…" : `Export ${scopeCount.toLocaleString()} message${scopeCount === 1 ? "" : "s"}`}
    </button>
  </footer>
</Dialog>

<style>
  footer {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
  fieldset:first-child {
    margin-top: 0;
  }
  fieldset {
    border: none;
    margin: 12px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  legend {
    font-weight: 600;
    margin-bottom: 4px;
    font-size: 12px;
    color: var(--fg-2);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  label {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  label.off {
    opacity: 0.5;
  }
  .formats {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }
  .fmt {
    align-items: flex-start;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    font-size: 12px;
  }
  .fmt.on {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .dest {
    display: flex;
    gap: 6px;
  }
  .dest input {
    flex: 1;
  }
  .msg {
    margin-top: 12px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--bg-2);
  }
  .msg.error {
    color: var(--danger);
  }
  .link {
    margin-left: 8px;
  }
  ul {
    margin: 4px 0;
    padding-left: 18px;
    max-height: 140px;
    overflow: auto;
    font-size: 12px;
  }
</style>
