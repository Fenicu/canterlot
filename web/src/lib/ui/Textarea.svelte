<script lang="ts">
  import type { FocusEventHandler } from 'svelte/elements';
  import Field from './Field.svelte';
  let { label, hint, error, value = $bindable(''), rows = 3, id, name, disabled = false, mono = false, onblur }: { label: string; hint?: string; error?: string; value?: string; rows?: number; id?: string; name?: string; disabled?: boolean; mono?: boolean; onblur?: FocusEventHandler<HTMLTextAreaElement> } = $props();
</script>
<Field {label} {hint} {error} {id}>{#snippet children({ id: fieldId, describedby, invalid })}<textarea id={fieldId} {name} {rows} {disabled} class:mono bind:value enterkeyhint="enter" aria-describedby={describedby} aria-invalid={invalid} {onblur}></textarea>{/snippet}</Field>
<style>
  textarea { field-sizing: content; min-height: calc(var(--fs-input) * var(--lh-body) * 2 + var(--s10)); max-height: calc(var(--fs-input) * var(--lh-body) * 14); resize: none; }
  .mono { font-family: var(--font-mono); font-size: var(--fs-meta); }
</style>
