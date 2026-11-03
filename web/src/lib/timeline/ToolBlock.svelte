<script lang="ts">
  import type { Component } from 'svelte';
  import type { BackgroundFact } from '#lib/stores/background.svelte.js';
  import type { BlockMedia, BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import { stripAnsi } from '#lib/render/ansi.js';
  import { fmt } from '#lib/format/ru.js';
  import { callCaption, firstLine, toolBackground, toolError, toolLabel, toolPillText, toolTiming, toolTitle } from '#lib/timeline/present.js';
  import StepHead from '#lib/timeline/StepHead.svelte';
  import AttachmentTile from '#lib/timeline/AttachmentTile.svelte';
  import { attachmentUrl } from '#lib/timeline/attachments.js';
  import ToolOutput from '#lib/timeline/ToolOutput.svelte';

  let {
    block,
    result = null,
    background = null,
    working = false,
    now,
    open,
    onToggle,
    load
  }: {
    block: BlockModel;
    result?: BlockModel | null;
    background?: BackgroundFact | null;
    working?: boolean;
    now: number;
    open: boolean;
    onToggle: () => void;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
  } = $props();
  let viewing = $state<number | null>(null);
  let Viewer = $state<Component<{ shots: BlockMedia[]; start?: number; title: string; onclose: () => void }> | null>(null);
  let FileViewer = $state<Component<{ shots: { media_id: string; src: string; mime: string; caption: string }[]; start?: number; title: string; heading: string; onclose: () => void }> | null>(null);

  async function view(index: number): Promise<void> {
    Viewer ??= (await import('#lib/timeline/MediaViewer.svelte')).default;
    viewing = index;
  }
  // Сводка приходит с финалом блока и сама по себе не реактивна.
  const title = $derived.by(() => {
    void block.state;
    return toolTitle(block);
  });
  const label = $derived(toolLabel(title));
  // Результат без своего вызова: имени нет, строку объясняет начало вывода.
  const detail = $derived(title.name ? [title.server, label.text].filter(Boolean).join(' · ') : firstLine(stripAnsi(block.text)));
  const called = $derived(block.kind === 'tool_call' && block.text.trim().length > 0);
  const timing = $derived(toolTiming(block, result, working, background));
  const running = $derived(timing.running || block.state === 'stale');
  const error = $derived(toolError(block, result) || background?.state === 'failed');
  const launched = $derived(!background && !error && toolBackground(result));
  // Оборванный ход называется так же, как оборванный текст рядом: «Прервано», а не «Ошибка».
  const stopped = $derived(!error && !running && (block.state === 'incomplete' || background?.state === 'stopped'));
  const status = $derived(launched ? 'В фоне' : stopped ? 'Прервано' : toolPillText({ ...timing, error, now }));
  const shots = $derived(block.media);
  const readyShots = $derived(shots.filter((shot) => shot.ready));
  const files = $derived(block.files);
  const fileShots = $derived(
    files
      .filter((file) => file.mime.startsWith('image/'))
      .map((file) => ({
        media_id: file.attachment_id,
        src: attachmentUrl(file.attachment_id),
        mime: file.mime,
        caption: [file.title || file.name, file.size > 0 ? fmt.bytes(file.size) : ''].filter(Boolean).join(' · ')
      }))
  );
  let fileViewing: number | null = $state(null);

  async function viewFile(file: { attachment_id: string; mime: string }): Promise<void> {
    if (!file.mime.startsWith('image/')) return;
    FileViewer ??= (await import('#lib/timeline/MediaViewer.svelte')).default;
    fileViewing = Math.max(0, fileShots.findIndex((shot) => shot.media_id === file.attachment_id));
  }
  const isCliPolicy = $derived(block.approvedBy === 'cli_policy');
</script>

<div class="tool">
  <StepHead name={title.name || 'Шаг'} {detail} dir={label.dir} {status} state={error ? 'error' : running ? 'running' : stopped ? 'stopped' : launched ? 'background' : 'done'} {open} ontoggle={onToggle}>
    {#snippet badge()}
      {#if background}<span class="policy-badge">Фоном</span>{/if}
      {#if isCliPolicy}<span class="policy-badge">Разрешено правилами CLI</span>{/if}
    {/snippet}
  </StepHead>
  {#if files.length > 0}
    <ul class="files">
      {#each files as file (file.attachment_id)}
        <li>
          <AttachmentTile
            file={{
              attachment_id: file.attachment_id,
              name: file.name,
              mime: file.mime,
              size: file.size,
              sha256: '',
              image: file.mime.startsWith('image/')
            }}
            onopen={() => void viewFile(file)}
          />
        </li>
      {/each}
    </ul>
  {/if}
  {#if shots.length > 0}
    <div class="shots">
      {#each shots as shot (shot.media_id)}
        <figure class="shot" data-state={shot.ready ? 'ready' : shot.failed ? 'failed' : 'loading'} style:max-width={`${shot.width}px`} style:aspect-ratio={`${shot.width} / ${shot.height}`}>
          {#if shot.ready}
            <button type="button" aria-label="Открыть картинку" onclick={() => void view(readyShots.indexOf(shot))}>
              <img src={`/api/v1/media/${shot.media_id}`} alt="" width={shot.width} height={shot.height} loading="lazy" />
            </button>
          {:else if shot.failed}
            <span>Картинка недоступна</span>
          {:else}
            <span>Картинка загружается</span>
          {/if}
        </figure>
      {/each}
    </div>
  {/if}
  {#if open}
    <div class="fold">
      <div class="body">
        {#if called}<ToolOutput {block} caption={callCaption(block)} {load} />{/if}
        {#if result || !called}<ToolOutput block={result ?? block} running={timing.running} {load} />{/if}
      </div>
    </div>
  {/if}
</div>
{#if viewing !== null && Viewer}
  <Viewer shots={readyShots} start={viewing} title={title.argument || title.name || 'инструмент'} onclose={() => { viewing = null; }} />
{/if}
{#if fileViewing !== null && FileViewer}
  <FileViewer shots={fileShots} start={fileViewing} title={title.argument || title.name || 'файл'} heading="Файл" onclose={() => { fileViewing = null; }} />
{/if}

<style>
  .tool { display: flex; flex-direction: column; min-width: 0; border-radius: var(--radius-card); background: var(--raised); }
  .policy-badge {
    flex: none;
    padding: var(--s1) var(--s3);
    border-radius: var(--radius-pill);
    background: var(--surface);
    color: var(--secondary);
    font-size: var(--fs-xs);
    white-space: nowrap;
  }
  .fold { display: grid; grid-template-rows: 1fr; animation: fold var(--duration-expand) var(--ease-expand); }
  .body { display: flex; flex-direction: column; gap: var(--s2); min-height: 0; overflow: hidden; padding-inline: var(--s2); }
  .body > :global(:last-child) { margin-bottom: var(--s2); }
  @keyframes fold { from { grid-template-rows: 0fr; } to { grid-template-rows: 1fr; } }
  .shots { display: flex; flex-wrap: wrap; gap: var(--s2); padding: var(--s0) var(--s2) var(--s2); }
  .files { display: flex; flex-wrap: wrap; align-items: flex-start; gap: var(--s2); margin: 0; padding: var(--s0) var(--s2) var(--s2); list-style: none; }
  .files li { display: flex; min-width: 0; max-width: 100%; }
  .shot { flex: 1 1 calc(50% - var(--s2)); min-width: 0; max-height: var(--preview-height); margin: 0; overflow: hidden; border-radius: var(--radius-sm); background: var(--chat-bg); }
  .shot[data-state='failed'] { background: var(--danger-soft); }
  .shot:not([data-state='ready']) { display: flex; align-items: center; justify-content: center; min-height: var(--target); padding: var(--s4); color: var(--secondary); font-size: var(--fs-sm); text-align: center; }
  .shot[data-state='failed'] { color: var(--danger); }
  .shot button { display: block; width: 100%; height: 100%; padding: var(--s0); border-radius: var(--radius-sm); background: var(--transparent); }
  img { display: block; width: 100%; height: 100%; object-fit: cover; }
</style>
