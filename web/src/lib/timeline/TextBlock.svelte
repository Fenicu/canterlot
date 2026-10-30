<script lang="ts">
  import { highlightWithin } from '#lib/render/highlight.js';
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

  /** Подсветка стартует, только когда текст завершён; пока идёт поток, значение пустое и работы нет. */
  function highlightFinal(node: HTMLElement, text: string | null) {
    let stop: () => void = () => undefined;
    const apply = (value: string | null) => {
      stop();
      stop = value === null ? () => undefined : highlightWithin(node);
    };
    apply(text);
    return { update: apply, destroy: () => stop() };
  }
</script>

<div class="text prose" data-block={block.id} use:highlightFinal={streaming ? null : block.text}>
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
