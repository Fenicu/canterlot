<script lang="ts">
  import type { Option } from './types';
  let { name, label, options, value = $bindable('') }: { name: string; label: string; options: Option[]; value?: string } = $props();
</script>
{#if options.length > 0}
  <div role="radiogroup" aria-label={label} class="segmented">
    {#each options as option (option.value)}<label><input type="radio" {name} value={option.value} bind:group={value} /><span>{option.label}</span></label>{/each}
  </div>
{/if}
<style>
  .segmented { display: flex; flex-wrap: wrap; gap: var(--s2); padding: var(--s2); border-radius: var(--radius-card); background: var(--raised); }
  label { flex: 1 1 auto; min-width: 0; text-align: center; cursor: pointer; font-size: var(--fs-meta); position: relative; }
  span { display: flex; min-height: var(--target); align-items: center; justify-content: center; border-radius: var(--radius-pill); padding-inline: var(--s3); white-space: nowrap; }
  input { position: absolute; opacity: 0; width: var(--hairline); }
  input:checked + span { background: var(--accent); color: var(--accent-ink); font-weight: var(--weight-medium); }
  input:focus-visible + span { outline: var(--focus-width) solid var(--accent); }
  /* Пять коротких вариантов в узком окне: без запаса вокруг плашек «1 год» падал на свою строку. */
  @media (max-width: 374px) {
    .segmented:has(> label:nth-child(5)) { gap: var(--s1); }
    .segmented:has(> label:nth-child(5)) span { padding-inline: var(--s1); }
  }
</style>
