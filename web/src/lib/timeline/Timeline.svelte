<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import { announce } from '#lib/a11y/announce.js';
  import { clock } from '#lib/clock.js';
  import { probe } from '#lib/perf/probe.js';
  import type { BlockPage } from '#lib/stores/blocks.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import AnswerCard from '#lib/timeline/AnswerCard.svelte';
  import UserBubble from '#lib/timeline/UserBubble.svelte';
  import { buildRows, type Row } from '#lib/timeline/rows.js';
  import { ScrollController } from '#lib/timeline/scrollController.js';
  import {
    HeightIndex,
    captureAnchor,
    restoreAnchor,
    rowEstimate,
    visibleRange
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
  let lock: { key: string; top: number; until: number } | null = null;
  let ownedTop: number | null = null;
  let seen = 0;
  let settling = false;
  let measuring = false;
  let queued: { key: string; height: number; node?: HTMLElement }[] = [];
  const PIN_PX = 48;

  const rows = $derived(buildRows(chat.timeline.entries, { now, laneRoot }));
  const windowRows = $derived(rows.slice(range.start, range.end));
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
  const failed = $derived(chat.timeline.entries.some((entry) => entry.kind === 'block' && (entry.block.kind === 'error' || entry.block.state === 'incomplete')));

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

  function applyHeight(key: string, height: number, node?: HTMLElement): void {
    if (measuring) {
      queued.push({ key, height, node });
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
      if (Math.abs(delta) < 1) return;
      if (controller.pinned) controller.onContentChange('resize');
      else if (node && node.getBoundingClientRect().top < host.getBoundingClientRect().top - 1) scrollTo(host.scrollTop + delta);
    } finally {
      measuring = false;
      const pending = queued;
      queued = [];
      for (const item of pending) applyHeight(item.key, item.height, item.node);
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
        if (key) applyHeight(key, node.getBoundingClientRect().height, node);
      }
    });
    return observer;
  }

  function measure(node: HTMLElement): { destroy: () => void } {
    const current = watch();
    current.observe(node);
    const key = node.dataset.key ?? '';
    const height = node.getBoundingClientRect().height;
    if (key && height) applyHeight(key, height, node);
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
    if (marks.length === 0) return;
    requestAnimationFrame(() => requestAnimationFrame(() => {
      for (const at of marks) {
        const from = Date.parse(at);
        if (Number.isFinite(from)) probe.record('delta_paint_ms', clock.serverNow() - from);
      }
    }));
  });

  function contentKey(rowsKeys: string[]): string | undefined {
    return rowsKeys.find((key) => !key.startsWith('day:'));
  }

  function rowNode(el: HTMLElement, key: string): HTMLElement | undefined {
    return [...el.querySelectorAll<HTMLElement>('[data-key]')].find((item) => item.dataset.key === key);
  }

  $effect.pre(() => {
    const list = rows;
    const el = host;
    if (!el || !controller || controller.pinned) return;
    const keys = list.map((row) => row.key);
    const prepended = previous.length > 0 && keys.length > previous.length && contentKey(keys) !== contentKey(previous) && keys[keys.length - 1] === previous[previous.length - 1];
    if (!prepended) return;
    const anchor = captureAnchor(ix, previous, el.scrollTop);
    const node = anchor ? rowNode(el, anchor.key) : undefined;
    if (!anchor || !node) return;
    lock = { key: anchor.key, top: node.getBoundingClientRect().top - el.getBoundingClientRect().top, until: performance.now() + 1500 };
  });

  async function settle(): Promise<void> {
    if (!lock || !host || !controller || settling) return;
    if (performance.now() > lock.until) {
      lock = null;
      return;
    }
    settling = true;
    try {
      for (let pass = 0; pass < 8; pass++) {
        // Высоты и распорки должны попасть в DOM до следующего измерения якоря.
        await tick();
        if (!lock || !host || !controller) return;
        const node = rowNode(host, lock.key);
        if (!node) return;
        const drift = node.getBoundingClientRect().top - host.getBoundingClientRect().top - lock.top;
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

  $effect(() => {
    const list = rows;
    const el = host;
    const keys = list.map((row) => row.key);
    if (!el || !controller) return;
    const prepended = previous.length > 0 && keys.length > previous.length && contentKey(keys) !== contentKey(previous) && keys[keys.length - 1] === previous[previous.length - 1];
    const grew = previous.length > 0 && keys[keys.length - 1] !== previous[previous.length - 1];
    const anchor = prepended ? captureAnchor(ix, previous, el.scrollTop) : null;
    untrack(() => layout(list, el.scrollTop, el.clientHeight || 1));
    previous = keys;
    requestAnimationFrame(() => {
      if (!controller) return;
      const nudge = (): void => {
        if (!controller || !anchor || !lock) return;
        const node = rowNode(el, anchor.key);
        if (!node) scrollTo(restoreAnchor(ix, keys, anchor));
        settle();
      };
      if (lock) nudge();
      else if (grew) controller.onContentChange('tail-growth');
      const retry = (left: number): void => {
        requestAnimationFrame(() => {
          nudge();
          if (lock && left > 0) retry(left - 1);
        });
      };
      retry(12);
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
      const own = ownedTop !== null && Math.abs(top - ownedTop) <= 1;
      const jumped = Math.abs(top - seen) > 400;
      seen = top;
      if (own) ownedTop = null;
      else if (lock && jumped) lock = null;
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
      <div class="row" data-row data-key={row.key} use:measure>
        {#if row.kind === 'day'}
          <p class="day">{row.label}</p>
        {:else if row.kind === 'input' && row.entries[0]?.kind === 'input'}
          <UserBubble input={row.entries[0].input} {chat} agentWorking={streaming} />
        {:else if row.kind === 'answer'}
          <AnswerCard entries={row.entries} all={chat.timeline.entries} conversationId={chat.id} {now} {onInfo} load={loadBlock} />
        {/if}
      </div>
    {/each}
    <div class="pad" style:height={`${range.padBottom}px`}></div>
  </div>
  {#if chat.timeline.tailDetached}
    <button type="button" class="jump" onclick={() => chat.reloadTail()}>К последним сообщениям</button>
  {:else if controller?.showJump}
    <button type="button" class="jump" onclick={() => controller?.jumpToBottom()}>К новым сообщениям</button>
  {/if}
</div>

<style>
  .feed-wrap { position: relative; flex: 1; min-height: 0; display: flex; }
  .feed { overflow: auto; overflow-anchor: none; scrollbar-gutter: stable; flex: 1; min-width: 0; }
  .feed :global(*) { overflow-anchor: none; }
  .day { margin: 0; }
  .jump { position: absolute; }
</style>
