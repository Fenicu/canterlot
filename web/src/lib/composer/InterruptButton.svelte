<script lang="ts">
  let { show, label, onclick }: { show: boolean; label: string; onclick: () => void } = $props();
</script>

{#if show}
  <button type="button" class="pill" {onclick}>{label}</button>
{/if}

<style>
  .pill {
    min-height: var(--target);
    padding: var(--s3) var(--s6);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--danger-soft);
    color: var(--danger);
    font-size: var(--fs-meta);
    font-weight: var(--weight-medium);
  }
</style>
