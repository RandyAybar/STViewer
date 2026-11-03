<script lang="ts">
  import Icon from "./Icon.svelte";
  import { api, errMsg } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import type { TagTarget } from "../lib/types";

  let open = $state(false);
  let targets = $state.raw<TagTarget[]>([]);
  /** Tag name → how many of `targets` carry it. */
  let have = $state.raw<Map<string, number>>(new Map());
  let busy = $state(false);
  let newName = $state("");
  let input = $state<HTMLInputElement>();

  const enabled = $derived(store.selection.size > 0 && store.nav != null);
  const sorted = $derived([...store.tags].sort((a, b) => a.name.localeCompare(b.name)));

  /** Opens the menu for the current selection (`T`). */
  export async function show(): Promise<void> {
    if (!enabled) return;
    open = true;
    newName = "";
    targets = await store.selectedTargets();
    await refreshHave();
    input?.focus();
  }

  async function refreshHave() {
    try {
      const lists = targets.length ? await api.tagsOf(targets) : [];
      const m = new Map<string, number>();
      for (const l of lists) for (const t of new Set(l)) m.set(t, (m.get(t) ?? 0) + 1);
      have = m;
    } catch (e) {
      store.toast(`Could not read tags: ${errMsg(e)}`, "error");
    }
  }

  async function toggle(name: string) {
    if (busy) return;
    busy = true;
    const all = (have.get(name) ?? 0) === targets.length;
    await store.setTag(targets, name, !all);
    await refreshHave();
    busy = false;
  }

  async function create() {
    const name = newName.trim();
    if (!name || busy) return;
    busy = true;
    newName = "";
    await store.setTag(targets, name, true);
    await refreshHave();
    busy = false;
  }

  function windowClick(e: MouseEvent) {
    if (open && !(e.target instanceof Element && e.target.closest(".tagmenu, .tag-btn"))) open = false;
  }

  function windowKey(e: KeyboardEvent) {
    if (!open || e.key !== "Escape") return;
    open = false;
    e.stopImmediatePropagation();
    e.preventDefault();
  }
</script>

<svelte:window onclick={windowClick} onkeydowncapture={windowKey} />

<div class="tagwrap">
  <button
    class="btn tag-btn"
    class:on={open}
    title="Tag the selected emails or attachments (T)"
    disabled={!enabled}
    aria-haspopup="dialog"
    aria-expanded={open}
    onclick={() => (open ? (open = false) : void show())}><Icon name="label" size={14} /> Tag</button
  >
  {#if open}
    <div class="tagmenu" role="dialog" aria-label="Tag the selection">
      <div class="head muted">
        {targets.length ? `${targets.length.toLocaleString()} item${targets.length === 1 ? "" : "s"}` : "Loading…"}
      </div>
      <div class="items" role="group" aria-label="Tags">
        {#each sorted as t (t.name)}
          {@const n = have.get(t.name) ?? 0}
          {@const state = n === 0 ? "false" : n === targets.length ? "true" : "mixed"}
          <button
            role="checkbox"
            aria-checked={state}
            class="item"
            disabled={busy || !targets.length}
            onclick={() => toggle(t.name)}
          >
            <span class="box {state}">{state === "true" ? "✓" : state === "mixed" ? "–" : ""}</span>
            <span class="dot" style:background={t.color}></span>
            <span class="name">{t.name}</span>
            <span class="n">{t.count.toLocaleString()}</span>
          </button>
        {:else}
          <div class="none muted">No tags yet — type a name below.</div>
        {/each}
      </div>
      <input
        bind:this={input}
        type="text"
        spellcheck="false"
        placeholder="New tag…"
        aria-label="New tag name (Enter creates and applies)"
        bind:value={newName}
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            void create();
          }
        }}
      />
      <button
        class="manage"
        onclick={() => {
          open = false;
          store.manageTagsOpen = true;
        }}>Manage tags…</button
      >
    </div>
  {/if}
</div>

<style>
  .tagwrap {
    position: relative;
  }
  .tag-btn.on {
    background: var(--hover);
  }
  .tagmenu {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 40;
    width: 260px;
    padding: 6px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow);
    font-size: 12.5px;
  }
  .head {
    padding: 2px 6px 6px;
    font-size: 11px;
  }
  .items {
    max-height: 280px;
    overflow: auto;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 5px 6px;
    border: none;
    border-radius: 6px;
    background: transparent;
    text-align: left;
  }
  .item:hover:not(:disabled) {
    background: var(--hover);
  }
  .box {
    width: 14px;
    height: 14px;
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--fg-3);
    border-radius: 3px;
    font-size: 11px;
    line-height: 1;
  }
  .box.true,
  .box.mixed {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-fg);
  }
  .dot {
    width: 10px;
    height: 10px;
    flex: none;
    border-radius: 50%;
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .n {
    color: var(--fg-3);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .none {
    padding: 6px;
  }
  input {
    width: 100%;
    margin-top: 6px;
  }
  .manage {
    display: block;
    width: 100%;
    margin-top: 4px;
    padding: 5px 6px;
    border: none;
    border-radius: 6px;
    background: transparent;
    text-align: left;
    color: var(--fg-2);
    font-size: 12px;
  }
  .manage:hover {
    background: var(--hover);
  }
</style>
