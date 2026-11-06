<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { fmt } from '#lib/format/ru.js';
  import { highlightWithin } from '#lib/render/highlight.js';
  import Html from '#lib/render/Html.svelte';
  import { sharedMarkdown } from '#lib/render/markdown.js';
  import type { AttachmentRef } from '#lib/stores/inputs.js';
  import { attachmentUrl, documentFitsViewer } from '#lib/timeline/attachments.js';
  import { DocumentTooLarge, fetchDocumentText } from '#lib/timeline/document.js';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';

  let { file, onclose }: { file: AttachmentRef; onclose: () => void } = $props();

  type Load =
    | { kind: 'loading' }
    | { kind: 'ready'; text: string }
    | { kind: 'error' }
    | { kind: 'too_large' };

  const name = $derived(file.name || 'Документ');
  const url = $derived(attachmentUrl(file.attachment_id));
  const size = $derived(file.size > 0 ? fmt.bytes(file.size) : '');
  const subtitle = $derived([size, 'Markdown'].filter(Boolean).join(' · '));

  let load = $state<Load>(documentFitsViewer(untrack(() => file.size)) ? { kind: 'loading' } : { kind: 'too_large' });
  let attempt = $state(0);
  let mode = $state<'view' | 'source'>('view');
  let copied = $state<'idle' | 'done' | 'failed'>('idle');
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let documentHost = $state<HTMLDivElement>();
  let stopHighlight: (() => void) | null = null;

  const html = $derived(load.kind === 'ready' ? sharedMarkdown.render(load.text) : '');

  $effect(() => {
    const known = file.size;
    void attempt;
    if (!documentFitsViewer(known)) {
      load = { kind: 'too_large' };
      return;
    }
    const controller = new AbortController();
    load = { kind: 'loading' };
    fetchDocumentText(url, controller.signal).then(
      (text) => {
        if (!controller.signal.aborted) load = { kind: 'ready', text };
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        load = error instanceof DocumentTooLarge ? { kind: 'too_large' } : { kind: 'error' };
      }
    );
    return () => controller.abort();
  });

  // Подсветка — только у готового текста в режиме просмотра: исходник остаётся моноширинным.
  $effect(() => {
    if (load.kind !== 'ready' || mode !== 'view' || !documentHost) return;
    void html;
    stopHighlight?.();
    stopHighlight = highlightWithin(documentHost);
    return () => {
      stopHighlight?.();
      stopHighlight = null;
    };
  });

  async function copyText() {
    if (load.kind !== 'ready') return;
    try {
      if (typeof navigator === 'undefined' || !navigator.clipboard?.writeText) {
        throw new Error('clipboard_unavailable');
      }
      await navigator.clipboard.writeText(load.text);
      copied = 'done';
    } catch {
      copied = 'failed';
    }
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => {
      copied = 'idle';
    }, 2000);
  }

  onDestroy(() => {
    if (copyTimer) clearTimeout(copyTimer);
  });
</script>

<Sheet open title={name} {subtitle} routeKey="document-viewer" {onclose}>
  <div class="document-viewer">
    <div class="toolbar">
      {#if load.kind === 'ready'}
        <Segmented
          name="document-view"
          label="Вид документа"
          bind:value={mode}
          options={[
            { value: 'view', label: 'Просмотр' },
            { value: 'source', label: 'Исходник' }
          ]}
        />
      {/if}
      <div class="actions">
        {#if load.kind === 'ready'}
          <Button onclick={() => void copyText()}>
            {copied === 'done' ? 'Скопировано' : copied === 'failed' ? 'Не удалось скопировать' : 'Копировать текст'}
          </Button>
        {/if}
        <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
        <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
        <a class="download" href={url} download={name} aria-label={`Скачать ${name}`}><Icon name="download" />Скачать</a>
      </div>
    </div>

    {#if load.kind === 'loading'}
      <p class="status" role="status">Загрузка документа…</p>
    {:else if load.kind === 'too_large'}
      <div class="status">
        <p>Файл большой для просмотра</p>
        <p class="hint">Разбирать его целиком слишком долго — скачайте и откройте в редакторе.</p>
      </div>
    {:else if load.kind === 'error'}
      <div class="status" role="alert">
        <p>Не удалось загрузить документ</p>
        <div class="actions">
          <Button onclick={() => { attempt += 1; }}>Повторить</Button>
        </div>
      </div>
    {:else if mode === 'source'}
      <pre class="source"><code>{load.text}</code></pre>
    {:else}
      <div class="prose document-text" bind:this={documentHost}><Html html={html} /></div>
    {/if}
  </div>
</Sheet>

<style>
  /* На телефоне лист занимает почти весь экран: документ читают, а не заглядывают в него. */
  :global(dialog:has(.document-viewer)) { height: 90dvh; }
  :global([data-layout='desktop'] dialog:has(.document-viewer)) { height: auto; max-height: 85dvh; }
  .document-viewer { display: flex; flex-direction: column; gap: var(--s6); min-width: 0; }
  .toolbar { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--s4); min-width: 0; }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s2); min-width: 0; }
  .download { display: inline-flex; align-items: center; gap: var(--s4); min-height: var(--target); padding: var(--s2) var(--s8); border-radius: var(--radius-pill); background: var(--raised); color: var(--text); font-size: var(--fs-meta); text-decoration: none; }
  .status { display: flex; flex-direction: column; align-items: flex-start; gap: var(--s2); margin: 0; padding-block: var(--s6); }
  .status p { margin: 0; }
  .hint { color: var(--secondary); font-size: var(--fs-sm); }
  .document-text { min-width: 0; }
  .source { max-width: 100%; margin: 0; padding: var(--s5) var(--s6); overflow-x: auto; border-radius: var(--radius-sm); background: var(--code); font-size: var(--fs-meta); line-height: var(--lh-body); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
