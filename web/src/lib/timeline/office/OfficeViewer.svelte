<script lang="ts">
  import { untrack } from 'svelte';
  import { fmt } from '#lib/format/ru.js';
  import Html from '#lib/render/Html.svelte';
  import { sanitizeDocument } from '#lib/render/sanitize.js';
  import type { AttachmentRef } from '#lib/stores/inputs.js';
  import { OFFICE_VIEW_MAX_BYTES, attachmentUrl, officeFitsViewer, officeLabel, type OfficeKind } from '#lib/timeline/attachments.js';
  import { DocumentTooLarge, fetchDocumentBytes } from '#lib/timeline/document.js';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import { parseOffice } from './parse.js';
  import type { OfficeFailure, SheetView } from './protocol.js';
  import SheetGrid from './SheetGrid.svelte';

  let { file, kind, onclose }: { file: AttachmentRef; kind: OfficeKind; onclose: () => void } = $props();

  type Load =
    | { kind: 'loading' }
    | { kind: 'document'; html: string }
    | { kind: 'sheets'; sheets: SheetView[] }
    | { kind: 'empty' }
    | { kind: 'failed'; reason: OfficeFailure }
    | { kind: 'error' };

  const name = $derived(file.name || officeLabel(kind));
  const url = $derived(attachmentUrl(file.attachment_id));
  const size = $derived(file.size > 0 ? fmt.bytes(file.size) : '');
  const subtitle = $derived([size, officeLabel(kind)].filter(Boolean).join(' · '));
  const app = $derived(kind === 'word' ? 'Word' : 'Excel');

  let load = $state<Load>(officeFitsViewer(untrack(() => file.size)) ? { kind: 'loading' } : { kind: 'failed', reason: 'too_large' });
  let attempt = $state(0);

  const failure = $derived.by(() => {
    if (load.kind !== 'failed') return null;
    const what = kind === 'word' ? 'документ Word' : 'таблица Excel';
    switch (load.reason) {
      case 'legacy':
        return { title: `Старый формат .${kind === 'word' ? 'doc' : 'xls'} здесь не открывается`, hint: `Просмотр понимает .${kind === 'word' ? 'docx' : 'xlsx'}. Скачайте файл и откройте его в ${app}.` };
      case 'locked':
        return { title: 'Файл защищён паролем', hint: `Пароль здесь не ввести. Скачайте файл и откройте его в ${app}.` };
      case 'too_large':
        return { title: 'Файл большой для просмотра', hint: `Больше 10 МБ или слишком тяжёлый внутри. Скачайте и откройте его в ${app}.` };
      default:
        return { title: 'Не удалось прочитать файл', hint: `Файл повреждён или это не ${what}. Скачайте его, чтобы открыть в другой программе.` };
    }
  });

  // Адреса blob: картинок из .docx живут, пока открыт просмотрщик, и освобождаются при закрытии.
  let images: string[] = [];
  function release() {
    for (const image of images) URL.revokeObjectURL(image);
    images = [];
  }

  async function open(signal: AbortSignal): Promise<Load> {
    const bytes = await fetchDocumentBytes(url, OFFICE_VIEW_MAX_BYTES, signal);
    const reply = await parseOffice(kind, bytes, signal);
    if (signal.aborted) throw signal.reason;
    if (reply.kind === 'failed') return { kind: 'failed', reason: reply.reason };
    if (reply.kind === 'xlsx') return reply.sheets.some((sheet) => sheet.rows.length > 0) ? { kind: 'sheets', sheets: reply.sheets } : { kind: 'empty' };
    images = reply.images.map((image) => URL.createObjectURL(new Blob([image.data], { type: image.type })));
    const html = sanitizeDocument(reply.html, images);
    return html.trim() ? { kind: 'document', html } : { kind: 'empty' };
  }

  $effect(() => {
    const known = file.size;
    void attempt;
    if (!officeFitsViewer(known)) {
      load = { kind: 'failed', reason: 'too_large' };
      return;
    }
    const controller = new AbortController();
    load = { kind: 'loading' };
    open(controller.signal).then(
      (next) => {
        if (!controller.signal.aborted) load = next;
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        load = error instanceof DocumentTooLarge ? { kind: 'failed', reason: 'too_large' } : { kind: 'error' };
      }
    );
    return () => {
      controller.abort();
      release();
    };
  });
</script>

<Sheet open title={name} {subtitle} routeKey="office-viewer" {onclose}>
  <div class="office-viewer" data-kind={kind} data-state={load.kind}>
    <div class="toolbar">
      <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
      <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
      <a class="download" href={url} download={name} aria-label={`Скачать ${name}`}><Icon name="download" />Скачать</a>
    </div>

    {#if load.kind === 'loading'}
      <p class="status" role="status">Открываю…</p>
    {:else if load.kind === 'failed' && failure}
      <div class="status" data-role="office-failure">
        <p>{failure.title}</p>
        <p class="hint">{failure.hint}</p>
      </div>
    {:else if load.kind === 'error'}
      <div class="status" role="alert">
        <p>Не удалось загрузить файл</p>
        <div class="actions">
          <Button onclick={() => { attempt += 1; }}>Повторить</Button>
        </div>
      </div>
    {:else if load.kind === 'empty'}
      <p class="status">{kind === 'word' ? 'В документе нет текста' : 'В таблице нет данных'}</p>
    {:else if load.kind === 'sheets'}
      <SheetGrid sheets={load.sheets} />
    {:else if load.kind === 'document'}
      <div class="prose document-text"><Html html={load.html} /></div>
    {/if}
  </div>
</Sheet>

<style>
  /* Как у просмотрщика Markdown: на телефоне лист почти во весь экран. */
  :global(dialog:has(.office-viewer)) { height: 90dvh; }
  :global([data-layout='desktop'] dialog:has(.office-viewer)) { height: auto; max-height: 85dvh; }
  /* Таблице нужна ширина и постоянная высота: сетка прокручивается внутри, а не тянет лист. */
  :global([data-layout='desktop'] dialog:has(.office-viewer[data-kind='excel'][data-state='sheets'])) { width: min(calc(100% - 2 * var(--s13)), calc(var(--index-max) * 1.25)); height: 85dvh; }
  .office-viewer { display: flex; flex-direction: column; gap: var(--s6); min-width: 0; }
  .office-viewer[data-state='sheets'] { height: 100%; }
  .toolbar { display: flex; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: var(--s4); min-width: 0; }
  .download { display: inline-flex; align-items: center; gap: var(--s4); min-height: var(--target); padding: var(--s2) var(--s8); border-radius: var(--radius-pill); background: var(--raised); color: var(--text); font-size: var(--fs-meta); text-decoration: none; }
  .status { display: flex; flex-direction: column; align-items: flex-start; gap: var(--s2); margin: 0; padding-block: var(--s6); }
  .status p { margin: 0; }
  .hint { color: var(--secondary); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s2); }
  .document-text { min-width: 0; }
  /* Картинка из документа не шире колонки и не выше экрана телефона. */
  .document-text :global(img) { display: block; max-width: 100%; height: auto; max-height: 70dvh; border-radius: var(--radius-sm); }
  /* mammoth кладёт абзацы в ячейки таблицы: внутри ячейки поля абзаца лишние. */
  .document-text :global(:is(td, th) > p) { margin-block: var(--s0); }
  .document-text :global(:is(td, th) > p + p) { margin-top: var(--s2); }
  .document-text :global(:is(sup, sub)) { font-size: var(--fs-xs); }
</style>
