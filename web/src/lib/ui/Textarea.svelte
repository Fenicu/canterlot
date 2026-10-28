<script lang="ts">
  import Field from './Field.svelte';
  let { label, hint, error, value = $bindable(''), rows = 3 }: { label: string; hint?: string; error?: string; value?: string; rows?: number } = $props();
</script>
<Field {label} {hint} {error}>{#snippet children({ id, describedby, invalid })}<textarea {id} {rows} bind:value enterkeyhint="enter" aria-describedby={describedby} aria-invalid={invalid}></textarea>{/snippet}</Field>
<style>
  textarea { display: block; width: 100%; min-height: var(--target); padding: var(--s6); border: 0; border-radius: var(--radius); background: var(--raised); font-size: var(--fs-input); resize: vertical; }
</style>
