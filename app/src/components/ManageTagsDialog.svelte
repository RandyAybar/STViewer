<script lang="ts">
  import Dialog from "./Dialog.svelte";
  import { api, errMsg } from "../lib/api";
  import { store } from "../lib/store.svelte";

  let newName = $state("");
  let newColor = $state("#2f6364");
  /** Tag awaiting a second click on Delete. */
  let confirmDelete = $state<string | null>(null);

  const sorted = $derived([...store.tags].sort((a, b) => a.name.localeCompare(b.name)));

  async function rename(old: string, input: HTMLInputElement) {
    const name = input.value.trim();
    if (name && name !== old) await store.renameTag(old, name);
    // Unchanged, empty or rejected: show the current name again.
    if (store.tags.some((t) => t.name === old)) input.value = old;
  }

  async function remove(name: string) {
    if (confirmDelete !== name) {
      confirmDelete = name;
      return;
    }
    confirmDelete = null;
    await store.deleteTag(name);
  }

  async function create() {
    const name = newName.trim();
    if (!name) return;
    try {
      await api.createTag(name, newColor);
      newName = "";
      await store.refreshTags();
    } catch (e) {
      store.toast(`Could not create ${name}: ${errMsg(e)}`, "error");
    }
  }
</script>

<Dialog title="Manage tags" width="460px" onclose={() => (store.manageTagsOpen = false)}>
  <div class="list">
    {#each sorted as t (t.name)}
      <div class="tag">
        <span class="dot" style:background={t.color}></span>
        <input
          type="text"
          spellcheck="false"
          value={t.name}
          aria-label="Tag name"
          onchange={(e) => rename(t.name, e.currentTarget)}
          onkeydown={(e) => e.key === "Enter" && e.currentTarget.blur()}
        />
        <span class="n muted">{t.count.toLocaleString()} item{t.count === 1 ? "" : "s"}</span>
        <button class="btn danger" class:confirm={confirmDelete === t.name} onclick={() => remove(t.name)}>
          {confirmDelete === t.name ? "Confirm delete" : "Delete"}
        </button>
      </div>
    {:else}
      <p class="muted">No tags yet.</p>
    {/each}
  </div>

  <form
    class="new"
    onsubmit={(e) => {
      e.preventDefault();
      void create();
    }}
  >
    <input type="color" bind:value={newColor} aria-label="New tag color" />
    <input type="text" spellcheck="false" placeholder="New tag name" data-autofocus bind:value={newName} />
    <button class="btn" type="submit" disabled={!newName.trim()}>Add</button>
  </form>
  <p class="hint muted">Rename a tag by editing its name. Deleting removes it from every email and attachment.</p>
</Dialog>

<style>
  .tag {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 0;
  }
  .dot {
    width: 12px;
    height: 12px;
    flex: none;
    border-radius: 50%;
  }
  .tag input {
    flex: 1;
    min-width: 0;
  }
  .n {
    font-size: 11.5px;
    white-space: nowrap;
  }
  .btn.danger {
    color: var(--danger);
  }
  .btn.confirm {
    background: var(--danger);
    border-color: var(--danger);
    color: #fff;
  }
  .new {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }
  .new input[type="text"] {
    flex: 1;
  }
  .new input[type="color"] {
    width: 32px;
    height: 28px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
  }
  .hint {
    margin: 10px 0 0;
    font-size: 11.5px;
  }
</style>
