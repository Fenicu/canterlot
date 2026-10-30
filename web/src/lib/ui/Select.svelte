<script lang="ts">
  import type { ChangeEventHandler } from 'svelte/elements';
  import Field from './Field.svelte';
  import Icon from './Icon.svelte';
  import type { Option } from './types';
  let { label, hint, error, options, value = $bindable(''), id, name, disabled = false, onchange }: { label: string; hint?: string; error?: string; options: Option[]; value?: string; id?: string; name?: string; disabled?: boolean; onchange?: ChangeEventHandler<HTMLSelectElement> } = $props();
</script>
<Field {label} {hint} {error} {id}>{#snippet children({ id: fieldId, describedby, invalid })}<span class="select"><select id={fieldId} {name} {disabled} bind:value aria-describedby={describedby} aria-invalid={invalid} {onchange}>{#each options as option (option.value)}<option value={option.value} disabled={option.disabled}>{option.label}</option>{/each}</select><Icon name="down" /></span>{/snippet}</Field>
<style>
  .select { position: relative; display: block; min-width: 0; }
  select { appearance: none; padding-right: var(--s13); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .select :global(svg) { position: absolute; top: 50%; right: var(--s6); translate: 0 -50%; color: var(--secondary); pointer-events: none; }
</style>
