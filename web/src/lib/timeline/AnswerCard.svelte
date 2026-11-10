<script lang="ts">
  import { blockEndLabel } from '#lib/format/labels.js';
  import { fmt } from '#lib/format/ru.js';
  import type { BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import RequestCard from '#lib/requests/RequestCard.svelte';
  import type { TimelineEntry } from '#lib/stores/timeline.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';
  import { agentTitle, handBackText, isMainAgent, toolTitle } from '#lib/timeline/present.js';
  import { liveLineQuiet } from '#lib/timeline/reasoning.js';
  import { shownInCard, type BlockLinks } from '#lib/timeline/rows.js';
  import { foldSteps } from '#lib/timeline/steps.js';
  import LiveLine from '#lib/timeline/LiveLine.svelte';
  import type { InterruptControl } from '#lib/composer/interrupt.svelte.js';
  import type { LeaseWait } from '#lib/format/lease.js';
  import NoticeBlock from '#lib/timeline/NoticeBlock.svelte';
  import ReasoningBlock from '#lib/timeline/ReasoningBlock.svelte';
  import StepGroupRow from '#lib/timeline/StepGroupRow.svelte';
  import QuietRow from '#lib/timeline/cards/QuietRow.svelte';
  import ToolCard from '#lib/timeline/cards/ToolCard.svelte';
  import { cardLabel, cardQuiet, cardStays, cardYieldsToRequest } from '#lib/timeline/cards/registry.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';
  import type { ConversationBookmarks } from '#lib/bookmarks/store.svelte.js';
  import { bookmarkAnchor } from '#lib/bookmarks/model.js';

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
    load,
    interrupt = null,
    leaseWait = null,
    bookmarks = null
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
    interrupt?: InterruptControl | null;
    /** Процесс текущего хода ждёт очереди тяжёлых задач машины. */
    leaseWait?: LeaseWait | null;
    /** Закладки открытого разговора: карточка держит одну отметку на верхнем блоке. */
    bookmarks?: ConversationBookmarks | null;
  } = $props();

  const all = $derived(entries.flatMap((entry) => (entry.kind === 'block' ? [entry.block] : [])));
  // Текст рассуждения читается здесь один раз: токен меняет строку только тогда, когда пустое рассуждение обретает текст.
  const hollow = $derived(entries.filter((entry) => entry.kind === 'block' && !shownInCard(entry.block)).map((entry) => entry.key).join(' '));
  const blocks = $derived.by(() => {
    const empty = new Set(hollow.split(' '));
    const list = entries.filter((entry) => !empty.has(entry.key));
    // Вопрос и ответ показывает его запрос, строка вызова их только повторяла бы.
    return list.filter((entry, index) => !(entry.kind === 'block' && cardYieldsToRequest(entry.block) && list[index + 1]?.kind === 'request'));
  });
  const head = $derived(all[0]);
  // Сведения хаб отдаёт по ходу; у карточки без хода (носитель файла прежних версий) их нет.
  const run = $derived(all.find((block) => block.inputCommandId)?.inputCommandId ?? null);
  const streaming = $derived(all.some((block) => block.state === 'streaming'));
  const active = $derived(all.findLast((block) => block.state === 'streaming'));
  const started = $derived(all.find((block) => block.state === 'streaming')?.startedAt ?? head?.createdAt ?? now);
  const phase = $derived(active?.kind === 'tool_call' || active?.kind === 'tool_result' ? 'tool' : active?.kind === 'reasoning' ? 'thinking' : 'working');
  const agent = $derived(agentTitle(head?.agent ?? '', harness));
  const working = $derived(activeRun !== null && head?.inputCommandId === activeRun);
  const live = $derived(working || streaming);
  // Рассуждение само показывает «Рассуждает» с часами: строка хода не повторяет текст, но остаётся с «Прервать».
  const ownReasoning = $derived(active?.kind === 'reasoning' && shownInCard(active));
  const quietLiveLine = $derived(liveLineQuiet(phase, ownReasoning));
  const report = (block: BlockModel) => block.parentId === laneRoot && block.kind === 'tool_result' && isMainAgent(block.agent);
  // Карточки делегирования, плана и файлов не прячутся в свёрнутые шаги: реестр говорит, какие.
  const items = $derived(foldSteps(blocks, { links, working, live, folds, stays: (block) => report(block) || cardStays(block), quiet: cardQuiet, label: cardLabel }));
  const view = $derived(folds.view(cardKey, items, live));
  const topBlocks = $derived(all.filter((block) => block.parentId === null));
  const anchor = $derived(bookmarkAnchor(topBlocks));
  // Отмеченным карточка считается по любому верхнему блоку, а снимает все такие отметки.
  const markedIds = $derived(topBlocks.filter((block) => bookmarks?.has('block', block.id) ?? false).map((block) => block.id));
  const marked = $derived(markedIds.length > 0);
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
    {:else if block.kind === 'tool_call' || block.kind === 'tool_result'}
      <ToolCard {block} result={links.result(block.id)} background={links.background(block.id)} {working} {now} {load} {links} {folds} {conversationId} open={folds.isOpen(block.id)} onToggle={() => folds.toggle(block.id)} />
    {:else if block.kind === 'warning' || block.kind === 'error'}
      <NoticeBlock {block} />
    {:else if block.kind === 'reasoning'}
      <ReasoningBlock {block} {folds} {now} />
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
      {:else if item.kind === 'quiet'}
        <QuietRow group={item} {folds} row={entryRow} />
      {:else}
        {@render entryRow(item.entry)}
      {/if}
    </div>
  {/each}
  {#if live && head}
    <LiveLine {phase} tool={active ? toolTitle(active).name : ''} startedAt={started} {now} {interrupt} waiting={working ? leaseWait : null} quiet={quietLiveLine} />
  {/if}
  {#if head}
    <footer class="byline" data-role="answer-footer">
      <p><strong>{agent}</strong><time datetime={new Date(head.createdAt).toISOString()} use:timeTip={head.createdAt}>{fmt.clock(head.createdAt)}</time></p>
      <div class="footer-actions">
        {#if anchor}
          <IconButton
            icon="bookmark"
            solid={marked}
            pressed={marked}
            active={marked}
            inert={!bookmarks}
            label={marked ? 'Убрать из избранного' : 'В избранное'}
            onclick={() => {
              if (!bookmarks) return;
              if (marked) {
                for (const id of markedIds) void bookmarks.remove('block', id);
              } else if (anchor) {
                void bookmarks.toggle('block', anchor.id);
              }
            }}
          />
        {/if}
        {#if run}
          <IconButton icon="info" label="Сведения о ходе" onclick={() => onInfo?.(run)} />
        {/if}
      </div>
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
  /* Высота кнопки (i) и без неё: футер карточки без хода не ниже остальных. */
  .byline { display: flex; justify-content: space-between; align-items: center; gap: var(--s2); margin-top: var(--s2); margin-right: calc(var(--s3) * -1); min-height: var(--target); }
  .byline p { display: flex; align-items: baseline; gap: var(--s4); min-width: 0; margin: 0; }
  strong { font-weight: var(--weight-semibold); }
  time { color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .byline :global(button) { color: var(--secondary); }
  .footer-actions { display: flex; align-items: center; gap: var(--s1); }
  .entry { min-width: 0; }
  .entry:has(> :global(.text)) { margin-block: var(--s2); }
  .report { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s2); }
  h3 { margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .incomplete { margin: 0; color: var(--warning); font-size: var(--fs-sm); }
  .more { align-self: flex-start; min-height: var(--target); padding-inline: var(--s6); color: var(--accent-link); font-size: var(--fs-sm); }
</style>
