<script lang="ts">
  import type { ColorKey } from './types';
  let { label, options, value, onchange }: { label: string; options: { key: ColorKey; name: string }[]; value: string; onchange: (key: ColorKey) => void } = $props();
  const id = $props.id();
</script>
<div class="swatches" role="radiogroup" aria-label={label}>
  {#each options as option (option.key)}
    <label class:on={value === option.key} title={option.name}>
      <input type="radio" name={id} value={option.key} checked={value === option.key} aria-label={option.name} onchange={() => onchange(option.key)} />
      <span class="dot" data-color={option.key}></span>
    </label>
  {/each}
</div>
<style>
  .swatches { display: flex; flex-wrap: wrap; gap: var(--s2); }
  label { position: relative; display: inline-flex; align-items: center; justify-content: center; width: var(--target); height: var(--target); border-radius: var(--radius-round); cursor: pointer; }
  label.on { outline: var(--focus-width) solid var(--accent); outline-offset: calc(var(--focus-width) * -1); }
  input { position: absolute; inset: 0; width: 100%; height: 100%; margin: 0; opacity: 0; cursor: pointer; }
  input:focus-visible + .dot { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  .dot { width: var(--icon); height: var(--icon); border-radius: var(--radius-round); background: var(--muted); }
  [data-color='accent'] { background: var(--accent); }
  [data-color='blue'] { background: var(--blue); }
  [data-color='amber'] { background: var(--warning); }
  [data-color='success'] { background: var(--success); }
  [data-color='danger'] { background: var(--danger); }
</style>
