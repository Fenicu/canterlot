<script lang="ts">
  import { liveLineText } from '#lib/timeline/present.js';
  let { phase = 'working', tool = '', startedAt, now }: { phase?: string; tool?: string; startedAt: number; now: number } = $props();
  const text = $derived(liveLineText(phase, Math.max(0, now - startedAt), tool));
</script>

<p class="live"><span class="dot" aria-hidden="true"></span>{text}</p>

<style>
  .live { display: flex; align-items: center; gap: var(--s3); margin: 0; color: var(--secondary); font-size: var(--fs-sm); }
  .dot { flex: none; width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: var(--accent); }
</style>
