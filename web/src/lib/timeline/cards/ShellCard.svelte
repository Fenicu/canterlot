<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import ToolOutput from '#lib/timeline/ToolOutput.svelte';
  import StatusPill from '#lib/timeline/StatusPill.svelte';
  import { toolBackground, toolPillText, toolTiming } from '#lib/timeline/present.js';
  import CardShell from './CardShell.svelte';
  import CodeLines from './CodeLines.svelte';
  import CopyButton from './CopyButton.svelte';
  import type { ShellData } from './defs/shell';
  import type { CardProps } from './types';

  let { call, data, line, word, grows, quiet, env }: CardProps<ShellData> = $props();
  /** Столько строк команды видно сразу; длинный скрипт раскрывается целиком по кнопке. */
  const SHOWN = 8;
  const key = $derived(`shell:${call.block.id}`);
  const all = $derived(env.folds.isOpen(key));
  const lines = $derived(data.formatted.split('\n'));
  const shown = $derived((all ? lines : lines.slice(0, SHOWN)).map((text) => ({ text })));
  const timing = $derived(toolTiming(call.block, call.result, env.working, env.background));
  const took = $derived(toolPillText({ running: timing.running, error: false, startedAt: timing.startedAt, finishedAt: timing.finishedAt, now: env.now }));
  const outputLines = $derived(call.result && call.result.state !== 'streaming' ? (call.result.lineCount ?? (call.output ? call.output.split('\n').length : 0)) : 0);
  const launched = $derived(toolBackground(call.result));
</script>

<!-- Команда видна всегда, вывод свёрнут: раскрытие показывает его хвост и «Показать полностью». -->
<CardShell {call} {env} {line} {grows} {quiet} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} reveal="вывод">
  {#snippet head()}<span class="title" data-role="shell-title">{data.description || 'Команда'}</span>{/snippet}
  {#snippet badge()}
    {#if env.background || data.background}<StatusPill state="background" text="Фоном" />{:else if launched}<StatusPill state="background" text="В фоне" />{/if}
  {/snippet}
  <CodeLines lines={shown} lang="bash" label="Команда" />
  <div class="meta" data-role="shell-meta">
    {#if lines.length > SHOWN}
      <button type="button" class="more" aria-expanded={all} onclick={() => env.folds.toggle(key)}>{all ? 'Свернуть команду' : `Вся команда · ${lines.length} ${fmt.plural(lines.length, ['строка', 'строки', 'строк'])}`}</button>
    {/if}
    {#if data.cwd}<span class="cwd" data-role="shell-cwd">{data.cwd}</span>{/if}
    {#if data.outcome.exit === 0}<span>код 0</span>{/if}
    {#if took}<span data-role="shell-took">{took}</span>{/if}
    {#if outputLines > 0}<span>вывод · {outputLines} {fmt.plural(outputLines, ['строка', 'строки', 'строк'])}</span>{/if}
    <CopyButton text={data.command} label="Скопировать команду" />
  </div>
  {#snippet fold()}
    {#if call.result}
      <ToolOutput block={call.result} running={call.state === 'running'} load={env.load} />
    {:else}
      <p class="none">{call.state === 'running' ? 'Вывод ещё не пришёл' : 'Вывода нет'}</p>
    {/if}
  {/snippet}
</CardShell>

<style>
  .title { min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; flex-wrap: wrap; align-items: center; column-gap: var(--s5); min-width: 0; margin-block: calc(var(--s2) * -1) calc(var(--s4) * -1); color: var(--secondary); font-size: var(--fs-sm); }
  .cwd { min-width: 0; overflow: hidden; font-family: var(--font-mono); text-overflow: ellipsis; white-space: nowrap; }
  .more { min-height: var(--target); padding: var(--s0); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-sm); }
  .none { margin: var(--s0); padding: var(--s4) var(--s6); border-radius: var(--radius-card); background: var(--code); color: var(--secondary); font-size: var(--fs-sm); }
</style>
