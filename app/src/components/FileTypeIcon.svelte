<script lang="ts">
  import Icon from "./Icon.svelte";
  import { CATEGORY_ICON } from "../lib/icons";
  import type { AttachmentCategory } from "../lib/types";

  let { category, size = 16 }: { category: AttachmentCategory; size?: number } = $props();
</script>

<span class="ft {category}"><Icon name={CATEGORY_ICON[category]} {size} /></span>

<style>
  .ft {
    display: inline-flex;
    flex: none;
    color: var(--fg-2);
  }
  .image {
    color: light-dark(#7c3aed, #a78bfa);
  }
  .pdf {
    color: light-dark(#dc2626, #f87171);
  }
  .document {
    color: light-dark(#2563eb, #60a5fa);
  }
  .spreadsheet {
    color: light-dark(#15803d, #4ade80);
  }
  .presentation {
    color: light-dark(#c2410c, #fb923c);
  }
  .archive {
    color: light-dark(#a16207, #facc15);
  }
  .email {
    color: var(--accent);
  }
  .calendar {
    color: light-dark(#0e7490, #22d3ee);
  }
  .audio {
    color: light-dark(#be185d, #f472b6);
  }
  .video {
    color: light-dark(#0f766e, #2dd4bf);
  }
  .other {
    color: var(--fg-3);
  }
</style>
