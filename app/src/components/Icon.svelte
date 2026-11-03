<script lang="ts">
  import { ICONS, type IconName } from "../lib/icons";

  let { name, size = 16, title }: { name: IconName; size?: number; title?: string } = $props();
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden={title ? undefined : "true"}
  role={title ? "img" : undefined}
>
  {#if title}<title>{title}</title>{/if}
  <path d={ICONS[name]} />
</svg>

<style>
  .icon {
    flex: none;
    display: inline-block;
    vertical-align: middle;
  }
</style>
