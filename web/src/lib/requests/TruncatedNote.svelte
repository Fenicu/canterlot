<script lang="ts">
  let { payload }: { payload: unknown } = $props();
  const truncated = $derived(typeof payload === 'object' && payload !== null && (payload as { truncated?: unknown }).truncated === true);
</script>

{#if truncated}<p class="note" data-truncated>Показано начало: решение относится к полному тексту</p>{/if}

<style>
  .note { margin: var(--s0); color: var(--muted); font-size: var(--fs-meta); }
</style>
