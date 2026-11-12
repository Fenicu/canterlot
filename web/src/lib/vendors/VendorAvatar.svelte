<script lang="ts">
  import { LOGOS } from './logos';
  import { vendorLetter, type Vendor } from './vendor';

  let {
    vendor,
    model = '',
    title = '',
    tone = 'default',
    size = 'row'
  }: {
    vendor: Vendor | null;
    model?: string;
    title?: string;
    /** Цвет точки состояния: работа, ожидание владельца, сбой или покой. */
    tone?: string;
    size?: 'row' | 'small';
  } = $props();

  const logo = $derived(vendor ? LOGOS[vendor] : undefined);
  const letter = $derived(vendorLetter(model, title));
</script>

<span class="avatar size-{size}" data-vendor={vendor ?? 'letter'} aria-hidden="true">
  {#if logo}
    <svg viewBox={logo.viewBox} fill-rule={logo.evenodd ? 'evenodd' : undefined} focusable="false">
      {#each logo.paths as d, index (index)}<path {d} />{/each}
    </svg>
  {:else}
    <span class="letter">{letter}</span>
  {/if}
  {#if tone !== 'none'}<span class="dot mark" data-token={tone}></span>{/if}
</span>

<style>
  .avatar { position: relative; flex: none; display: inline-flex; align-items: center; justify-content: center; width: var(--avatar); height: var(--avatar); border-radius: var(--radius-round); background: var(--chip); color: var(--chip-text); }
  .size-small { width: var(--avatar-small); height: var(--avatar-small); }
  svg { width: 56%; height: 56%; fill: currentColor; }
  .letter { font-size: var(--fs-title); font-weight: var(--weight-semibold); line-height: var(--lh-tight); }
  .size-small .letter { font-size: var(--fs-meta); }
  [data-vendor='anthropic'] { background: var(--vendor-anthropic); color: var(--vendor-mark); }
  [data-vendor='openai'] { background: var(--vendor-openai); color: var(--vendor-mark); }
  [data-vendor='mimo'] { background: var(--vendor-mimo); color: var(--vendor-mark); }
  [data-vendor='minimax'] { background: var(--vendor-minimax); color: var(--vendor-mark); }
  [data-vendor='gemini'], [data-vendor='antigravity'] { background: var(--vendor-gemini); color: var(--vendor-mark); }
  [data-vendor='zai'], [data-vendor='xai'], [data-vendor='opencode'] { background: var(--vendor-dark); color: var(--vendor-mark); }
  [data-vendor='deepseek'] { background: var(--vendor-light); color: var(--vendor-deepseek); }
  [data-vendor='meta'] { background: var(--vendor-light); color: var(--vendor-meta); }
  [data-vendor='moonshot'] { background: var(--vendor-light); color: var(--vendor-ink); }
  [data-vendor='qwen'] { background: var(--vendor-light); color: var(--vendor-qwen); }
  [data-vendor='mistral'] { background: var(--vendor-light); color: var(--vendor-mistral); }
  /* Тёмный и светлый круг на фоне той же светлоты держит тонкий контур линии темы. */
  [data-vendor='zai'], [data-vendor='xai'], [data-vendor='opencode'], [data-vendor='deepseek'], [data-vendor='meta'], [data-vendor='moonshot'], [data-vendor='qwen'], [data-vendor='mistral'] { outline: var(--hairline) solid var(--line); outline-offset: calc(var(--hairline) * -1); }
  [data-vendor='anthropic'] svg, [data-vendor='zai'] svg, [data-vendor='xai'] svg, [data-vendor='opencode'] svg { width: 50%; height: 50%; }
  /* Кольцо точки — фон строки под ней, сама точка — внутренний слой цвета состояния. */
  .dot { position: absolute; right: calc(var(--s1) * -0.5); bottom: calc(var(--s1) * -0.5); width: var(--avatar-dot); height: var(--avatar-dot); box-sizing: border-box; padding: calc(var(--s1) + var(--hairline) / 2); border-radius: var(--radius-round); background: var(--avatar-ring, var(--bg)); }
  .dot::after { content: ''; display: block; width: 100%; height: 100%; border-radius: var(--radius-round); background: var(--muted); }
  .size-small .dot { width: var(--avatar-dot-small); height: var(--avatar-dot-small); padding: var(--s1); }
  .dot[data-token='--accent']::after { background: var(--accent); }
  .dot[data-token='--blue']::after { background: var(--blue); }
  .dot[data-token='--warning']::after { background: var(--warning); }
  .dot[data-token='--success']::after { background: var(--success); }
  .dot[data-token='--danger']::after { background: var(--danger); }
</style>
