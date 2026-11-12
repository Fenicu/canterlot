<script lang="ts">
  import { encode } from 'uqr';
  let { value, label }: { value: string; label: string } = $props();
  // Код рисуется здесь же, без сторонних сервисов: модули одним путём SVG с тихой зоной в четыре модуля.
  const code = $derived(encode(value, { ecc: 'M', border: 4 }));
  const path = $derived.by(() => {
    let d = '';
    code.data.forEach((row, y) => row.forEach((dark, x) => { if (dark) d += `M${x} ${y}h1v1h-1z`; }));
    return d;
  });
</script>
<svg viewBox={`0 0 ${code.size} ${code.size}`} role="img" aria-label={label} shape-rendering="crispEdges" data-qr={value}>
  <rect width={code.size} height={code.size} class="light" />
  <path d={path} class="dark" />
</svg>
<style>
  svg { display: block; width: var(--qr-size); max-width: 100%; height: auto; aspect-ratio: 1; border-radius: var(--radius-sm); }
  .light { fill: var(--qr-light); }
  .dark { fill: var(--qr-dark); }
</style>
