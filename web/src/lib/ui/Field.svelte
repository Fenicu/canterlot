<script lang="ts">
  import type { Snippet } from 'svelte';
  let { label, hint, error, id: givenId, trailing, children }: { label: string; hint?: string; error?: string; id?: string; trailing?: Snippet; children: Snippet<[{ id: string; describedby?: string; invalid: boolean }]> } = $props();
  const autoId = $props.id();
  const id = $derived(givenId ?? autoId);
</script>
<div class="field">
  {#if trailing}
    <div class="label-row"><label for={id}>{label}</label>{@render trailing()}</div>
  {:else}
    <label for={id}>{label}</label>
  {/if}
  {@render children({ id, describedby: error || hint ? `${id}-note` : undefined, invalid: !!error })}
  {#if error || hint}<p id={`${id}-note`} class:error={!!error}>{error ?? hint}</p>{/if}
</div>
<style>
  .field { min-width: 0; }
  label { display: block; color: var(--secondary); font-size: var(--fs-meta); margin: var(--s0) var(--s2) var(--s3); }
  /* Действие в строке подписи: поле под ним не отдаёт кнопке свой правый верхний угол. */
  .label-row { display: flex; align-items: center; gap: var(--s2); margin: var(--s0) var(--s2) var(--s3); }
  .label-row label { flex: 1; min-width: 0; margin: var(--s0); }
  p { margin: var(--s3) var(--s2) var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .error { color: var(--danger); }
  :global(.field input), :global(.field select), :global(.field textarea) {
    display: block; width: 100%; min-height: var(--target); padding: var(--s5) var(--s8); border: 0; border-radius: var(--radius-pill); background: var(--field-bg, var(--raised)); font-size: var(--fs-input); color: inherit;
  }
  :global(.field textarea) {
    border-radius: var(--radius);
  }
  :global(.field :is(input, select, textarea):disabled) { color: var(--muted); cursor: default; }
</style>
