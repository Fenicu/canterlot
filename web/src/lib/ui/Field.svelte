<script lang="ts">
  import type { Snippet } from 'svelte';
  let { label, hint, error, id: givenId, children }: { label: string; hint?: string; error?: string; id?: string; children: Snippet<[{ id: string; describedby?: string; invalid: boolean }]> } = $props();
  const autoId = $props.id();
  const id = $derived(givenId ?? autoId);
</script>
<div class="field"><label for={id}>{label}</label>{@render children({ id, describedby: error || hint ? `${id}-note` : undefined, invalid: !!error })}
  {#if error || hint}<p id={`${id}-note`} class:error={!!error}>{error ?? hint}</p>{/if}
</div>
<style>
  .field { min-width: 0; }
  label { display: block; color: var(--secondary); font-size: var(--fs-meta); margin: var(--s0) var(--s2) var(--s3); }
  p { margin: var(--s3) var(--s2) var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .error { color: var(--danger); }
  :global(.field input), :global(.field select), :global(.field textarea) {
    display: block; width: 100%; min-height: var(--target); padding: var(--s5) var(--s8); border: 0; border-radius: var(--radius-pill); background: var(--raised); font-size: var(--fs-input); color: inherit;
  }
  :global(.field textarea) {
    border-radius: var(--radius);
  }
  :global(.field :is(input, select, textarea):disabled) { color: var(--muted); cursor: default; }
</style>
