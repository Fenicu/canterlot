<script lang="ts">
  import { icons, type IconName } from './icons';
  let { name, filled = false }: { name: IconName; filled?: boolean } = $props();
</script>
<svg class:filled viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d={icons[name]} /></svg>
<style>
  svg { display: block; flex-shrink: 0; width: var(--icon); height: var(--icon); fill: none; stroke: currentColor; stroke-width: var(--focus-width); }
  /* Заливка — состояние переключателя: контур и заливка рисуются одним путём. */
  svg.filled { fill: currentColor; }
</style>
