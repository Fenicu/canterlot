<script lang="ts">
  import Html from '$lib/render/Html.svelte';
  import { createMarkdown } from '$lib/render/markdown';
  import { StreamingMarkdown } from '$lib/render/streamingMarkdown';
  import { prefersReducedMotion } from '$lib/motion';
  import type { BlockModel } from '$lib/stores/blocks';
  import { revealedText } from '$lib/timeline/present';

  let { block }: { block: BlockModel } = $props();
  const engine = new StreamingMarkdown(createMarkdown());
  let shown = $state('');
  let target = '';
  let receivedAt = 0;

  const streaming = $derived(block.state === 'streaming' || block.state === 'stale');
  const rendered = $derived(engine.update(shown, !streaming));

  $effect(() => {
    const next = block.text;
    const done = block.state === 'final' || block.state === 'incomplete';
    const frame = requestAnimationFrame(() => {
      if (next !== target) {
        target = next;
        receivedAt = performance.now();
      }
      const scale = prefersReducedMotion() ? 0 : motionScale();
      const elapsed = performance.now() - receivedAt;
      const value = done || scale === 0 ? next : revealedText(next, elapsed, scale);
      if (value !== shown) shown = value;
      if (value !== next) requestAnimationFrame(() => { shown = next; });
    });
    return () => cancelAnimationFrame(frame);
  });

  let scaleCache = 1;
  let scaleAt = 0;

  function motionScale(): number {
    const now = performance.now();
    if (now - scaleAt < 500) return scaleCache;
    scaleAt = now;
    const raw = getComputedStyle(document.documentElement).getPropertyValue('--motion-scale').trim();
    const value = Number(raw);
    scaleCache = Number.isFinite(value) ? value : 1;
    return scaleCache;
  }
</script>

<div class="text" data-block={block.id}>
  {#each rendered.chunks as chunk (chunk.id)}
    <Html html={chunk.html} />
  {/each}
  {#if rendered.tail.html}
    <div class="tail"><Html html={rendered.tail.html} /></div>
  {/if}
  {#if streaming}<span class="caret" aria-hidden="true"></span>{/if}
</div>

<style>
  .text { min-width: 0; }
  .text :global(pre) { overflow-x: auto; max-width: 100%; }
  .caret {
    display: inline-block;
    width: var(--hairline);
    height: var(--s4);
    background: var(--accent);
    animation-name: caret;
    animation-duration: var(--duration-caret);
    animation-iteration-count: infinite;
    animation-direction: alternate;
  }
  @keyframes caret { from { opacity: 1; } to { opacity: 0; } }
</style>
