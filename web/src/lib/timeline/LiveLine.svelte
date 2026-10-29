<script lang="ts">
  import { liveLineText } from '#lib/timeline/present.js';
  let { phase = 'working', startedAt, now }: { phase?: string; startedAt: number; now: number } = $props();
  const text = $derived(liveLineText(phase, Math.max(0, now - startedAt)));
</script>

<p class="live">{text}</p>

<style>
  .live { margin: 0; color: var(--accent-link); }
</style>
