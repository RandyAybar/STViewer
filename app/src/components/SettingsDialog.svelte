<script lang="ts">
  import Dialog from "./Dialog.svelte";
  import DisplaySettings from "./DisplaySettings.svelte";
  import AgentAccessSection from "./AgentAccessSection.svelte";
  import { store } from "../lib/store.svelte";
</script>

<Dialog title="Settings" width="760px" onclose={() => (store.settingsOpen = false)}>
  <DisplaySettings />
  <AgentAccessSection />
</Dialog>
