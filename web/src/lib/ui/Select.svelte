<script lang="ts">
  import Field from './Field.svelte';
  import type { Option } from './types';
  let { label, hint, error, options, value = $bindable('') }: { label: string; hint?: string; error?: string; options: Option[]; value?: string } = $props();
</script>
<Field {label} {hint} {error}>{#snippet children({ id, describedby, invalid })}<select {id} bind:value aria-describedby={describedby} aria-invalid={invalid}>{#each options as option (option.value)}<option value={option.value}>{option.label}</option>{/each}</select>{/snippet}</Field>
<style>
  select { display: block; width: 100%; min-height: var(--target); padding: var(--s5) var(--s6); border: 0; border-radius: var(--radius-pill); background: var(--raised); font-size: var(--fs-input); }
</style>
