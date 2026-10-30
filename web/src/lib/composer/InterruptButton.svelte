<script lang="ts">
  import Icon from '#lib/ui/Icon.svelte';
  let { show, label, onclick }: { show: boolean; label: string; onclick: () => void } = $props();
</script>

{#if show}
  <button type="button" class="pill" {onclick}><Icon name="stop" /><span>{label}</span></button>
{/if}

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    gap: var(--s3);
    max-width: 100%;
    min-height: var(--target);
    padding: var(--s3) var(--s7);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--chat-bg);
    color: var(--secondary);
    font-size: var(--fs-meta);
    font-weight: var(--weight-medium);
  }
  .pill :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
