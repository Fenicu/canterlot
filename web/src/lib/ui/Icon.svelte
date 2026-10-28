<script lang="ts">
  import { icons, type IconName } from './icons';
  let { name }: { name: IconName } = $props();
</script>
<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d={icons[name]} /></svg>
<style>
  svg { display: block; flex-shrink: 0; width: var(--icon); height: var(--icon); fill: none; stroke: currentColor; stroke-width: var(--focus-width); }
</style>
