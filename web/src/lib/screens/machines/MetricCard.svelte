<script lang="ts">
  import Gauge from '$lib/ui/Gauge.svelte';
  let { title, value, caption = '', pct, warn = false }: { title: string; value: string; caption?: string; pct?: number | null; warn?: boolean } = $props();
</script>
<section class:warn>
  <div><div class="metric-title"><h3>{title}</h3><strong>{value}</strong></div>{#if caption}<p>{caption}</p>{/if}</div>
  {#if pct !== null && pct !== undefined}<Gauge value={pct} {warn} label={title} />{/if}
</section>
<style>
  section { display: flex; align-items: center; justify-content: space-between; gap: var(--s8); background: var(--surface); border-radius: var(--radius-card); padding: var(--s5) var(--s6); margin-block: var(--s2); }
  section > div { min-width: 0; }
  .metric-title { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s4); }
  h3 { font-size: var(--fs-body); font-weight: var(--weight-medium); }
  strong { font-size: var(--fs-title); font-variant-numeric: tabular-nums; }
  p { font-size: var(--fs-meta); color: var(--muted); }
  .warn strong { color: var(--warning); }
</style>
