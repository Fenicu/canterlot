<script lang="ts">
  import { untrack } from 'svelte';
  import { blockEndLabel } from '#lib/format/labels.js';
  import { fmt } from '#lib/format/ru.js';
  import { cacheHintText, costLine } from '#lib/format/cost.js';
  import type { RunCost } from '#lib/stores/chat.svelte.js';
  import type { BlockModel, BlockPage } from '#lib/stores/blocks.js';
  import RequestCard from '#lib/requests/RequestCard.svelte';
  import type { TimelineEntry } from '#lib/stores/timeline.js';
  import type { FeedFolds } from '#lib/timeline/folds.js';
  import { agentTitle, handBackText, isMainAgent, toolTitle } from '#lib/timeline/present.js';
  import { liveLineQuiet } from '#lib/timeline/reasoning.js';
  import { apiErrorView } from '#lib/timeline/system.js';
  import { shownInCard, type BlockLinks } from '#lib/timeline/rows.js';
  import { foldSteps, type CardItem } from '#lib/timeline/steps.js';
  import { liveWindow } from '#lib/timeline/liveWindow.js';
  import LiveWindow from '#lib/timeline/LiveWindow.svelte';
  import LiveLine from '#lib/timeline/LiveLine.svelte';
  import { blockFold } from '#lib/ui/fold.js';
  import type { InterruptControl } from '#lib/composer/interrupt.svelte.js';
  import type { LeaseWait } from '#lib/format/lease.js';
  import NoticeBlock from '#lib/timeline/NoticeBlock.svelte';
  import ReasoningBlock from '#lib/timeline/ReasoningBlock.svelte';
  import StepGroupRow from '#lib/timeline/StepGroupRow.svelte';
  import QuietRow from '#lib/timeline/cards/QuietRow.svelte';
  import ToolCard from '#lib/timeline/cards/ToolCard.svelte';
  import { cardFold, cardLabel, cardQuiet, cardStays, cardYieldsToRequest } from '#lib/timeline/cards/registry.js';
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
    settled = false,
    notLast = false,
    conversationId,
    now,
    onInfo,
    costs = null,
    load,
    interrupt = null,
    leaseWait = null,
    bookmarks = null,
    markable = true
  }: {
    cardKey: string;
    laneRoot?: string;
    entries: TimelineEntry[];
    folds: FeedFolds;
    links: BlockLinks;
    harness?: { title?: string; provider?: string } | null;
    /** Ход, который идёт сейчас: вызов без результата в нём ещё работает. */
    activeRun?: string | null;
    /** Ход идёт дальше в более поздней карточке (его разрезало сжатие): здесь живой строки нет. */
    settled?: boolean;
    notLast?: boolean;
    conversationId: string;
    now: number;
    onInfo?: (runId: string) => void;
    /** Расход ходов разговора: строка цены под законченным ответом. */
    costs?: ReadonlyMap<string, RunCost> | null;
    load: (blockId: string, offset: number) => Promise<BlockPage>;
    interrupt?: InterruptControl | null;
    /** Процесс текущего хода ждёт очереди тяжёлых задач машины. */
    leaseWait?: LeaseWait | null;
    /** Закладки открытого разговора: карточка держит одну отметку на верхнем блоке. */
    bookmarks?: ConversationBookmarks | null;
    markable?: boolean;
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
  const streaming = $derived(!settled && all.some((block) => block.state === 'streaming'));
  const active = $derived(all.findLast((block) => block.state === 'streaming'));
  const started = $derived(all.find((block) => block.state === 'streaming')?.startedAt ?? head?.createdAt ?? now);
  const phase = $derived(active?.kind === 'tool_call' || active?.kind === 'tool_result' ? 'tool' : active?.kind === 'reasoning' ? 'thinking' : 'working');
  const agent = $derived(agentTitle(head?.agent ?? '', harness));
  const working = $derived(!settled && activeRun !== null && head?.inputCommandId === activeRun);
  const live = $derived(working || streaming);
  const cost = $derived(run && !live && !notLast ? costs?.get(run) : undefined);
  const costText = $derived(cost ? costLine(cost.usage, cost.limit) : '');
  const hintText = $derived(cost ? cacheHintText(cost.cache_hint) : null);
  // Рассуждение само показывает «Рассуждает» с часами: строка хода не повторяет текст, но остаётся с «Прервать».
  const ownReasoning = $derived(active?.kind === 'reasoning' && shownInCard(active));
  const quietLiveLine = $derived(liveLineQuiet(phase, ownReasoning));
  const report = (block: BlockModel) => block.parentId === laneRoot && block.kind === 'tool_result' && isMainAgent(block.agent);
  // Карточки делегирования, плана и файлов не прячутся в свёрнутые шаги: реестр говорит, какие.
  const stays = (block: BlockModel) => report(block) || cardStays(block);
  const items = $derived(foldSteps(blocks, { links, working, live, folds, stays, quiet: cardQuiet, label: cardLabel, fold: cardFold }));
  const view = $derived(folds.view(cardKey, items, live));
  // Живое окно: пока ход идёт, шаги текут хвостом, а не сворачиваются на глазах.
  const windowState = $derived(liveWindow(blocks, { links, working, stays, label: cardLabel, harness }));
  const windowFull = $derived(folds.isOpen(`window:${cardKey}`));
  const windowShown = $derived(live && windowState.stream.length > 0);
  const beforeKeys = $derived(new Set(windowState.before.map((entry) => entry.key)));
  // В полном списке вступление уже стоит над окном: из списка его строки уходят, место то же.
  const windowRest = $derived(view.shown.filter((item) => !beforeKeys.has(item.key)));
  // Ход кончился: окно уступает сегодняшнему виду, а лента у хвоста остаётся прижатой.
  let wasLive = untrack(() => live);
  $effect.pre(() => {
    if (live) {
      wasLive = true;
      return;
    }
    if (!wasLive) return;
    wasLive = false;
    folds.autoChange(true);
  });
  function toggleWindow(): void {
    folds.toggle(`window:${cardKey}`, windowFull);
  }
  const topBlocks = $derived(all.filter((block) => block.parentId === null));
  const anchor = $derived(bookmarkAnchor(topBlocks));
  // Отмеченным карточка считается по любому верхнему блоку, а снимает все такие отметки.
  const markedIds = $derived(topBlocks.filter((block) => bookmarks?.has('block', block.id) ?? false).map((block) => block.id));
  const marked = $derived(markedIds.length > 0);
</script>

{#snippet entryRow(entry: TimelineEntry)}
  {#if entry.kind === 'request'}
    <RequestCard request={entry.request} onDecision={async () => undefined} {now} open={folds.isOpen(entry.key)} onToggle={() => folds.toggle(entry.key)} />
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
      {#if !apiErrorView(block, harness)}<NoticeBlock {block} />{/if}
    {:else if block.kind === 'reasoning'}
      <ReasoningBlock {block} {folds} {now} />
    {:else}
      {#if block.state === 'incomplete'}<p class="incomplete">Неполный ответ</p>{/if}
      <TextBlock {block} />
      {#if block.state === 'incomplete' && block.incompleteReason}<p class="incomplete">{blockEndLabel(block.incompleteReason)}</p>{/if}
    {/if}
  {/if}
{/snippet}

{#snippet itemRow(item: CardItem)}
  <div class="entry" data-key={item.key}>
    {#if item.kind === 'group'}
      <StepGroupRow group={item} {folds} row={entryRow} />
    {:else if item.kind === 'quiet'}
      <QuietRow group={item} {folds} row={entryRow} />
    {:else}
      {@render entryRow(item.entry)}
    {/if}
  </div>
{/snippet}

<article class="card">
  {#if windowShown}
    {#each windowState.before as entry (entry.key)}
      <div class="entry" data-key={entry.key}>{@render entryRow(entry)}</div>
    {/each}
    <div class="live-fold" out:blockFold>
      <LiveWindow view={windowState} {now} open={windowFull} onToggle={toggleWindow} />
    </div>
    {#if windowFull}
      {#each windowRest as item (item.key)}
        {@render itemRow(item)}
      {/each}
    {:else}
      {#each windowState.requests as entry (entry.key)}
        <div class="entry" data-key={entry.key}>{@render entryRow(entry)}</div>
      {/each}
    {/if}
  {:else}
    {#each view.shown as item (item.key)}
      {#if item.key === view.seam}
        <button type="button" class="more" onclick={() => folds.reveal(cardKey)}>Показать ещё {view.hidden} {fmt.plural(view.hidden, ['шаг', 'шага', 'шагов'])}</button>
      {/if}
      {@render itemRow(item)}
    {/each}
  {/if}
  {#if live && head}
    <LiveLine {phase} tool={active ? toolTitle(active).name : ''} startedAt={started} {now} {interrupt} waiting={working ? leaseWait : null} quiet={quietLiveLine} />
  {/if}
  {#if head}
    <footer class="byline" data-role="answer-footer">
      <p><strong>{agent}</strong><time datetime={new Date(head.createdAt).toISOString()} use:timeTip={head.createdAt}>{fmt.clock(head.createdAt)}</time></p>
      <div class="footer-actions">
        {#if anchor && markable}
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
    {#if costText && run}
      <button type="button" class="cost" data-role="run-cost" onclick={() => onInfo?.(run)}>{costText}{#if hintText}<span class="hint" data-role="cache-hint">{hintText}</span>{/if}</button>
    {/if}
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
  /* В ветке у подписи нет кнопок: пустая полоса высотой с кнопку делала карточку перегруженной снизу. */
  .byline:not(:has(.footer-actions > *)) { min-height: var(--s0); }
  .byline p { display: flex; align-items: baseline; gap: var(--s4); min-width: 0; margin: 0; }
  strong { font-weight: var(--weight-semibold); }
  time { color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .byline :global(button) { color: var(--secondary); }
  /* Строка цены: тихая, под подписью; нажатие открывает сведения о ходе. */
  .cost { align-self: flex-start; background: none; max-width: 100%; min-height: var(--target); padding: var(--s0); text-align: left; color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
  /* Подсказка о кеше: своя строка под цифрами, на узком экране обрезается многоточием, а не ломает строку. */
  .hint { display: block; max-width: 100%; margin-top: var(--s0); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .footer-actions { display: flex; align-items: center; gap: var(--s1); }
  .entry { min-width: 0; }
  /* Окно хода складывается тем же движением, что раскрытая строка ленты. */
  .live-fold { display: grid; grid-template-rows: 1fr; min-width: 0; }
  .live-fold > :global(*) { min-height: 0; overflow: hidden; }
  .entry:has(> :global(.text)) { margin-block: var(--s2); }
  .report { display: flex; flex-direction: column; gap: var(--s2); margin-top: var(--s2); }
  h3 { margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .incomplete { margin: 0; color: var(--warning); font-size: var(--fs-sm); }
  .more { align-self: flex-start; min-height: var(--target); padding-inline: var(--s6); color: var(--accent-link); font-size: var(--fs-sm); }
</style>
