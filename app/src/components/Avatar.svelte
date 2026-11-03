<script lang="ts">
  import { avatarHue, initials } from "../lib/format";
  import type { Address } from "../lib/types";

  /** Sender initials on a stable per-address color. */
  let { addr, size = 36 }: { addr: Address | null; size?: number } = $props();
</script>

<span
  class="avatar"
  aria-hidden="true"
  style:width="{size}px"
  style:height="{size}px"
  style:font-size="{Math.round(size * 0.36)}px"
  style:background="hsl({avatarHue(addr)} 55% 45%)">{initials(addr)}</span
>

<style>
  .avatar {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 50%;
    color: #fff;
    font-weight: 600;
  }
</style>
