<script lang="ts">
  import { icons, type IconName } from './icons';
  let { name, filled = false }: { name: IconName; filled?: boolean } = $props();
</script>
<svg class:filled viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d={icons[name]} /></svg>
<style>
  svg { display: block; flex-shrink: 0; width: var(--icon); height: var(--icon); fill: var(--transparent); stroke: currentColor; stroke-width: var(--focus-width); transition: fill var(--duration-fast) var(--ease); }
  /* Заливка — состояние переключателя: контур и заливка рисуются одним путём. Прозрачная заливка вместо `none`,
     чтобы она проявлялась плавно, а не вспыхивала: `none` с цветом не смешивается. */
  svg.filled { fill: currentColor; }
</style>
