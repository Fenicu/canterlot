<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { edgeFade } from '#lib/render/edgeFade.js';
  import { highlightWithin } from '#lib/render/highlight.js';
  import Html from '#lib/render/Html.svelte';
  import { sharedMarkdown } from '#lib/render/markdown.js';
  import { markRevealWords, TextReveal } from '#lib/render/reveal.js';
  import { StreamingMarkdown } from '#lib/render/streamingMarkdown.js';
  import { durationMs, motionScale, prefersReducedMotion } from '#lib/motion.js';
  import type { BlockModel } from '#lib/stores/blocks.js';

  let {
    block
  }: {
    block: Pick<BlockModel, 'id' | 'text' | 'state'> & {
      rev?: number;
      checksum?: string | null;
    };
  } = $props();
  const engine = new StreamingMarkdown(sharedMarkdown);
  const reveal = new TextReveal();

  const streaming = $derived(block.state === 'streaming' || block.state === 'stale');
  const finalKey = $derived(
    streaming
      ? undefined
      : `${block.id}:${block.checksum ?? block.rev ?? 0}:${block.text.length}`
  );
  // Блок, открытый посреди потока, показывает уже принятое сразу; дальше идёт проявление.
  const initial = untrack(() => ({
    text: block.text,
    live: block.state === 'streaming' || block.state === 'stale'
  }));
  reveal.update(initial.text, {
    final: !initial.live,
    now: wallNow(),
    instant: true
  });
  let settled = $state(!initial.live);
  let view = $state({ text: reveal.text, newWords: 0, animate: false });
  let animate = false;
  let frame = 0;

  const rendered = $derived(
    settled
      ? engine.update(block.text, true, finalKey)
      : engine.update(view.text, false, undefined, { prosePlain: true })
  );

  function wallNow(): number {
    return typeof performance === 'undefined' ? Date.now() : performance.now();
  }

  function countWords(value: string): number {
    return value.match(/\S+/g)?.length ?? 0;
  }

  function schedule(): void {
    if (frame) return;
    frame = requestAnimationFrame(tick);
  }

  function tick(): void {
    frame = 0;
    if (settled) return;
    reveal.tick(wallNow());
    pump();
  }

  function pump(): void {
    if (settled) return;
    const next = reveal.text;
    if (next !== view.text) {
      // Хвост растёт только вперёд; замена снимка не должна переигрываться.
      const delta = next.startsWith(view.text) ? next.slice(view.text.length) : '';
      view = { text: next, newWords: countWords(delta), animate };
    }
    if (!reveal.done) schedule();
    else if (!streaming) settled = true;
  }

  $effect(() => {
    const text = block.text;
    const state = block.state;
    const done = state !== 'streaming' && state !== 'stale';
    if (settled && done) return;
    const instant = prefersReducedMotion() || motionScale() === 0;
    animate = !instant;
    reveal.update(text, {
      final: done,
      now: wallNow(),
      instant,
      scale: instant ? 0 : motionScale(),
      finishMs: durationMs('--duration-text-reveal')
    });
    pump();
  });

  onDestroy(() => {
    if (frame) cancelAnimationFrame(frame);
  });

  /** Простой хвост дописывается: показанные слова не пересобираются, новые проявляются. */
  function revealTail(node: HTMLElement, value: { text: string; animate: boolean }) {
    let shown = '';

    const append = (value: string, animated: boolean) => {
      for (const part of value.match(/(\s+|\S+)/g) ?? []) {
        if (!animated || /^\s+$/.test(part) || part.length > 600) {
          node.append(document.createTextNode(part));
          continue;
        }
        const word = document.createElement('span');
        word.className = 'reveal-word';
        word.textContent = part;
        node.append(word);
        word.addEventListener(
          'animationend',
          () => {
            const text = document.createTextNode(word.textContent ?? '');
            word.replaceWith(text);
            const previous = text.previousSibling;
            if (previous?.nodeType === Node.TEXT_NODE) {
              (previous as Text).appendData(text.data);
              text.remove();
            }
          },
          { once: true }
        );
      }
    };

    const update = (next: { text: string; animate: boolean }) => {
      if (next.text.startsWith(shown)) {
        append(next.text.slice(shown.length), next.animate && shown.length > 0);
      } else {
        node.replaceChildren();
        append(next.text, false);
      }
      shown = next.text;
    };

    update(value);
    return { update };
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

<div class="text prose" data-block={block.id} use:highlightFinal={streaming ? null : block.text} use:edgeFade>
  {#each rendered.chunks as chunk (chunk.id)}
    <Html html={chunk.html} copy source={streaming ? undefined : block.text} final={!streaming} />
  {/each}
  {#if rendered.tail.plain}
    <div class="tail"><div use:revealTail={{ text: rendered.tail.text ?? '', animate: view.animate }}></div>{#if streaming}<span class="caret" aria-hidden="true"></span>{/if}</div>
  {:else if rendered.tail.html}
    <div class="tail"><Html html={markRevealWords(rendered.tail.html, view.animate ? view.newWords : 0)} copy />{#if streaming}<span class="caret" aria-hidden="true"></span>{/if}</div>
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
