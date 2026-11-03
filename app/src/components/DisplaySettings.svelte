<script lang="ts">
  import { settings, SYSTEM_TIME_ZONE, TIME_ZONES, type Theme } from "../lib/settings.svelte";

  const THEMES: { id: Theme; label: string }[] = [
    { id: "light", label: "Light" },
    { id: "dark", label: "Dark" },
    { id: "system", label: "System default" },
  ];

  /** UTC right after "System default", then every other zone. */
  const ZONES = ["UTC", ...TIME_ZONES.filter((z) => z !== "UTC")];
</script>

<section>
  <h3>Time zone</h3>
  <p class="muted">Every date and time in STViewer is shown in this zone.</p>
  <select
    class="tz"
    aria-label="Time zone"
    value={settings.timeZone}
    onchange={(e) => settings.setTimeZone(e.currentTarget.value)}
  >
    <option value="">System default ({SYSTEM_TIME_ZONE})</option>
    {#each ZONES as z (z)}<option value={z}>{z.replaceAll("_", " ")}</option>{/each}
  </select>
</section>

<section>
  <h3>Appearance</h3>
  <div class="seg" role="radiogroup" aria-label="Theme">
    {#each THEMES as t (t.id)}
      <button
        role="radio"
        aria-checked={settings.theme === t.id}
        class:on={settings.theme === t.id}
        onclick={() => settings.setTheme(t.id)}>{t.label}</button
      >
    {/each}
  </div>
</section>

<style>
  section {
    padding-bottom: 16px;
    margin-bottom: 16px;
    border-bottom: 1px solid var(--border);
  }
  h3 {
    margin: 0 0 4px;
    font-size: 14px;
  }
  p {
    margin: 0 0 8px;
  }
  .tz {
    width: 100%;
  }
  .seg {
    display: inline-flex;
    gap: 2px;
    margin-top: 4px;
    padding: 2px;
    border-radius: 7px;
    background: var(--bg-3);
  }
  .seg button {
    border: none;
    background: transparent;
    padding: 4px 12px;
    border-radius: 5px;
    color: var(--fg-2);
  }
  .seg button.on {
    background: var(--bg);
    color: var(--fg);
    box-shadow: 0 0 0 1px var(--border);
  }
</style>
