<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import { announce } from '#lib/a11y/announce.js';
  import RequestCard from '#lib/requests/RequestCard.svelte';
  import { clock } from '#lib/clock.js';
  import { durationMs } from '#lib/motion.js';
  import { probe } from '#lib/perf/probe.js';
  import type { BlockPage } from '#lib/stores/blocks.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import type { TimelineSource } from '#lib/stores/timeline.js';
  import AnswerCard from '#lib/timeline/AnswerCard.svelte';
  import { INTERRUPT_FAILED, INTERRUPT_FAILED_OUTCOME, INTERRUPT_IDLE, type InterruptControl } from '#lib/composer/interrupt.svelte.js';
  import CompactionRow from '#lib/timeline/CompactionRow.svelte';
  import EventEntry from '#lib/timeline/EventEntry.svelte';
  import LiveLine from '#lib/timeline/LiveLine.svelte';
  import UserBubble from '#lib/timeline/UserBubble.svelte';
  import { activityLiveLine } from '#lib/timeline/present.js';
  import { FeedFolds } from '#lib/timeline/folds.js';
  import { dayStart, shownInCard, tailKey, RowsCache, type Row } from '#lib/timeline/rows.js';
  import { rowIndexOfInput } from '#lib/timeline/ownerMessages.js';
  import { turnIsLive } from '#lib/composer/sendRules.js';
  import { ScrollController } from '#lib/timeline/scrollController.js';
  import { feedEvents, listSession, previousCall, setFeedCards } from '#lib/timeline/cards/context.js';
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
    laneRoot,
    feed,
    interrupt = null
  }: {
    chat: ChatStore;
    /** Прерывание идущего хода: кнопка в живой строке хода. Только у ленты разговора. */
    interrupt?: InterruptControl | null;
    onInfo?: (runId: string) => void;
    now?: number;
    laneRoot?: string;
    /** Записи ветки субагента; без него лента показывает окно разговора. */
    feed?: TimelineSource;
  } = $props();
  let host = $state<HTMLDivElement | null>(null);
  let controller = $state<ScrollController | null>(null);
  let range = $state({ start: 0, end: 0, padTop: 0, padBottom: 0 });
  let phase = $state<'idle' | 'streaming' | 'done' | 'error'>('idle');
  let loading = false;
  const ix = new HeightIndex(rowEstimate);
  const known = new SvelteMap<string, number>();
  const typical = new SvelteMap<string, number>();
  const guessed = new SvelteMap<string, number>();
  let observer: ResizeObserver | null = null;
  let previous: string[] = [];
  let previousTail = '';
  // Пока владелец читает, эта строка стоит на месте, что бы ни менялось выше и ниже неё.
  let held: ViewAnchor | null = null;
  let ownedTop: number | null = null;
  let settling = false;
  let measuring = false;
  let queued: { key: string; height: number }[] = [];
  const folds = new FeedFolds(keepInPlace);
  const PIN_PX = 48;
  const OLDER_PAGE = 100;
  let highlightKey = $state<string | null>(null);
  let jumpSerial = 0;
  let highlightTimer: ReturnType<typeof setTimeout> | null = null;
  const JUMP_HOLD_MS = 1300;
  // Во время прыжка окно монтирует только то, что нужно у цели: иначе первый кадр
  // нового окна собирает лишние строки и становится долгой задачей.
  let overscan = $state(1.5);

  const today = $derived(dayStart(now));
  const entries = $derived(feed ? feed.entries : chat.timeline.entries);
  const hasOlder = $derived(feed ? feed.hasOlder : chat.timeline.hasOlder);
  const rowsCache = new RowsCache();
  const view = $derived(
    rowsCache.build(entries, { now: today, laneRoot, background: chat.backgroundFacts })
  );
  const links = $derived(view.links);
  const rows = $derived(view.rows);
  const windowRows = $derived(rows.slice(range.start, range.end));
  const lastInput = $derived(rows.findLast((row) => row.kind === 'input')?.key);
  // Своё сообщение возвращает ленту к хвосту: плашка «К новым сообщениям» нужна только для чужих.
  let seenSends = untrack(() => chat.ownSends);
  $effect(() => {
    const sends = chat.ownSends;
    if (sends === seenSends) return;
    seenSends = sends;
    // Окно далеко от хвоста: к своему сообщению ведёт свежий хвост, а не низ окна.
    if (!feed && untrack(() => chat.timeline.tailDetached)) void jumpToLatest();
    else void tick().then(() => controller?.jumpToBottom());
  });
  // Владелец у самого низа настоящего хвоста видит последние записи: хаб снимает непрочитанное.
  let shown = $state(0);
  $effect(() => {
    void shown;
    if (feed || laneRoot || !controller?.pinned || chat.timeline.tailDetached) return;
    const seq = chat.timeline.newestSeq();
    if (seq) untrack(() => chat.markRead(seq));
  });
  let seenRows: string | null = null;
  $effect(() => {
    // Сборка ключей и метка в DOM нужны только зонду: без него строка не склеивается.
    if (!probe.enabled) return;
    const next = rows.map((row) => row.key).join('\n');
    const node = host;
    if (!node || seenRows === next) return;
    seenRows = next;
    probe.count('timeline_render');
    node.setAttribute('data-renders', String(Number(node.getAttribute('data-renders') ?? '0') + 1));
  });
  const streaming = $derived(entries.some((entry) => entry.kind === 'block' && entry.block.state === 'streaming'));
  const failed = $derived(entries.some((entry) => entry.kind === 'block' && (entry.block.kind === 'error' || (entry.block.state === 'incomplete' && shownInCard(entry.block)))));
  const harness = $derived(chat.detail?.harness as { title?: string; provider?: string } | undefined);
  // Карточки инструментов читают ленту через контекст: свой ли это оркестратор, прошлый список задач, события хаба.
  setFeedCards({
    get root() {
      return !laneRoot && !feed && !chat.detail?.parent_id;
    },
    get harness() {
      return harness?.provider ?? '';
    },
    previous: (block) => previousCall(entries, block),
    session: listSession,
    events: (sessionId) => feedEvents(entries, sessionId)
  });
  const harnessId = $derived((chat.detail?.harness as { id?: string } | undefined)?.id ?? '');
  const permissionMode = $derived(
    (chat.settings.effective as { permission_mode?: string } | null | undefined)?.permission_mode ??
      ((chat.detail?.permission_mode as string | undefined) ?? '')
  );
  // Строка «Ход прерван»: прерывание остановило ход, а не сессию. Видна, пока
  // очередь на паузе `user_stop` и новый ход не идёт: продолжение и следующий
  // ход её снимают. От остановленной сессии отличается живой исполняеммостью.
  // Живая строка закрывает промежуток до строки эпизода: фаза агента или только что принятый запрос.
  const compactingRow = $derived(entries.some((entry) => entry.kind === 'compaction' && entry.compaction.ok === null));
  const compacting = $derived(!feed && !laneRoot && chat.compacting && !compactingRow);
  const turn = $derived(turnIsLive(chat.activity, chat.queue.run_slot));
  // Текущий ход узнаётся по слоту очереди: до первого блока последний блок ещё принадлежит прошлому ходу.
  const runInputId = $derived.by(() => {
    const slot = chat.queue.run_slot;
    if (typeof slot !== 'object' || slot === null) return null;
    if ('running' in slot) return slot.running?.input_id ?? null;
    return slot.dispatching?.input_id ?? null;
  });
  // Ход, начатый самим CLI, места в очереди не занимает: его выдают блоки, созданные после начала активности.
  const turnStart = $derived.by(() => {
    const since = Date.parse(String(chat.activity?.since ?? ''));
    return Number.isFinite(since) ? since : null;
  });
  const activeRun = $derived.by(() => {
    const state = chat.activity?.state;
    if (state !== 'working' && state !== 'waiting_for_user') return null;
    if (runInputId) return runInputId;
    const own = entries.findLast((entry) =>
      entry.kind === 'block' &&
      (entry.block.state === 'streaming' || (turnStart !== null && entry.block.createdAt >= turnStart))
    );
    return own?.kind === 'block' ? own.block.inputCommandId : null;
  });
  // Живой строкой хода владеет карточка ответа, как только у текущего хода есть блок.
  const liveAnswer = $derived.by(() => {
    if (feed || laneRoot) return false;
    return rows.some((row) =>
      row.kind === 'answer' &&
      row.entries.some((entry) =>
        entry.kind === 'block' &&
        (entry.block.state === 'streaming' || (activeRun !== null && entry.block.inputCommandId === activeRun))
      )
    );
  });
  // С начала хода и до первого блока строка стоит в хвосте ленты: «Прервать» есть сразу.
  const earlyLive = $derived.by(() => {
    if (feed || laneRoot || !turn || liveAnswer) return null;
    return activityLiveLine(chat.activity, now);
  });

  const interruptDone = $derived.by(() => {
    if (feed || laneRoot) return false;
    const status = (chat.interrupt as { status?: string } | null)?.status;
    if (status !== 'acknowledged' && status !== 'completed') return false;
    if (turn) return false;
    const pauses = chat.queue.pause_reasons as { reason?: string }[];
    return pauses.some((pause) => pause?.reason === 'user_stop');
  });
  // Итог прерывания живёт там же, где была живая строка, и после хода без ответа.
  const interruptOutcome = $derived.by(() => {
    if (feed || laneRoot || !interrupt || turn || interrupt.running) return null;
    if (interrupt.label === INTERRUPT_IDLE) return null;
    if (interrupt.label === INTERRUPT_FAILED) return INTERRUPT_FAILED_OUTCOME;
    return interrupt.label;
  });
  const abortText = $derived(interruptOutcome ?? (interruptDone ? 'Ход прерван' : null));

  // Оценка непромеренной строки назначается один раз: иначе каждый замер живой карточки сдвигал бы все строки выше экрана и сбивал прокрутку.
  function guess(row: Row): number {
    let height = guessed.get(row.key);
    if (height === undefined) {
      height = typical.get(row.kind) ?? rowEstimate(row.kind);
      guessed.set(row.key, height);
    }
    return height;
  }

  function layout(list: Row[], top: number, viewport: number): void {
    ix.setCount(list.length);
    list.forEach((row, index) => ix.set(index, known.get(row.key) ?? guess(row)));
    // Прижатая лента показывает хвост, где бы ни стоял scrollTop: браузер на миг поджимает его,
    // когда оценки сменяются замерами, и окно, посчитанное от поджатого места, качалось бы
    // между двумя наборами строк, а высота ленты прыгала бы вместе с ним.
    if (controller?.pinned) top = Math.max(0, ix.total() - viewport);
    let next = visibleRange(ix, top, viewport, viewport * overscan);
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

  function keepInPlace(closing: boolean): void {
    controller?.release();
    hold();
    if (!closing) return;
    // Свёрнутое у самого низа возвращает окно к хвосту.
    void tick().then(() => {
      if (!host || host.scrollHeight - host.clientHeight - host.scrollTop > PIN_PX) return;
      held = null;
      controller?.jumpToBottom();
    });
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
      const baseline = prev ?? guess(list[index]!);
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
        if (spacers.has(node)) {
          if (controller?.pinned) controller.onContentChange('resize');
          continue;
        }
        const key = node.dataset.key;
        if (key) applyHeight(key, node.getBoundingClientRect().height);
      }
    });
    return observer;
  }

  // Распорки окна и живые строки под ним меняются не замером строки: без наблюдения прижатая
  // лента не узнала бы, что после последней прокрутки к низу содержимое выросло.
  const spacers = new WeakSet<Element>();
  function spacer(node: HTMLElement): { destroy: () => void } {
    const current = watch();
    spacers.add(node);
    current.observe(node);
    return { destroy: () => current.unobserve(node) };
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
    if (loading || !hasOlder) return;
    loading = true;
    try { await (feed ? feed.loadOlder() : chat.loadOlder()); }
    finally { loading = false; }
  }

  async function pullNewer(): Promise<void> {
    if (loading || feed || !chat.timeline.tailDetached) return;
    loading = true;
    try { await chat.loadNewer(); }
    catch { /* следующая прокрутка вниз попробует снова */ }
    finally { loading = false; }
  }

  async function jumpToLatest(): Promise<void> {
    held = null;
    try { await chat.reloadTail(); }
    catch { return; }
    await tick();
    controller?.jumpToBottom();
  }

  const nextFrame = (): Promise<void> =>
    new Promise((resolve) => requestAnimationFrame(() => resolve()));

  /**
   * Прыжок к своему сообщению: старые блоки не монтируются, пока летим, —
   * лента заменяется окном вокруг цели, а высоты строк доводят её до верха.
   */
  async function jumpTo(request: { inputId: string; seq: string; nonce: number }): Promise<void> {
    const serial = ++jumpSerial;
    overscan = 0.1;
    held = null;
    controller?.release();
    try {
      await chat.focusMessage(request.inputId, request.seq);
    } catch {
      if (serial === jumpSerial) overscan = 1.5;
      return;
    }
    if (serial !== jumpSerial || !host) return;
    // Кадр даёт измерить новые строки до того, как по их высотам считается место.
    await nextFrame();
    if (serial !== jumpSerial || !host) return;
    let target: Row | null = null;
    const viewport = host.clientHeight || 1;
    for (let pass = 0; pass < 8; pass++) {
      if (serial !== jumpSerial || !host) return;
      const index = rowIndexOfInput(rows, request.inputId);
      if (index < 0) {
        await nextFrame();
        continue;
      }
      const row = rows[index]!;
      target = row;
      const node = rowNode(host, row.key);
      if (!node) {
        layout(rows, host.scrollTop, viewport);
        const top = ix.offset(index);
        scrollTo(top);
        layout(rows, top, viewport);
        await nextFrame();
        continue;
      }
      const desired = Number.parseFloat(getComputedStyle(host).paddingTop) || 0;
      const drift = node.getBoundingClientRect().top - host.getBoundingClientRect().top - desired;
      if (Math.abs(drift) <= 2) break;
      scrollTo(host.scrollTop + drift);
      layout(rows, host.scrollTop, viewport);
      await nextFrame();
    }
    if (!target || serial !== jumpSerial) {
      if (serial === jumpSerial) overscan = 1.5;
      return;
    }
    await markJump(target.key);
    // Широкий запас вернётся при следующей прокрутке, а не отдельным рывком сразу после прыжка.
    overscan = 1.5;
  }

  async function markJump(key: string): Promise<void> {
    if (highlightTimer) clearTimeout(highlightTimer);
    highlightKey = null;
    await tick();
    highlightKey = key;
    await tick();
    if (host) rowNode(host, key)?.querySelector<HTMLElement>('.bubble')?.focus({ preventScroll: true });
    // Подсветка живёт, пока гаснет; без движения она стоит ровным тоном столько же, сколько гасла бы.
    highlightTimer = setTimeout(() => {
      highlightKey = null;
      highlightTimer = null;
    }, durationMs('--duration-jump-glow') || JUMP_HOLD_MS);
  }

  $effect(() => {
    // Чтение держит эффект подписанным, а забор обнуляет просьбу: она одноразовая.
    void chat.jumpRequest;
    const request = untrack(() => chat.takeJumpRequest());
    if (!request) return;
    untrack(() => {
      void jumpTo(request);
    });
  });

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
      // Низ оторванного окна не хвост: следовать за ним значило бы перепрыгнуть пришедшую страницу.
      // Первый расклад открытого разговора сюда не входит: хвост возвращает монтирование.
      if (previous.length > 0 && !feed && chat.timeline.tailDetached && controller?.pinned) {
        controller.release();
        hold();
      }
      // Сверху пришла старшая страница или окно обрезано при подгрузке вниз: читаемая строка стоит на месте.
      const headMoved = previous.length > 0 && contentKey(keys) !== contentKey(previous);
      const tail = tailKey(list);
      const grew = previous.length > 0 && tail !== previousTail;
      const captured = headMoved && held ? captureAnchor(ix, previous, el.scrollTop) : null;
      const anchor = captured && keys.includes(captured.key) ? captured : null;
      previous = keys;
      previousTail = tail;
      layout(list, el.scrollTop, el.clientHeight || 1);
      const target = anchor ? restoreAnchor(ix, keys, anchor) : null;
      if (target !== null) layout(list, target, el.clientHeight || 1);
      void tick().then(() => {
        if (target !== null) scrollTo(target);
        settle();
        // Свёрнутый ход может уместиться в окно: тогда старшую страницу не вызвать прокруткой.
        const room = feed ? feed.hasOlder : !laneRoot && chat.timeline.entries.length + OLDER_PAGE <= chat.timeline.maxLoaded;
        if (room && el.scrollHeight - el.clientHeight <= PIN_PX) void pullOlder();
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
    // Тёплый разговор оставили у якоря или с обрезанным окном: открытие возвращает
    // к последним сообщениям, а не к низу прежнего окна.
    if (!feed && !laneRoot && chat.timeline.tailDetached) void jumpToLatest();
    const onScroll = () => {
      if (!host) return;
      const top = host.scrollTop;
      if (ownedTop !== null && Math.abs(top - ownedTop) <= 1) ownedTop = null;
      else hold();
      layout(rows, top, host.clientHeight || 1);
      settle();
      const overflow = host.scrollHeight - host.clientHeight;
      if (host.scrollTop < PIN_PX && !(controller?.pinned && overflow > PIN_PX)) void pullOlder();
      else if (overflow - host.scrollTop < host.clientHeight) void pullNewer();
    };
    scroller.addEventListener('scroll', onScroll, { passive: true });
    const onVisible = () => { if (document.visibilityState === 'visible') shown += 1; };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      document.removeEventListener('visibilitychange', onVisible);
      scroller.removeEventListener('scroll', onScroll);
      controller?.destroy();
      observer?.disconnect();
      if (highlightTimer) clearTimeout(highlightTimer);
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
    data-loaded={entries.length}
    data-pinned={controller?.pinned ? '' : undefined}
    data-rows={windowRows.length}
    data-renders="0"
  >
    <div class="sentinel" use:sentinel></div>
    <div class="pad" style:height={`${range.padTop}px`} use:spacer></div>
    {#each windowRows as row (row.key)}
      <div class="row" data-row data-kind={row.kind} data-key={row.key} use:measure>
        {#if row.kind === 'day'}
          <p class="day">{row.label}</p>
        {:else if row.kind === 'input' && row.entries[0]?.kind === 'input'}
          <UserBubble input={row.entries[0].input} {chat} agentWorking={streaming && row.key === lastInput} highlight={row.key === highlightKey} />
        {:else if row.kind === 'event' && row.entries[0]?.kind === 'input'}
          <EventEntry input={row.entries[0].input} open={folds.isOpen(row.key)} onToggle={() => folds.toggle(row.key)} />
        {:else if row.kind === 'request' && row.entries[0]?.kind === 'request'}
          <RequestCard request={row.entries[0].request} onDecision={async () => undefined} {harnessId} {permissionMode} open={folds.isOpen(row.key)} onToggle={() => folds.toggle(row.key)} />
        {:else if row.kind === 'answer'}
          <AnswerCard cardKey={row.key} {laneRoot} entries={row.entries} {folds} {links} {harness} {activeRun} conversationId={chat.id} {now} {onInfo} load={loadBlock} interrupt={feed || laneRoot ? null : interrupt} />
        {:else if row.kind === 'compaction' && row.entries[0]?.kind === 'compaction'}
          <CompactionRow compaction={row.entries[0].compaction} />
        {/if}
      </div>
    {/each}
    <div class="pad" style:height={`${range.padBottom}px`} use:spacer></div>
    {#if earlyLive}
      <div class="row" data-row data-kind="live" data-key="live-turn" data-role="live-turn" use:spacer>
        <LiveLine phase={earlyLive.phase} tool={earlyLive.tool} startedAt={earlyLive.startedAt} {now} {interrupt} />
      </div>
    {/if}
    {#if compacting}
      <div class="row" data-row data-kind="compaction" data-key="compaction-live" use:spacer>
        <CompactionRow compaction={{ id: 'live', trigger: 'auto', preTokens: null, ok: null, createdAt: now, finishedAt: null }} />
      </div>
    {/if}
    {#if abortText}
      <div class="row" data-row data-kind="interrupt" data-key="interrupt-outcome" data-role="interrupt-outcome" use:spacer>
        <p class="aborted">{abortText}</p>
      </div>
    {/if}
    {#if rows.length === 0 && !navigator.onLine}<p class="day">История недоступна без связи</p>
    {:else if rows.length === 0 && !feed && !chat.detail}
      <!-- Пока снимок не пришёл, лента показывает заготовку, как шапка, а не пустоту. -->
      <div class="loading" data-feed-loading aria-hidden="true"><span class="skeleton" style:width="var(--bar-74)"></span><span class="skeleton" style:width="var(--bar-56)"></span><span class="skeleton" style:width="var(--bar-81)"></span></div>
    {:else if rows.length === 0 && !feed && !compacting && chat.requests.items.size === 0}
      <p class="empty" data-feed-empty>Сообщений пока нет. Напишите агенту задачу</p>
    {/if}
  </div>
  {#if !feed && chat.timeline.tailDetached}
    <button type="button" class="jump" onclick={() => void jumpToLatest()}>К последним сообщениям</button>
  {:else if controller?.showJump}
    <button type="button" class="jump" onclick={() => controller?.jumpToBottom()}>К новым сообщениям</button>
  {/if}
</div>

<style>
  .loading { display: flex; flex-direction: column; gap: var(--s6); padding: var(--s8) var(--s6); }
  .empty { margin: auto var(--s0); padding: var(--s8) var(--s6); color: var(--muted); font-size: var(--fs-meta); text-align: center; }
  .feed-wrap { position: relative; flex: 1; min-height: 0; display: flex; container-type: inline-size; }
  .feed { overflow: auto; overflow-anchor: none; scrollbar-gutter: stable; flex: 1; min-width: 0; padding-inline: max(var(--gutter), calc((100% - var(--reading-max)) / 2 + var(--gutter))); padding-block: var(--s6); mask-image: linear-gradient(to bottom, var(--transparent), var(--text) var(--s6), var(--text) calc(100% - var(--s6)), var(--transparent)); }
  .feed :global(*) { overflow-anchor: none; }
  .row { padding-top: var(--s3); }
  .row[data-kind='input'] { padding-top: var(--s8); }
  .row:first-of-type { padding-top: var(--s0); }
  .day { margin: 0; padding-block: var(--s4) var(--s0); color: var(--muted); font-size: var(--fs-sm); text-align: center; }
  .aborted { margin: 0; padding-block: var(--s4) var(--s0); color: var(--warning); font-size: var(--fs-sm); text-align: center; }
  .jump { position: absolute; bottom: var(--s4); left: 50%; translate: -50%; min-height: var(--target); padding-inline: var(--s8); border-radius: var(--radius-pill); background: var(--accent); color: var(--accent-ink); font-size: var(--fs-meta); white-space: nowrap; }
</style>
