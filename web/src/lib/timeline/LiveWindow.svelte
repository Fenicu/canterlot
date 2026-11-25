<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { stripAnsi } from '#lib/render/ansi.js';
  import type { BlockModel } from '#lib/stores/blocks.js';
  import { cleanToolOutput, firstLine, liveElapsed, prettyToolOutput } from '#lib/timeline/present.js';
  import StepHead from '#lib/timeline/StepHead.svelte';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import type { LiveWindowView, WindowItem } from '#lib/timeline/liveWindow.js';

  let {
    view,
    now,
    open = false,
    onToggle
  }: {
    view: LiveWindowView;
    now: number;
    /** Полный список открыт: окно уже не рисуется, шапка остаётся и возвращает к окну. */
    open?: boolean;
    onToggle: () => void;
  } = $props();

  /** Под шапкой видно только последние строки вывода: выше всё равно срезано верхом окна. */
  const OUT_LINES = 4;
  const head = $derived(view.head);
  const name = $derived(head.count > 0 ? `${head.count} ${fmt.plural(head.count, ['шаг', 'шага', 'шагов'])}` : 'Шаги');
  const elapsed = $derived(head.startedAt > 0 ? fmt.duration(liveElapsed(now - head.startedAt)) : '');
  const status = $derived([elapsed, head.failed > 0 ? `Ошибка ${head.failed}` : ''].filter(Boolean).join(' · '));

  type ToolItem = Extract<WindowItem, { kind: 'tool' }>;

  function streaming(block: BlockModel): boolean {
    return block.state === 'streaming' || block.state === 'stale';
  }

  function toolStatus(item: ToolItem): string {
    if (item.failed) return 'Ошибка';
    if (item.running) return fmt.duration(liveElapsed(now - item.startedAt));
    if (item.finishedAt === null) return '';
    return fmt.duration(Math.max(0, item.finishedAt - item.startedAt));
  }

  /** Вывод вызова — как в `ToolOutput`: снять ANSI, обёртку CLI и сжать JSON, затем хвост. */
  function output(item: ToolItem): string[] {
    if (!item.result) return [];
    const clean = prettyToolOutput(cleanToolOutput(stripAnsi(item.result.text))).replace(/\n+$/, '');
    if (clean.length === 0) return [];
    return clean.split('\n').slice(-OUT_LINES);
  }

  function keydown(event: KeyboardEvent): void {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    onToggle();
  }
</script>

<section class="live-window" data-role="live-window">
  <div class="head" data-role="live-window-head">
    <StepHead {name} detail={head.tally} {status} state={head.failed > 0 ? 'error' : 'done'} {open} ontoggle={onToggle} />
  </div>
  {#if !open}
    <div class="body" data-role="live-window-body" role="button" tabindex="0" aria-label="Открыть все шаги хода" onclick={onToggle} onkeydown={keydown}>
      <div class="stream">
        {#each view.stream as item (item.key)}
          {#if item.kind === 'reasoning'}
            <p class="reasoning" data-role="live-reasoning" data-streaming={streaming(item.block) ? '' : undefined}>{item.block.text}{#if streaming(item.block)}<span class="caret" aria-hidden="true"></span>{/if}</p>
          {:else if item.kind === 'text'}
            <TextBlock block={item.block} />
          {:else if item.kind === 'notice'}
            <p class="notice" data-kind={item.block.kind}>{firstLine(item.block.text)}</p>
          {:else if item.kind === 'result'}
            <p class="orphan">{firstLine(stripAnsi(item.block.text))}</p>
          {:else if item.kind === 'tool'}
            {@const lines = output(item)}
            <div class="tool" data-failed={item.failed ? '' : undefined}>
              <div class="tool-head">
                <span class="tool-name">{item.name}</span>
                {#if item.detail}<span class="tool-detail">{item.detail}</span>{/if}
                <span class="tool-status" data-state={item.failed ? 'error' : item.running ? 'running' : 'done'}>
                  {#if item.running && !item.failed}<span class="spin" data-role="tool-spin" aria-hidden="true"></span>{/if}
                  {toolStatus(item)}
                </span>
              </div>
              {#if lines.length > 0}
                <div class="out" data-failed={item.failed ? '' : undefined}>
                  {#each lines as line, index (index)}<div class="line">{line}</div>{/each}
                </div>
              {/if}
            </div>
          {/if}
        {/each}
      </div>
    </div>
  {/if}
</section>

<style>
  .live-window { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; }
  .head { border-radius: var(--radius-card); background: var(--raised); }
  /* Окно растёт с содержимым до предела, дальше стоит на нём: новые строки внизу,
     старые уходят под верхний край. Прокрутки нет: вложенная ловит палец у ленты. */
  .body {
    position: relative;
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    max-height: var(--window-max);
    padding: var(--s2) var(--s4) var(--s3);
    overflow: hidden;
    border-radius: var(--radius-card);
    background: var(--code);
    cursor: pointer;
    -webkit-mask-image: linear-gradient(to bottom, transparent 0, currentColor var(--s10));
    mask-image: linear-gradient(to bottom, transparent 0, currentColor var(--s10));
  }
  .stream { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; }
  .reasoning { margin: 0; color: var(--secondary); font-size: var(--fs-meta); line-height: var(--lh-body); white-space: pre-wrap; overflow-wrap: anywhere; }
  .reasoning[data-streaming] { color: var(--text); }
  .caret {
    display: inline-block;
    width: var(--caret-width);
    height: var(--caret-height);
    margin-left: var(--s1);
    vertical-align: text-bottom;
    background: var(--accent);
    animation: caret var(--duration-caret) alternate infinite;
  }
  @keyframes caret { from { opacity: 1; } to { opacity: 0; } }
  .tool { display: flex; flex-direction: column; gap: var(--s1); min-width: 0; }
  .tool-head { display: flex; align-items: baseline; gap: var(--s2); min-width: 0; }
  .tool-name { flex: none; color: var(--accent); font-size: var(--fs-meta); font-weight: var(--weight-semibold); white-space: nowrap; }
  .tool-detail { flex: 1 1 0; min-width: 0; color: var(--text); font-family: var(--font-mono); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .tool-status { flex: none; display: inline-flex; align-items: center; gap: var(--s2); color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .tool-status[data-state='error'] { color: var(--danger); }
  .spin {
    flex: none;
    width: var(--s5);
    height: var(--s5);
    border: var(--s1) solid var(--accent-soft);
    border-top-color: var(--accent);
    border-radius: var(--radius-round);
    animation: spin var(--duration-stage-spin) var(--ease-stage-spin) infinite;
  }
  @keyframes spin { to { transform: rotate(1turn); } }
  /* Вывод: хвост последних строк, без переноса. Правило слева краснеет упавшему вызову. */
  .out { min-width: 0; margin-left: var(--s2); padding-left: var(--s4); border-left: var(--s1) solid var(--line); color: var(--muted); font-family: var(--font-mono); font-size: var(--fs-sm); line-height: var(--lh-code); }
  .out[data-failed] { border-left-color: var(--danger); }
  .line { overflow: hidden; text-overflow: ellipsis; white-space: pre; }
  .notice { margin: 0; color: var(--warning); font-size: var(--fs-sm); }
  .notice[data-kind='error'] { color: var(--danger); }
  .orphan { margin: 0; color: var(--muted); font-family: var(--font-mono); font-size: var(--fs-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  @media (prefers-reduced-motion: reduce) {
    .caret, .spin { animation: none; }
  }
</style>
