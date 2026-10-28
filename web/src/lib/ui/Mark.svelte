<script lang="ts">
  import type { ColorKey } from './types';
  let { color = 'accent' }: { color?: ColorKey } = $props();
</script>
<span class="mark" data-color={color} aria-hidden="true"></span>
<style>
  .mark { display: inline-block; width: var(--mark); height: var(--mark); border-radius: var(--radius-round); background: var(--accent); flex-shrink: 0; }
  [data-color=blue] { background: var(--blue); }
  [data-color=amber] { background: var(--warning); }
  [data-color=success] { background: var(--success); }
  [data-color=danger] { background: var(--danger); }
  [data-color=neutral] { background: var(--muted); }
</style>
