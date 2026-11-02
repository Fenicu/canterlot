<script lang="ts">
  let { text }: { text: string } = $props();
  const parts = $derived(text.split(/(``[^`]+``)/g));
</script>

{#each parts as part, index (index)}
  {#if index % 2 === 1}
    <code>{part.slice(2, -2)}</code>
  {:else}{part}{/if}
{/each}

<style>
  code {
    padding: var(--s1) var(--s2);
    border-radius: var(--radius-sm);
    background: var(--code);
    font-family: var(--font-mono);
    font-size: var(--fs-meta);
    overflow-wrap: anywhere;
  }
</style>
