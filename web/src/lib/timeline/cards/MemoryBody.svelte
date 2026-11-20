<script lang="ts">
  import Html from '#lib/render/Html.svelte';
  import { sharedMarkdown } from '#lib/render/markdown.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';

  let { body, cut = false, folds, foldKey, role = 'memory-body' }: { body: string; cut?: boolean; folds: FeedFolds; foldKey: string; role?: string } = $props();
  const html = $derived(sharedMarkdown.render(body));
  const open = $derived(folds.isOpen(foldKey));
  // Короткий текст показан целиком; длинный сложен до двенадцати строк и раскрывается кнопкой.
  const long = $derived(body.length > 480 || body.split('\n').length > 8);
</script>

<!-- Текст записи памяти: markdown, как у ответа агента; длинный сложен, как бриф. -->
<div class="body prose" class:folded={long && !open} data-role={role}><Html {html} /></div>
{#if cut}<p class="note">Текст оборван: история отдаёт только начало вызова</p>{/if}
{#if long}
  <button type="button" class="more" aria-expanded={open} onclick={() => folds.toggle(foldKey)}>{open ? 'Свернуть' : 'Весь текст'}</button>
{/if}

<style>
  .body { min-width: 0; color: var(--secondary); overflow-wrap: anywhere; }
  .body.folded { max-height: calc(var(--fs-body) * var(--lh-body) * 8); overflow: hidden; mask-image: linear-gradient(to bottom, var(--text) calc(100% - var(--s10)), var(--transparent)); }
  .body :global(:is(h1, h2, h3, h4, h5, h6)) { margin-block: var(--s3) var(--s2); font-size: var(--fs-body); }
  .body :global(:first-child) { margin-top: var(--s0); }
  .body :global(:last-child) { margin-bottom: var(--s0); }
  .body :global(pre) { max-width: 100%; overflow-x: auto; }
  .note { margin: var(--s0); color: var(--muted); }
  .more { align-self: flex-start; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
