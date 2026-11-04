<script lang="ts">
  import { blockEndLabel } from '#lib/format/labels.js';
  import { fmt } from '#lib/format/ru.js';
  import type { BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import RequestCard from '#lib/requests/RequestCard.svelte';
  import type { TimelineEntry } from '#lib/stores/timeline.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';
  import { ASK_TOOL, agentTitle, handBackText, isMainAgent, toolTitle } from '#lib/timeline/present.js';
  import { isSubagentCall, shownInCard, type BlockLinks } from '#lib/timeline/rows.js';
  import { foldSteps } from '#lib/timeline/steps.js';
  import LiveLine from '#lib/timeline/LiveLine.svelte';
  import NoticeBlock from '#lib/timeline/NoticeBlock.svelte';
  import ReasoningBlock from '#lib/timeline/ReasoningBlock.svelte';
  import StepGroupRow from '#lib/timeline/StepGroupRow.svelte';
  import SubagentRow from '#lib/timeline/SubagentRow.svelte';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import ToolBlock from '#lib/timeline/ToolBlock.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';

  let {
    cardKey,
    laneRoot,
    entries,
    folds,
    links,
    harness,
    activeRun = null,
    conversationId,
    now,
    onInfo,
    load
  }: {
    cardKey: string;
    laneRoot?: string;
    entries: TimelineEntry[];
    folds: FeedFolds;
    links: BlockLinks;
    harness?: { title?: string; provider?: string } | null;
    /** Ход, который идёт сейчас: вызов без результата в нём ещё работает. */
    activeRun?: string | null;
    conversationId: string;
    now: number;
    onInfo?: (runId: string) => void;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
  } = $props();

  const all = $derived(entries.flatMap((entry) => (entry.kind === 'block' ? [entry.block] : [])));
  // Текст рассуждения читается здесь один раз: токен меняет строку только тогда, когда пустое рассуждение обретает текст.
  const hollow = $derived(entries.filter((entry) => entry.kind === 'block' && !shownInCard(entry.block)).map((entry) => entry.key).join(' '));
  const blocks = $derived.by(() => {
    const empty = new Set(hollow.split(' '));
    const list = entries.filter((entry) => !empty.has(entry.key));
    // Вопрос и ответ показывает его запрос, строка вызова их только повторяла бы.
    return list.filter((entry, index) => !(entry.kind === 'block' && entry.block.toolName === ASK_TOOL && list[index + 1]?.kind === 'request'));
  });
  const head = $derived(all[0]);
  const streaming = $derived(all.some((block) => block.state === 'streaming'));
  const active = $derived(all.findLast((block) => block.state === 'streaming'));
  const started = $derived(all.find((block) => block.state === 'streaming')?.startedAt ?? head?.createdAt ?? now);
  const phase = $derived(active?.kind === 'tool_call' || active?.kind === 'tool_result' ? 'tool' : active?.kind === 'reasoning' ? 'thinking' : 'working');
  const agent = $derived(agentTitle(head?.agent ?? '', harness));
  const working = $derived(activeRun !== null && head?.inputCommandId === activeRun);
  const live = $derived(working || streaming);
  const report = (block: BlockModel) => block.parentId === laneRoot && block.kind === 'tool_result' && isMainAgent(block.agent);
  const items = $derived(foldSteps(blocks, { links, working, live, folds, stays: report }));
  const view = $derived(folds.view(cardKey, items, live));
</script>

{#snippet entryRow(entry: TimelineEntry)}
  {#if entry.kind === 'request'}
    <RequestCard request={entry.request} onDecision={async () => undefined} open={folds.isOpen(entry.key)} onToggle={() => folds.toggle(entry.key)} />
  {:else if entry.kind === 'block'}
    {@const block = entry.block}
    {#if laneRoot && report(block)}
      <section class="report">
        <h3>Итог</h3>
        <TextBlock block={{ id: block.id, text: handBackText(block.text), state: 'final' }} />
      </section>
    {:else if isSubagentCall(block)}
      <SubagentRow call={block} nodes={links.children(block.id)} result={links.result(block.id)} background={links.background(block.id)} {working} {now} {conversationId} />
    {:else if block.kind === 'tool_call' || block.kind === 'tool_result'}
      <ToolBlock {block} result={links.result(block.id)} background={links.background(block.id)} {working} {now} {load} open={folds.isOpen(block.id)} onToggle={() => folds.toggle(block.id)} />
    {:else if block.kind === 'warning' || block.kind === 'error'}
      <NoticeBlock {block} />
    {:else if block.kind === 'reasoning'}
      <ReasoningBlock {block} open={folds.isOpen(block.id)} onToggle={() => folds.toggle(block.id)} />
    {:else}
      {#if block.state === 'incomplete'}<p class="incomplete">Неполный ответ</p>{/if}
      <TextBlock {block} />
      {#if block.state === 'incomplete' && block.incompleteReason}<p class="incomplete">{blockEndLabel(block.incompleteReason)}</p>{/if}
    {/if}
  {/if}
{/snippet}

<article class="card">
  {#each view.shown as item (item.key)}
    {#if item.key === view.seam}
      <button type="button" class="more" onclick={() => folds.reveal(cardKey)}>Показать ещё {view.hidden} {fmt.plural(view.hidden, ['шаг', 'шага', 'шагов'])}</button>
    {/if}
    <div class="entry" data-key={item.key}>
      {#if item.kind === 'group'}
        <StepGroupRow group={item} {folds} row={entryRow} />
      {:else}
        {@render entryRow(item.entry)}
      {/if}
    </div>
  {/each}
  {#if live && head}
    <LiveLine {phase} tool={active ? toolTitle(active).name : ''} startedAt={started} {now} />
  {/if}
  {#if head}
    <footer class="byline" data-role="answer-footer">
      <p><strong>{agent}</strong><time datetime={new Date(head.createdAt).toISOString()}>{fmt.clock(head.createdAt)}</time></p>
      <IconButton icon="info" label="Сведения о ходе" onclick={() => onInfo?.(head.inputCommandId || head.id)} />
    </footer>
  {/if}
</article>

<style>
  .card {
    background: var(--surface);
    border-radius: var(--radius-card);
    padding: var(--s3) var(--s6) var(--s6);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
  }
  .byline { display: flex; justify-content: space-between; align-items: center; gap: var(--s2); margin-top: var(--s2); margin-right: calc(var(--s3) * -1); }
  .byline p { display: flex; align-items: baseline; gap: var(--s4); min-width: 0; margin: 0; }
  strong { font-weight: var(--weight-semibold); }
  time { color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .byline :global(button) { color: var(--secondary); }
  .entry { min-width: 0; }
  .entry:has(> :global(.text)) { margin-block: var(--s2); }
  .report { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s2); }
  h3 { margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .incomplete { margin: 0; color: var(--warning); font-size: var(--fs-sm); }
  .more { align-self: flex-start; min-height: var(--target); padding-inline: var(--s6); color: var(--accent-link); font-size: var(--fs-sm); }
</style>
