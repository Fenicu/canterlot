<script lang="ts">
  import type { Snippet } from 'svelte';
  let { label, hint, error, children }: { label: string; hint?: string; error?: string; children: Snippet<[{ id: string; describedby?: string; invalid: boolean }]> } = $props();
  const id = $props.id();
</script>
<div class="field"><label for={id}>{label}</label>{@render children({ id, describedby: error || hint ? `${id}-note` : undefined, invalid: !!error })}
  {#if error || hint}<p id={`${id}-note`} class:error={!!error}>{error ?? hint}</p>{/if}
</div>
<style>
  .field { margin-bottom: var(--s8); min-width: 0; }
  label { display: block; color: var(--secondary); font-size: var(--fs-meta); margin: var(--s0) var(--s6) var(--s4); }
  p { margin: var(--s4) var(--s6) var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .error { color: var(--danger); }
  :global(.field input), :global(.field select), :global(.field textarea) {
    display: block; width: 100%; min-height: var(--target); padding: var(--s5) var(--s6); border: 0; border-radius: var(--radius-pill); background: var(--raised); font-size: var(--fs-input); color: inherit;
  }
  :global(.field textarea) {
    border-radius: var(--radius);
  }
</style>
