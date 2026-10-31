<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import { announce } from '#lib/a11y/announce.js';
  import RequestCard from '#lib/requests/RequestCard.svelte';
  import { clock } from '#lib/clock.js';
  import { probe } from '#lib/perf/probe.js';
  import type { BlockPage } from '#lib/stores/blocks.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import AnswerCard from '#lib/timeline/AnswerCard.svelte';
  import UserBubble from '#lib/timeline/UserBubble.svelte';
  import { FeedFolds } from '#lib/timeline/folds.js';
  import { blockLinks, buildRows, dayStart, shownInCard, tailKey, type Row } from '#lib/timeline/rows.js';
  import { ScrollController } from '#lib/timeline/scrollController.js';
  import {
    HeightIndex,
    captureAnchor,
    restoreAnchor,
    rowEstimate,
    viewAnchor,
    visibleRange,
    type ViewAnchor
  } from '#lib/timeline/virtual.js';

  let {
    chat,
    onInfo,
    now = Date.now(),
    laneRoot
  }: {
    chat: ChatStore;
    onInfo?: (runId: string) => void;
    now?: number;
    laneRoot?: string;
  } = $props();
  let host = $state<HTMLDivElement | null>(null);
  let controller = $state<ScrollController | null>(null);
  let range = $state({ start: 0, end: 0, padTop: 0, padBottom: 0 });
  let phase = $state<'idle' | 'streaming' | 'done' | 'error'>('idle');
  let loading = false;
  const ix = new HeightIndex(rowEstimate);
  const known = new SvelteMap<string, number>();
  const typical = new SvelteMap<string, number>();
  let observer: ResizeObserver | null = null;
  let previous: string[] = [];
  let previousTail = '';
  // Пока владелец читает, эта строка стоит на месте, что бы ни менялось выше и ниже неё.
  let held: ViewAnchor | null = null;
  let ownedTop: number | null = null;
  let settling = false;
  let measuring = false;
  let queued: { key: string; height: number }[] = [];
  const folds = new FeedFolds();
  const PIN_PX = 48;

  const today = $derived(dayStart(now));
  const links = $derived(blockLinks(chat.timeline.entries));
  const rows = $derived(buildRows(chat.timeline.entries, { now: today, laneRoot, links }));
  const windowRows = $derived(rows.slice(range.start, range.end));
  const lastInput = $derived(rows.findLast((row) => row.kind === 'input')?.key);
  let seenRows: string | null = null;
  $effect(() => {
    const next = rows.map((row) => row.key).join('\n');
    const node = host;
    if (!node || seenRows === next) return;
    seenRows = next;
    probe.count('timeline_render');
    node.setAttribute('data-renders', String(Number(node.getAttribute('data-renders') ?? '0') + 1));
  });
  const streaming = $derived(chat.timeline.entries.some((entry) => entry.kind === 'block' && entry.block.state === 'streaming'));
  const failed = $derived(chat.timeline.entries.some((entry) => entry.kind === 'block' && (entry.block.kind === 'error' || (entry.block.state === 'incomplete' && shownInCard(entry.block)))));
  const harness = $derived(chat.detail?.harness as { title?: string; provider?: string } | undefined);
  const activeRun = $derived.by(() => {
    const state = chat.activity?.state;
    if (state !== 'working' && state !== 'waiting_for_user') return null;
    return chat.timeline.entries.findLast((entry) => entry.kind === 'block')?.block.inputCommandId ?? null;
  });

  function layout(list: Row[], top: number, viewport: number): void {
    ix.setCount(list.length);
    list.forEach((row, index) => ix.set(index, known.get(row.key) ?? typical.get(row.kind) ?? rowEstimate(row.kind)));
    let next = visibleRange(ix, top, viewport, viewport * 1.5);
    if (next.end - next.start > 80) {
      const focus = ix.indexAt(top + viewport / 2);
      const start = Math.max(0, Math.min(focus - 40, list.length - 80));
      const end = Math.min(list.length, start + 80);
      next = { start, end, padTop: ix.offset(start), padBottom: Math.max(0, ix.total() - ix.offset(end)) };
    }
    if (range.start === next.start && range.end === next.end && range.padTop === next.padTop && range.padBottom === next.padBottom) return;
    range = next;
  }

  function hold(): void {
    held = host && !controller?.pinned ? viewAnchor(host) : null;
  }

  function applyHeight(key: string, height: number): void {
    if (measuring) {
      queued.push({ key, height });
      return;
    }
    const prev = known.get(key);
    if (prev !== undefined && Math.abs(prev - height) < 1) return;
    measuring = true;
    try {
      const list = rows;
      const index = list.findIndex((row) => row.key === key);
      if (index < 0 || !host || !controller) {
        known.set(key, height);
        return;
      }
      const baseline = prev ?? typical.get(list[index]!.kind) ?? rowEstimate(list[index]!.kind);
      typical.set(list[index]!.kind, height);
      const delta = height - baseline;
      known.set(key, height);
      ix.set(index, height);
      layout(list, host.scrollTop, host.clientHeight || 1);
      if (Math.abs(delta) >= 1 && controller.pinned) controller.onContentChange('resize');
    } finally {
      measuring = false;
      const pending = queued;
      queued = [];
      for (const item of pending) applyHeight(item.key, item.height);
      settle();
    }
  }

  function watch(): ResizeObserver {
    if (observer) return observer;
    observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const node = entry.target as HTMLElement;
        if (node === host) {
          if (host) layout(rows, host.scrollTop, host.clientHeight || 1);
          controller?.onContentChange('resize');
          continue;
        }
        const key = node.dataset.key;
        if (key) applyHeight(key, node.getBoundingClientRect().height);
      }
    });
    return observer;
  }

  function measure(node: HTMLElement): { destroy: () => void } {
    const current = watch();
    current.observe(node);
    const key = node.dataset.key ?? '';
    const height = node.getBoundingClientRect().height;
    if (key && height) applyHeight(key, height);
    return { destroy: () => current.unobserve(node) };
  }

  function sentinel(node: HTMLElement): { destroy: () => void } {
    const root = node.parentElement;
    const io = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting) || !host) return;
      const overflow = host.scrollHeight - host.clientHeight;
      if (controller?.pinned && overflow > PIN_PX) return;
      void pullOlder();
    }, { root });
    io.observe(node);
    return { destroy: () => io.disconnect() };
  }

  async function pullOlder(): Promise<void> {
    if (loading || !chat.timeline.hasOlder) return;
    loading = true;
    try { await chat.loadOlder(); }
    finally { loading = false; }
  }

  async function loadBlock(blockId: string, offset: number): Promise<BlockPage> {
    const pages = await chat.blockContent(blockId);
    let last: BlockPage | null = null;
    for await (const page of pages) {
      last = page;
      if (page.offset >= offset) return page;
    }
    if (!last) throw new Error('not_found');
    return last;
  }

  $effect(() => {
    const next = failed ? 'error' : streaming ? 'streaming' : phase === 'streaming' ? 'done' : phase;
    if (next === phase) return;
    phase = next;
    if (next === 'streaming') announce('Агент отвечает');
    else if (next === 'done') announce('Ответ завершён');
    else if (next === 'error') announce('Ошибка в ответе');
  });

  $effect(() => {
    void chat.deltaSerial;
    const marks = chat.deltaMarks.splice(0);
    if (marks.length === 0 || !probe.enabled) return;
    // Эффект идёт в кадре применения пакета, и текст рисуется в этом же кадре: следующий кадр наступает уже после отрисовки.
    requestAnimationFrame(() => {
      for (const at of marks) {
        const from = Date.parse(at);
        if (Number.isFinite(from)) {
          probe.record('delta_paint_ms', clock.serverNow() - from);
          probe.record('delta_paint_raw_ms', Date.now() - from);
        }
      }
    });
  });

  function contentKey(rowsKeys: string[]): string | undefined {
    return rowsKeys.find((key) => !key.startsWith('day:'));
  }

  function rowNode(el: HTMLElement, key: string): HTMLElement | undefined {
    return [...el.querySelectorAll<HTMLElement>('[data-key]')].find((item) => item.dataset.key === key);
  }

  async function settle(): Promise<void> {
    if (!held || !host || !controller || settling) return;
    settling = true;
    try {
      for (let pass = 0; pass < 8; pass++) {
        // Высоты и распорки должны попасть в DOM до следующего измерения якоря.
        await tick();
        if (!held || !host || !controller || controller.pinned) return;
        const node = rowNode(host, held.key);
        if (!node) {
          // Строка ушла из своей карточки: окно держится за то, что видно теперь.
          if (rowNode(host, held.row)) hold();
          return;
        }
        const drift = node.getBoundingClientRect().top - host.getBoundingClientRect().top - held.top;
        if (Math.abs(drift) < 1) return;
        scrollTo(host.scrollTop + drift);
        layout(rows, host.scrollTop, host.clientHeight || 1);
      }
    } finally {
      settling = false;
    }
  }

  function scrollTo(top: number): void {
    if (!controller) return;
    ownedTop = top;
    controller.ownScroll(top);
  }

  // До отрисовки: окно считается по индексам, и старшая страница иначе на такт сняла бы с экрана строку, которую читают.
  $effect.pre(() => {
    const list = rows;
    const el = host;
    if (!el || !controller) return;
    untrack(() => {
      const keys = list.map((row) => row.key);
      const prepended = previous.length > 0 && keys.length > previous.length && contentKey(keys) !== contentKey(previous) && keys[keys.length - 1] === previous[previous.length - 1];
      const tail = tailKey(list);
      const grew = previous.length > 0 && tail !== previousTail;
      const anchor = prepended && held ? captureAnchor(ix, previous, el.scrollTop) : null;
      previous = keys;
      previousTail = tail;
      layout(list, el.scrollTop, el.clientHeight || 1);
      const target = anchor ? restoreAnchor(ix, keys, anchor) : null;
      if (target !== null) layout(list, target, el.clientHeight || 1);
      void tick().then(() => {
        if (target !== null) scrollTo(target);
        settle();
      });
      requestAnimationFrame(() => {
        if (grew) controller?.onContentChange('tail-growth');
        if (!anchor) return;
        const nudge = (): void => {
          if (!held) return;
          if (!rowNode(el, held.row)) scrollTo(restoreAnchor(ix, keys, anchor));
          settle();
        };
        const retry = (left: number): void => {
          nudge();
          if (left > 0) requestAnimationFrame(() => retry(left - 1));
        };
        retry(12);
      });
    });
  });

  onMount(() => {
    if (!host) return;
    const current = watch();
    current.observe(host);
    const scroller = host;
    controller = new ScrollController(scroller);
    layout(rows, scroller.scrollTop, scroller.clientHeight || 1);
    controller.jumpToBottom();
    const onScroll = () => {
      if (!host) return;
      const top = host.scrollTop;
      if (ownedTop !== null && Math.abs(top - ownedTop) <= 1) ownedTop = null;
      else hold();
      layout(rows, top, host.clientHeight || 1);
      settle();
      const overflow = host.scrollHeight - host.clientHeight;
      if (host.scrollTop < PIN_PX && !(controller?.pinned && overflow > PIN_PX)) void pullOlder();
    };
    scroller.addEventListener('scroll', onScroll, { passive: true });
    return () => {
      scroller.removeEventListener('scroll', onScroll);
      controller?.destroy();
      observer?.disconnect();
    };
  });
</script>

<div class="feed-wrap">
  <div
    class="feed"
    bind:this={host}
    role="log"
    aria-live="off"
    aria-busy={streaming}
    data-loaded={chat.timeline.entries.length}
    data-pinned={controller?.pinned ? '' : undefined}
    data-rows={windowRows.length}
    data-renders="0"
  >
    <div class="sentinel" use:sentinel></div>
    <div class="pad" style:height={`${range.padTop}px`}></div>
    {#each windowRows as row (row.key)}
      <div class="row" data-row data-kind={row.kind} data-key={row.key} use:measure>
        {#if row.kind === 'day'}
          <p class="day">{row.label}</p>
        {:else if row.kind === 'input' && row.entries[0]?.kind === 'input'}
          <UserBubble input={row.entries[0].input} {chat} agentWorking={streaming && row.key === lastInput} />
        {:else if row.kind === 'request' && row.entries[0]?.kind === 'request'}
          <RequestCard request={row.entries[0].request} onDecision={async () => undefined} />
        {:else if row.kind === 'answer'}
          <AnswerCard cardKey={row.key} entries={row.entries} {folds} {links} {harness} {activeRun} conversationId={chat.id} {now} {onInfo} load={loadBlock} />
        {/if}
      </div>
    {/each}
    <div class="pad" style:height={`${range.padBottom}px`}></div>
    {#if rows.length === 0 && !navigator.onLine}<p class="day">История недоступна без связи</p>{/if}
  </div>
  {#if chat.timeline.tailDetached}
    <button type="button" class="jump" onclick={() => chat.reloadTail()}>К последним сообщениям</button>
  {:else if controller?.showJump}
    <button type="button" class="jump" onclick={() => controller?.jumpToBottom()}>К новым сообщениям</button>
  {/if}
</div>

<style>
  .feed-wrap { position: relative; flex: 1; min-height: 0; display: flex; container-type: inline-size; }
  .feed { overflow: auto; overflow-anchor: none; scrollbar-gutter: stable; flex: 1; min-width: 0; padding-inline: max(var(--gutter), calc((100% - var(--reading-max)) / 2 + var(--gutter))); padding-bottom: var(--s4); }
  .feed :global(*) { overflow-anchor: none; }
  .row { padding-top: var(--s3); }
  .row[data-kind='input'] { padding-top: var(--s8); }
  .row:first-of-type { padding-top: var(--s0); }
  .day { margin: 0; padding-block: var(--s4) var(--s0); color: var(--muted); font-size: var(--fs-sm); text-align: center; }
  .jump { position: absolute; bottom: var(--s4); left: 50%; translate: -50%; min-height: var(--target); padding-inline: var(--s8); border-radius: var(--radius-pill); background: var(--accent); color: var(--accent-ink); font-size: var(--fs-meta); white-space: nowrap; }
</style>
