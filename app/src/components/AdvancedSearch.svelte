<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../lib/store.svelte";
  import { buildQuery, emptyAdvanced, parseAdvanced, type AdvancedQuery } from "../lib/query";

  let { onclose }: { onclose: () => void } = $props();

  let f = $state<AdvancedQuery>(parseAdvanced(store.query));
  let first = $state<HTMLInputElement>();
  let form = $state<HTMLFormElement>();
  const src = $derived(store.contextSource);
  const folderNames = $derived(
    [...new Set((src != null ? store.folders[src] : undefined)?.map((x) => x.name) ?? [])].sort(),
  );
  const preview = $derived(buildQuery(f));

  onMount(() => first?.focus());

  // Esc closes, Enter anywhere (checkboxes and selects too) searches.
  $effect(() => {
    const el = form;
    if (!el) return;
    el.addEventListener("keydown", keydown);
    return () => el.removeEventListener("keydown", keydown);
  });

  function submit() {
    store.runSearch(buildQuery(f));
    onclose();
  }

  function keydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    } else if (e.key === "Enter" && !(e.target instanceof HTMLButtonElement)) {
      e.preventDefault();
      submit();
    }
  }
</script>

<form
  class="adv"
  aria-label="Advanced search"
  bind:this={form}
  onsubmit={(e) => {
    e.preventDefault();
    submit();
  }}
>
  <div class="grid">
    <label>
      <span>From</span>
      <input bind:this={first} type="text" spellcheck="false" placeholder="Name or address" bind:value={f.from} />
    </label>
    <label>
      <span>To <em>(To, Cc, Bcc)</em></span>
      <input type="text" spellcheck="false" placeholder="Name or address" bind:value={f.to} />
    </label>
    <label>
      <span>Subject</span>
      <input type="text" bind:value={f.subject} />
    </label>
    <label>
      <span>Has the words</span>
      <input type="text" bind:value={f.words} />
    </label>
    <label>
      <span>Exact phrase</span>
      <input type="text" bind:value={f.phrase} />
    </label>
    <label>
      <span>Doesn't have</span>
      <input type="text" placeholder="Words to exclude" bind:value={f.without} />
    </label>
    <label>
      <span>Attachment name or text</span>
      <input type="text" bind:value={f.attachment} />
    </label>
    <label>
      <span>Label / category</span>
      <input type="text" bind:value={f.label} />
    </label>
    <label>
      <span>In folder</span>
      <input type="text" list="adv-folders" placeholder="Any folder" bind:value={f.folder} />
      <datalist id="adv-folders">
        {#each folderNames as n (n)}<option value={n}></option>{/each}
      </datalist>
    </label>
    <fieldset class="scope">
      <legend>Search in</legend>
      <label class="inline"><input type="radio" bind:group={store.searchScope} value="all" /> All mailboxes</label>
      <label class="inline" class:off={src == null}>
        <input type="radio" bind:group={store.searchScope} value="source" disabled={src == null} />
        This mailbox{src != null ? ` (${store.sourceName(src)})` : ""}
      </label>
    </fieldset>
    <label>
      <span>Date after</span>
      <input type="date" bind:value={f.after} />
    </label>
    <label>
      <span>Date before</span>
      <input type="date" bind:value={f.before} />
    </label>
    <div class="size">
      <span>Larger than</span>
      <div class="pair">
        <input type="text" inputmode="decimal" aria-label="Larger than" bind:value={f.larger} />
        <select aria-label="Larger than unit" bind:value={f.largerUnit}>
          <option value="KB">KB</option>
          <option value="MB">MB</option>
        </select>
      </div>
    </div>
    <div class="size">
      <span>Smaller than</span>
      <div class="pair">
        <input type="text" inputmode="decimal" aria-label="Smaller than" bind:value={f.smaller} />
        <select aria-label="Smaller than unit" bind:value={f.smallerUnit}>
          <option value="KB">KB</option>
          <option value="MB">MB</option>
        </select>
      </div>
    </div>
  </div>

  <div class="checks">
    <label class="inline"><input type="checkbox" bind:checked={f.hasAttachment} /> Has attachment</label>
    <label class="inline"><input type="checkbox" bind:checked={f.unread} /> Unread</label>
    <label class="inline"><input type="checkbox" bind:checked={f.flagged} /> Flagged</label>
    <label class="inline type">
      Type
      <select bind:value={f.itemType}>
        <option value="">Any item</option>
        <option value="appointment">Appointments</option>
        <option value="meeting">Meeting messages</option>
        <option value="contact">Contacts</option>
        <option value="task">Tasks</option>
      </select>
    </label>
  </div>

  <div class="query selectable" title="Query that will run">{preview || "(everything)"}</div>

  <div class="buttons">
    <button type="button" class="btn" onclick={() => (f = emptyAdvanced())}>Clear</button>
    <span class="grow"></span>
    <button type="button" class="btn" onclick={onclose}>Cancel</button>
    <button type="submit" class="btn primary">Search</button>
  </div>
</form>

<style>
  .adv {
    position: absolute;
    top: 36px;
    left: 0;
    z-index: 30;
    width: 520px;
    padding: 12px 14px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow);
    font-size: 12.5px;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px 12px;
  }
  label,
  .size {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  label > span,
  .size > span,
  legend {
    font-size: 11px;
    color: var(--fg-2);
  }
  em {
    font-style: normal;
    color: var(--fg-3);
  }
  input[type="text"],
  input[type="date"] {
    width: 100%;
    min-width: 0;
    height: 28px;
  }
  input[type="date"] {
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    padding: 2px 6px;
    outline: none;
  }
  .pair {
    display: flex;
    gap: 4px;
  }
  .pair select {
    height: 28px;
  }
  .scope {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    border: none;
    min-width: 0;
  }
  legend {
    padding: 0;
    margin-bottom: 3px;
  }
  .inline {
    flex-direction: row;
    align-items: center;
    gap: 6px;
  }
  .off {
    color: var(--fg-3);
  }
  .checks {
    display: flex;
    align-items: center;
    gap: 18px;
    margin-top: 10px;
  }
  .type {
    margin-left: auto;
  }
  .type select {
    height: 26px;
  }
  .query {
    margin-top: 10px;
    padding: 5px 8px;
    border-radius: 6px;
    background: var(--bg-2);
    font: 11.5px/1.4 var(--mono);
    color: var(--fg-2);
    overflow-wrap: anywhere;
  }
  .buttons {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
  }
  .grow {
    flex: 1;
  }
</style>
