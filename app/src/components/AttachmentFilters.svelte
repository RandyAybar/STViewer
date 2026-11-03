<script lang="ts">
  import Icon from "./Icon.svelte";
  import FileTypeIcon from "./FileTypeIcon.svelte";
  import { store } from "../lib/store.svelte";
  import { CATEGORIES, CATEGORY_LABEL } from "../lib/icons";
  import type { AttachmentCategory } from "../lib/types";

  let { onclose }: { onclose: () => void } = $props();

  type Unit = "KB" | "MB";
  const UNIT: Record<Unit, number> = { KB: 1024, MB: 1024 * 1024 };

  const PRESETS: { label: string; title: string; apply: () => void }[] = [
    {
      label: "Hide < 10 KB",
      title: "Signature logos, tracking pixels",
      apply: () => store.setAttPrefs({ minSize: 10 * 1024 }),
    },
    {
      label: "Hide < 100 KB",
      title: "Small images and snippets",
      apply: () => store.setAttPrefs({ minSize: 100 * 1024 }),
    },
    { label: "Hide > 10 MB", title: "Very large files", apply: () => store.setAttPrefs({ maxSize: 10 * 1024 * 1024 }) },
  ];

  function unitFor(bytes: number | null): Unit {
    return bytes != null && bytes >= UNIT.MB && bytes % (UNIT.MB / 10) === 0 ? "MB" : "KB";
  }

  let minUnit = $state<Unit>(unitFor(store.attPrefs.minSize));
  let maxUnit = $state<Unit>(unitFor(store.attPrefs.maxSize));
  let extInput = $state("");

  const counts = $derived(new Map(store.attachments?.categoryCounts ?? []));

  function shown(bytes: number | null, unit: Unit): string {
    return bytes == null ? "" : String(Math.round((bytes / UNIT[unit]) * 100) / 100);
  }

  function parseSize(v: string, unit: Unit): number | null {
    const n = Number(v.trim().replace(",", "."));
    return v.trim() && Number.isFinite(n) && n > 0 ? Math.round(n * UNIT[unit]) : null;
  }

  function setMin(v: string) {
    store.setAttPrefs({ minSize: parseSize(v, minUnit) });
  }

  function setMax(v: string) {
    store.setAttPrefs({ maxSize: parseSize(v, maxUnit) });
  }

  /** Unit change keeps the number typed, so "5 KB" → "5 MB". */
  function changeUnit(which: "min" | "max", unit: Unit) {
    if (which === "min") {
      const v = shown(store.attPrefs.minSize, minUnit);
      minUnit = unit;
      if (v) setMin(v);
    } else {
      const v = shown(store.attPrefs.maxSize, maxUnit);
      maxUnit = unit;
      if (v) setMax(v);
    }
  }

  function toggleCategory(c: AttachmentCategory, visible: boolean) {
    const cur = store.attPrefs.excludeCategories;
    store.setAttPrefs({ excludeCategories: visible ? cur.filter((x) => x !== c) : [...cur, c] });
  }

  /** Adds the comma/space separated extensions typed so far. */
  function commitExt() {
    const add = extInput
      .split(/[\s,;]+/)
      .map((s) =>
        s
          .trim()
          .toLowerCase()
          .replace(/^\*?\./, ""),
      )
      .filter((s) => s && !store.attPrefs.excludeExtensions.includes(s));
    extInput = "";
    if (add.length) store.setAttPrefs({ excludeExtensions: [...store.attPrefs.excludeExtensions, ...new Set(add)] });
  }

  function extKey(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === "," || e.key === " ") {
      e.preventDefault();
      commitExt();
    } else if (e.key === "Backspace" && !extInput && store.attPrefs.excludeExtensions.length) {
      store.setAttPrefs({ excludeExtensions: store.attPrefs.excludeExtensions.slice(0, -1) });
    } else if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    }
  }

  function removeExt(x: string) {
    store.setAttPrefs({ excludeExtensions: store.attPrefs.excludeExtensions.filter((e) => e !== x) });
  }
</script>

<div class="panel" role="dialog" aria-label="Attachment filters">
  <section>
    <h4>Show types</h4>
    <div class="cats">
      {#each CATEGORIES as c (c)}
        {@const visible = !store.attPrefs.excludeCategories.includes(c)}
        {@const n = counts.get(c) ?? 0}
        <label class="cat" class:dim={n === 0 && visible}>
          <input type="checkbox" checked={visible} onchange={(e) => toggleCategory(c, e.currentTarget.checked)} />
          <FileTypeIcon category={c} size={14} />
          <span class="lbl">{CATEGORY_LABEL[c]}</span>
          {#if visible && n > 0}<span class="n">{n.toLocaleString()}</span>{/if}
        </label>
      {/each}
    </div>
  </section>

  <section>
    <h4>Hide extensions</h4>
    <div class="exts">
      {#each store.attPrefs.excludeExtensions as x (x)}
        <span class="ext-chip"
          >.{x}<button class="icon-btn x" aria-label="Show .{x} files" onclick={() => removeExt(x)}
            ><Icon name="x" size={10} /></button
          ></span
        >
      {/each}
      <input
        type="text"
        spellcheck="false"
        placeholder={store.attPrefs.excludeExtensions.length ? "more…" : "e.g. png, ics, vcf"}
        bind:value={extInput}
        onkeydown={extKey}
        onblur={commitExt}
      />
    </div>
  </section>

  <section>
    <h4>Size</h4>
    <div class="sizes">
      <label for="att-min">At least</label>
      <input
        id="att-min"
        type="text"
        inputmode="decimal"
        placeholder="any"
        value={shown(store.attPrefs.minSize, minUnit)}
        onchange={(e) => setMin(e.currentTarget.value)}
      />
      <select
        value={minUnit}
        aria-label="Minimum size unit"
        onchange={(e) => changeUnit("min", e.currentTarget.value as Unit)}
      >
        <option value="KB">KB</option>
        <option value="MB">MB</option>
      </select>
      <label for="att-max">At most</label>
      <input
        id="att-max"
        type="text"
        inputmode="decimal"
        placeholder="any"
        value={shown(store.attPrefs.maxSize, maxUnit)}
        onchange={(e) => setMax(e.currentTarget.value)}
      />
      <select
        value={maxUnit}
        aria-label="Maximum size unit"
        onchange={(e) => changeUnit("max", e.currentTarget.value as Unit)}
      >
        <option value="KB">KB</option>
        <option value="MB">MB</option>
      </select>
    </div>
    <div class="presets">
      {#each PRESETS as p (p.label)}
        <button
          class="btn small"
          title={p.title}
          onclick={() => {
            p.apply();
            minUnit = unitFor(store.attPrefs.minSize);
            maxUnit = unitFor(store.attPrefs.maxSize);
          }}>{p.label}</button
        >
      {/each}
    </div>
  </section>

  <section>
    <label class="toggle">
      <input
        type="checkbox"
        checked={store.attPrefs.includeInline}
        onchange={(e) => store.setAttPrefs({ includeInline: e.currentTarget.checked })}
      />
      Include inline images <span class="muted">(signature logos, embedded <code>cid:</code> pictures)</span>
    </label>
  </section>

  <footer>
    <span class="muted">Saved for all mailboxes</span>
    <button
      class="btn"
      disabled={store.attFilterCount === 0 && store.attCategories.length === 0}
      onclick={() => store.clearAttFilters()}
    >
      Clear filters
    </button>
  </footer>
</div>

<style>
  .panel {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 30;
    width: 340px;
    max-height: min(560px, 80vh);
    overflow: auto;
    padding: 10px 12px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow);
    font-size: 12.5px;
  }
  section + section {
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }
  h4 {
    margin: 0 0 6px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-2);
  }
  .cats {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 2px 10px;
  }
  .cat {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 0;
    min-width: 0;
  }
  .cat.dim .lbl {
    color: var(--fg-3);
  }
  .lbl {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .n {
    color: var(--fg-3);
    font-variant-numeric: tabular-nums;
    font-size: 11px;
  }
  .exts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    padding: 3px 4px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .exts:focus-within {
    border-color: var(--accent);
  }
  .exts input {
    flex: 1;
    min-width: 80px;
    border: none;
    padding: 2px 4px;
    background: transparent;
    box-shadow: none;
  }
  .ext-chip {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    padding: 0 2px 0 7px;
    border-radius: 999px;
    background: var(--bg-3);
    font: 11px/20px var(--mono);
  }
  .icon-btn.x {
    width: 16px;
    height: 16px;
    border-radius: 50%;
  }
  .sizes {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 4px 6px;
  }
  .sizes input {
    width: 100%;
    min-width: 0;
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 8px;
  }
  .btn.small {
    padding: 2px 8px;
    font-size: 12px;
  }
  .toggle {
    display: flex;
    align-items: baseline;
    gap: 6px;
    flex-wrap: wrap;
  }
  code {
    font-family: var(--mono);
    font-size: 11px;
  }
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }
</style>
