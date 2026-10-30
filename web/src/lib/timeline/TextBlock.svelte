<script lang="ts">
  import Html from '#lib/render/Html.svelte';
  import { createMarkdown } from '#lib/render/markdown.js';
  import { StreamingMarkdown } from '#lib/render/streamingMarkdown.js';
  import type { BlockModel } from '#lib/stores/blocks.js';

  let { block }: { block: BlockModel } = $props();
  const engine = new StreamingMarkdown(createMarkdown());

  const streaming = $derived(block.state === 'streaming' || block.state === 'stale');
  // Дельта рисуется в том же кадре, в котором её применил пакет событий.
  const rendered = $derived(engine.update(block.text, !streaming));

  /** Длинный простой хвост растёт дописыванием в тот же текстовый узел: браузер не пересобирает абзац заново. */
  function appendText(node: HTMLElement, initial: string) {
    const text = document.createTextNode(initial);
    node.append(text);
    return {
      update(next: string) {
        const shown = text.data;
        if (next.length > shown.length && next.startsWith(shown)) text.appendData(next.slice(shown.length));
        else if (next !== shown) text.data = next;
      }
    };
  }
</script>

<div class="text" data-block={block.id}>
  {#each rendered.chunks as chunk (chunk.id)}
    <Html html={chunk.html} />
  {/each}
  {#if rendered.tail.plain}
    <div class="tail"><div use:appendText={rendered.tail.text ?? ''}></div>{#if streaming}<span class="caret" aria-hidden="true"></span>{/if}</div>
  {:else if rendered.tail.html}
    <div class="tail"><Html html={rendered.tail.html} />{#if streaming}<span class="caret" aria-hidden="true"></span>{/if}</div>
  {:else if streaming}
    <span class="caret" aria-hidden="true"></span>
  {/if}
</div>

<style>
  .text { min-width: 0; }
  .text :global(pre) { overflow-x: auto; max-width: 100%; }
  .tail > :global(div), .tail > :global(div > p:last-child) { display: inline; }
  .caret {
    display: inline-block;
    width: var(--caret-width);
    height: var(--caret-height);
    margin-left: var(--s1);
    vertical-align: text-bottom;
    background: var(--accent);
    animation-name: caret;
    animation-duration: var(--duration-caret);
    animation-iteration-count: infinite;
    animation-direction: alternate;
  }
  @keyframes caret { from { opacity: 1; } to { opacity: 0; } }
</style>
