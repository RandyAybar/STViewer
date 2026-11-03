<script lang="ts">
  import Icon from "./Icon.svelte";
  import AdvancedSearch from "./AdvancedSearch.svelte";
  import { store } from "../lib/store.svelte";
  import { shortcut } from "../lib/format";

  let input = $state<HTMLInputElement>();
  let advanced = $state(false);

  export function focus() {
    input?.focus();
    input?.select();
  }

  function key(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      store.runSearch();
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (store.query || store.search) store.clearSearch();
      else input?.blur();
    }
  }

  function closeAdvanced(e: MouseEvent) {
    if (advanced && !(e.target instanceof Element && e.target.closest(".adv, .adv-btn"))) advanced = false;
  }
</script>

<svelte:window onclick={closeAdvanced} />

<div class="searchbar">
  <div class="field">
    <Icon name="search" size={14} />
    <input
      bind:this={input}
      bind:value={store.query}
      type="text"
      spellcheck="false"
      autocomplete="off"
      placeholder="Search"
      title="Search all mail ({shortcut('F')}) — from:, subject:, has:attachment, is:meeting, after:…, -word excludes"
      onkeydown={key}
    />
    <button
      class="icon-btn adv-btn"
      class:on={advanced}
      title="Advanced search"
      aria-expanded={advanced}
      onclick={() => (advanced = !advanced)}><Icon name="settings" size={14} /></button
    >
    {#if store.query}
      <button class="icon-btn clear" title="Clear search (Esc)" onclick={() => store.clearSearch()}
        ><Icon name="x" size={13} /></button
      >
    {/if}
  </div>
  {#if advanced}<AdvancedSearch onclose={() => (advanced = false)} />{/if}
  <select bind:value={store.searchScope} title="Search scope" onchange={() => store.search && store.runSearch()}>
    <option value="all">All sources</option>
    <option value="source" disabled={store.contextSource == null}>This source</option>
    <option value="folder" disabled={!store.current}>This folder</option>
  </select>
</div>

<style>
  .searchbar {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 0 1 auto;
    min-width: 0;
  }
  .field {
    width: 560px;
    flex: 0 1 auto;
    min-width: 320px;
    max-width: 560px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 4px 0 10px;
    height: 30px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    color: var(--fg-3);
  }
  .field:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 25%, transparent);
  }
  .field input {
    flex: 1;
    border: none;
    background: transparent;
    padding: 0;
    color: var(--fg);
    box-shadow: none;
  }
  .clear,
  .adv-btn {
    width: 22px;
    height: 22px;
  }
  .adv-btn.on {
    color: var(--accent);
    background: var(--hover);
  }
  select {
    height: 30px;
  }
</style>
