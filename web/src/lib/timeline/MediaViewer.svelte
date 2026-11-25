<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import Icon from '#lib/ui/Icon.svelte';
  import { dropLayer, pushLayer } from '#lib/ui/layers.js';
  import { durationMs, fadeMs } from '#lib/motion.js';
  import { declaredSize, downloadName, isVideoShot, shotCaption, shotPreviewSrc, shotSrc, videoPlayable, type ViewerShot } from '#lib/viewer/shots.js';
  import { Gallery, galleryId, galleryPosition, mergeGalleryShots, type GalleryAnchor, type GalleryItem } from '#lib/viewer/gallery.js';
  import { gallerySource } from '#lib/viewer/gallery-context.js';
  import {
    clampView,
    coverRect,
    dismissOpacity,
    dismisses,
    fitView,
    isZoomed,
    pinchView,
    rubber,
    rubberView,
    settle,
    stepScale,
    swipeStep,
    toggleZoom,
    transformOf,
    wheelScale,
    zoomAt,
    type Point,
    type Size,
    type View
  } from '#lib/viewer/zoom.js';

  let {
    shots,
    start = 0,
    title,
    heading = 'Картинки ответа',
    anchor = null,
    onclose
  }: {
    shots: ViewerShot[];
    start?: number;
    title: string;
    heading?: string;
    /** Место нажатой картинки в общем списке сессии: без него листаются только `shots`. */
    anchor?: GalleryAnchor | null;
    onclose: () => void;
  } = $props();

  const ROUTE_KEY = 'image-viewer';
  /** Через столько без касаний элементы управления прячутся сами. */
  const CHROME_IDLE_MS = 3000;
  /** Второе касание в этих пределах считается двойным. */
  const DOUBLE_TAP_MS = 300;
  const DOUBLE_TAP_SLOP = 32;
  const captionId = $props.id();

  const source = gallerySource();
  // Список всей сессии грузится вокруг нажатой картинки; пока он в пути, видны кадры сообщения.
  const gallery = untrack(() => (source && anchor ? new Gallery((query) => source.page(query), galleryId(anchor.kind, anchor.id)) : null));

  let currentId = $state(untrack(() => shots[Math.min(Math.max(0, start), shots.length - 1)]?.media_id ?? ''));
  /** Кадр, с которого открыли просмотрщик: подпись карточки относится только к нему. */
  const openedId = untrack(() => currentId);
  /** Страницы хаба; без них лента просмотрщика — только кадры вызвавшей карточки. */
  let galleryShots = $state<ViewerShot[] | null>(null);
  let anchors = $state<Record<string, GalleryItem>>({});
  let total = $state<number | null>(null);
  /** Сколько картинок старше первого кадра списка: номер в разговоре. */
  let offset = $state(0);
  let hasOlder = $state(false);
  let hasNewer = $state(false);
  /** Хаб не ответил: листаем кадры карточки, как раньше. */
  let fallback = $state(false);

  let chrome = $state(true);
  let zoomed = $state(false);
  let gesture = $state<'idle' | 'pan' | 'pinch' | 'swipe' | 'dismiss'>('idle');
  /** Натуральный размер каждой картинки: из описания, миниатюры или загруженного оригинала. */
  let naturals = $state<Record<string, Size>>({});
  /** Оригинал раскодирован и заменил миниатюру. */
  let ready = $state<Record<string, boolean>>({});
  let failed = $state<Record<string, boolean>>({});
  /** Миниатюра из ленты, которую можно показать до оригинала. */
  let previews = $state<Record<string, string>>({});
  /** Показ-миниатюра не загрузился и убран: виден текст загрузки, пока идёт оригинал. */
  let previewGone = $state<Record<string, boolean>>({});
  /** Браузер не играет этот ролик: вместо плеера текст и «Скачать». */
  let unplayable = $state<Record<string, boolean>>({});

  const list = $derived(galleryShots ?? shots);
  const index = $derived.by(() => {
    const found = list.findIndex((entry) => entry.media_id === currentId);
    return found >= 0 ? found : Math.min(Math.max(0, start), Math.max(0, list.length - 1));
  });
  const shot = $derived(list[index] ?? list[0]!);
  /** Ролик на сцене играет плеер браузера: его не приближают и не двигают, листают и закрывают свайпом по полям. */
  const isClip = $derived(isVideoShot(shot));
  const position = $derived(galleryPosition(offset, index));
  const knownTotal = $derived(total ?? (gallery && !fallback ? null : list.length));
  const many = $derived((knownTotal ?? list.length) > 1);
  const currentAnchor = $derived(anchors[shot.media_id] ?? null);
  const src = $derived(shotSrc(shot));
  const caption = $derived(shotCaption(shot, naturals[shot.media_id] ?? declaredSize(shot)));
  // Подпись кадра уже начинается с заголовка файла: второй раз его не печатаем.
  const captioned = $derived(title !== '' && (caption === title || caption.startsWith(`${title} · `)));
  const slides = $derived([index - 1, index, index + 1].filter((i) => i >= 0 && i < list.length));

  let dialog: HTMLDialogElement;
  let stage: HTMLDivElement;
  let backdrop: HTMLDivElement;
  let chromeBox: HTMLDivElement;

  let size: Size = { width: 0, height: 0 };
  let view: View = { scale: 1, x: 0, y: 0 };
  /** Сдвиг ленты при свайпе вбок и кадра при свайпе вниз. */
  let swipeX = 0;
  let drag: Point = { x: 0, y: 0 };
  let opener: HTMLElement | null = null;
  let layer: object | null = null;
  let entry = $state(false);
  let closing = false;
  let settleTimer: ReturnType<typeof setTimeout> | undefined;
  let chromeTimer: ReturnType<typeof setTimeout> | undefined;
  let tapTimer: ReturnType<typeof setTimeout> | undefined;
  let frame = 0;
  let resize: ResizeObserver | null = null;
  let olderLoading = false;
  let newerLoading = false;

  // Указатели сцены: позиция каждого пальца и начало текущего жеста.
  // Счётчик пальцев живёт вне реактивности: он меняется на каждом событии указателя и разметку не трогает.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const pointers = new Map<number, Point>();
  let press: { point: Point; view: View; swipeX: number; time: number; type: string } | null = null;
  let pinch: { from: [Point, Point]; view: View } | null = null;
  let lastTap: { time: number; point: Point } | null = null;
  let samples: { time: number; x: number; y: number }[] = [];

  function naturalOf(entry: ViewerShot | undefined): Size | null {
    if (!entry || isVideoShot(entry)) return null;
    return naturals[entry.media_id] ?? declaredSize(entry);
  }

  /** Размер для жестов: у ролика это вся сцена, чтобы листание и закрытие работали по его полям. */
  function gestureSize(): Size | null {
    return naturalOf(shot) ?? (isClip ? size : null);
  }

  function viewOf(i: number): View {
    const natural = naturalOf(list[i]);
    if (!natural) return { scale: 1, x: 0, y: 0 };
    return i === index ? view : fitView(size, natural);
  }

  function local(event: { clientX: number; clientY: number }): Point {
    const box = stage.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  }

  /** Все преобразования пишутся в одном кадре: только transform и opacity, без перекладки. */
  function paint(): void {
    frame = 0;
    if (!stage) return;
    for (const node of stage.querySelectorAll<HTMLElement>('.slide')) {
      const i = Number(node.dataset.index);
      const offset = (i - index) * (size.width + gap()) + swipeX;
      const own = i === index ? drag : { x: 0, y: 0 };
      node.style.transform = `translate3d(${offset + own.x}px, ${own.y}px, 0)`;
      const target = frameOf(i);
      const natural = naturalOf(list[i]);
      if (target && natural) {
        target.style.width = `${natural.width}px`;
        target.style.height = `${natural.height}px`;
        target.style.transform = transformOf(viewOf(i));
      }
    }
    backdrop.style.opacity = String(dismissOpacity(drag.y, size.height));
  }

  function frameOf(i: number): HTMLElement | null {
    return stage?.querySelector<HTMLElement>(`.slide[data-index="${i}"] .frame`) ?? null;
  }

  function schedule(): void {
    if (!frame) frame = requestAnimationFrame(paint);
  }

  function paintNow(): void {
    if (frame) cancelAnimationFrame(frame);
    paint();
  }

  let gapPx = 0;
  function gap(): number {
    if (!gapPx && stage) gapPx = parseFloat(getComputedStyle(stage).getPropertyValue('--viewer-gap')) || 24;
    return gapPx;
  }

  /** Переход к новому виду плавно: CSS-переход на время токена; без движения сразу. */
  function animateTo(next: View, nextSwipe = 0, nextDrag: Point = { x: 0, y: 0 }): void {
    view = next;
    swipeX = nextSwipe;
    drag = nextDrag;
    zoomed = isZoomed(view, size, naturalOf(shot) ?? size);
    const duration = durationMs('--duration-expand');
    if (duration > 0) {
      stage.dataset.settling = '';
      clearTimeout(settleTimer);
      settleTimer = setTimeout(() => { delete stage.dataset.settling; }, duration + 50);
    }
    paintNow();
  }

  function setView(next: View): void {
    view = next;
    zoomed = isZoomed(view, size, naturalOf(shot) ?? size);
    schedule();
  }

  function stopSettling(): void {
    clearTimeout(settleTimer);
    if (stage) delete stage.dataset.settling;
  }

  function measure(): void {
    const box = stage.getBoundingClientRect();
    size = { width: box.width, height: box.height };
  }

  function refit(): void {
    const natural = naturalOf(shot);
    if (!natural) return;
    view = zoomed ? clampView(view, size, natural) : fitView(size, natural);
    zoomed = isZoomed(view, size, natural);
  }

  // Элементы управления.
  function showChrome(hold = false): void {
    chrome = true;
    clearTimeout(chromeTimer);
    if (!hold) chromeTimer = setTimeout(hideChrome, CHROME_IDLE_MS);
  }

  function hideChrome(): void {
    clearTimeout(chromeTimer);
    // Пока на кнопке фокус или курсор, панель не прячется из-под руки.
    if (chromeBox?.matches(':focus-within, :hover')) {
      chromeTimer = setTimeout(hideChrome, CHROME_IDLE_MS);
      return;
    }
    chrome = false;
  }

  function toggleChrome(): void {
    if (chrome) hideChrome();
    else showChrome();
  }

  // Миниатюры в ленте: откуда въезжает кадр, куда уходит и куда вернуть фокус.
  function thumbOf(entry: ViewerShot | undefined): HTMLImageElement | null {
    const id = entry?.media_id;
    if (!id) return null;
    const all = [...document.querySelectorAll<HTMLImageElement>(`[data-viewer-thumb="${CSS.escape(id)}"]`)];
    const scope = opener?.closest('[data-key]');
    return all.find((node) => opener?.contains(node)) ?? all.find((node) => scope?.contains(node)) ?? all[0] ?? null;
  }

  function visibleRect(node: Element | null): DOMRect | null {
    if (!node) return null;
    const rect = node.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) return null;
    if (rect.bottom < 0 || rect.top > innerHeight || rect.right < 0 || rect.left > innerWidth) return null;
    return rect;
  }

  function collectPreviews(): void {
    const next: Record<string, string> = {};
    const found: Record<string, Size> = {};
    for (const entry of list) {
      const thumb = thumbOf(entry);
      if (thumb?.complete && thumb.naturalWidth > 0) {
        next[entry.media_id] = thumb.currentSrc || thumb.src;
        if (!declaredSize(entry)) found[entry.media_id] = { width: thumb.naturalWidth, height: thumb.naturalHeight };
      }
    }
    previews = next;
    naturals = { ...found, ...naturals };
  }

  /** Страница хаба заменила кадры карточки: место нажатой картинки сохраняется её id. */
  function applyGallery(): void {
    if (!gallery) return;
    anchors = Object.fromEntries(gallery.items.map((item) => [item.media_id ?? item.attachment_id ?? '', item]));
    galleryShots = mergeGalleryShots(gallery.items, shots);
    // Хаб считает картинки сессии; ролики сообщения, вставленные в список, прибавляются к числу.
    total = gallery.total === null ? null : gallery.total + (galleryShots.length - gallery.items.length);
    offset = gallery.offset;
    hasOlder = gallery.before !== null;
    hasNewer = gallery.after !== null;
    collectPreviews();
    // Кадры и их места приходят в разметку следующим тиком: тогда и пересчитываем ленту.
    void tick().then(() => {
      if (!closing) paintNow();
    });
  }

  async function moreOlder(): Promise<number> {
    if (!gallery || olderLoading || !hasOlder) return 0;
    olderLoading = true;
    try {
      const added = await gallery.older();
      applyGallery();
      return added;
    } catch {
      fallback = true;
      return 0;
    } finally {
      olderLoading = false;
    }
  }

  async function moreNewer(): Promise<number> {
    if (!gallery || newerLoading || !hasNewer) return 0;
    newerLoading = true;
    try {
      const added = await gallery.newer();
      applyGallery();
      return added;
    } catch {
      fallback = true;
      return 0;
    } finally {
      newerLoading = false;
    }
  }

  // Близко к краю — догружаем соседнюю страницу, не дожидаясь упора в последнюю картинку.
  $effect(() => {
    const at = index;
    const count = list.length;
    const older = hasOlder;
    const newer = hasNewer;
    if (!gallery || (!older && !newer)) return;
    untrack(() => {
      if (older && at <= 2) void moreOlder();
      if (newer && count - 1 - at <= 2) void moreNewer();
    });
  });

  /** Браузер, который не знает тип ролика, плеера не получает: сразу текст и «Скачать». */
  function probe(node: HTMLVideoElement, entry: ViewerShot): void {
    const type = entry.mime?.startsWith('video/') ? entry.mime : undefined;
    if (!videoPlayable(type, (name) => node.canPlayType(name))) unplayable = { ...unplayable, [entry.media_id]: true };
  }

  // Ролик, с которого ушли листанием, замолкает: на сцене играет только текущий.
  $effect(() => {
    const at = index;
    untrack(() => {
      for (const node of stage?.querySelectorAll<HTMLVideoElement>('video') ?? []) {
        if (node.closest<HTMLElement>('.slide')?.dataset.index !== String(at)) node.pause();
      }
    });
  });

  function easeToken(): string {
    return getComputedStyle(document.documentElement).getPropertyValue('--ease-viewer-open').trim() || 'ease-out';
  }

  /** Въезд: кадр летит из миниатюры в вписанный вид без обрезки слоя: только transform, без clip-path. */
  function enter(): void {
    const natural = naturalOf(shot);
    const target = frameOf(index);
    const fade = fadeMs('--duration-viewer-fade');
    const duration = durationMs('--duration-viewer-open');
    const thumb = thumbOf(shot);
    const rect = visibleRect(thumb);
    const stageBox = stage.getBoundingClientRect();
    if (duration > 0 && natural && target && rect) {
      const { view: from } = coverRect({ x: rect.left - stageBox.left, y: rect.top - stageBox.top, width: rect.width, height: rect.height }, natural);
      target.animate(
        [{ transform: transformOf(from) }, { transform: transformOf(view) }],
        { duration: durationMs('--duration-viewer-open'), easing: easeToken() }
      );
      backdrop.animate([{ opacity: 0 }, { opacity: 1 }], { duration: durationMs('--duration-viewer-open'), easing: easeToken() });
      chromeBox.animate([{ opacity: 0 }, { opacity: 1 }], { duration: fadeMs('--duration-viewer-fade') });
      return;
    }
    if (fade > 0) dialog.animate([{ opacity: 0 }, { opacity: 1 }], { duration: fadeMs('--duration-viewer-fade') });
  }

  /** Уход: кадр возвращается на миниатюру без обрезки слоя: только transform, без clip-path. */
  async function leave(): Promise<void> {
    const natural = naturalOf(shot);
    const target = frameOf(index);
    const duration = durationMs('--duration-viewer-open');
    const fade = fadeMs('--duration-viewer-fade');
    const thumb = thumbOf(shot);
    const rect = visibleRect(thumb);
    const stageBox = stage.getBoundingClientRect();
    const opacity = Number(backdrop.style.opacity || 1);
    if (duration > 0 && natural && target && rect) {
      // Текущий сдвиг свайпа вниз переносится в вид кадра, чтобы уход начался с того места, где палец его отпустил.
      const current: View = { scale: view.scale, x: view.x + drag.x, y: view.y + drag.y };
      drag = { x: 0, y: 0 };
      swipeX = 0;
      stopSettling();
      paintNow();
      target.style.transform = transformOf(current);
      const { view: to } = coverRect({ x: rect.left - stageBox.left, y: rect.top - stageBox.top, width: rect.width, height: rect.height }, natural);
      chrome = false;
      const motion = target.animate(
        [{ transform: transformOf(current) }, { transform: transformOf(to) }],
        { duration: durationMs('--duration-viewer-open'), easing: easeToken(), fill: 'forwards' }
      );
      backdrop.animate([{ opacity }, { opacity: 0 }], { duration: durationMs('--duration-viewer-open'), easing: easeToken(), fill: 'forwards' });
      await motion.finished.catch(() => undefined);
      return;
    }
    if (fade > 0) await dialog.animate([{ opacity: 1 }, { opacity: 0 }], { duration: fadeMs('--duration-viewer-fade'), fill: 'forwards' }).finished.catch(() => undefined);
  }

  function restoreFocus(): void {
    const thumb = thumbOf(shot)?.closest<HTMLElement>('button, a');
    (thumb ?? opener)?.focus({ preventScroll: true });
  }

  async function close(fromHistory = false): Promise<void> {
    if (closing) return;
    closing = true;
    clearTimeout(chromeTimer);
    clearTimeout(tapTimer);
    await leave();
    if (dialog.open) dialog.close();
    dropLayer(layer);
    restoreFocus();
    if (!fromHistory && entry && page.state.sheet === ROUTE_KEY) history.back();
    entry = false;
    onclose();
  }

  /** Закрыть просмотрщик и показать место картинки в чате. */
  async function reveal(): Promise<void> {
    const item = currentAnchor;
    if (!item || !source) return;
    await close();
    source.reveal(item);
  }

  async function go(next: number, fromSwipe = false): Promise<void> {
    if (next < 0) {
      const added = await moreOlder();
      if (added > 0) next += added;
      else {
        if (fromSwipe) animateTo(view);
        return;
      }
    } else if (next >= list.length) {
      // Догруженная страница не перескакивает свои кадры: листаем на первый из новых.
      const count = list.length;
      const added = await moreNewer();
      if (added > 0) next = count;
      else {
        if (fromSwipe) animateTo(view);
        return;
      }
    }
    if (next < 0 || next >= list.length || list[next]?.media_id === currentId) {
      if (fromSwipe) animateTo(view);
      return;
    }
    const step = next - index;
    // Лента остаётся на месте: новый индекс сдвигает её на ширину кадра, а остаток уезжает переходом.
    const carried = swipeX + step * (size.width + gap());
    currentId = list[next]!.media_id;
    view = fitView(size, naturalOf(list[next]) ?? size);
    zoomed = false;
    swipeX = carried;
    await tick();
    paintNow();
    if (fromSwipe || durationMs('--duration-expand') > 0) {
      // Кадр с прежним сдвигом уже на экране: переход начинается с него.
      void stage.offsetWidth;
      animateTo(view, 0);
    } else {
      swipeX = 0;
      paintNow();
    }
  }

  // Жесты.
  function velocity(): { x: number; y: number } {
    const last = samples.at(-1);
    const first = samples.find((sample) => last && last.time - sample.time <= 100) ?? samples[0];
    if (!first || !last || last.time === first.time) return { x: 0, y: 0 };
    return { x: (last.x - first.x) / (last.time - first.time), y: (last.y - first.y) / (last.time - first.time) };
  }

  function record(point: Point): void {
    const time = performance.now();
    samples.push({ time, x: point.x, y: point.y });
    while (samples.length > 2 && time - samples[0]!.time > 160) samples.shift();
  }

  function pointerDown(event: PointerEvent): void {
    if (closing) return;
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    // Плеер и его кнопки ведут нажатие сами: захват указателя сценой лишил бы их кликов.
    if (event.target instanceof Element && event.target.closest('video, a, button')) return;
    stage.setPointerCapture?.(event.pointerId);
    const point = local(event);
    pointers.set(event.pointerId, point);
    if (pointers.size === 1) {
      stopSettling();
      press = { point, view, swipeX, time: performance.now(), type: event.pointerType };
      samples = [];
      record(point);
    } else if (pointers.size === 2 && !isClip) {
      // Второй палец превращает любой начатый жест в щипок: начатый свайп откатывается.
      clearTimeout(tapTimer);
      const [a, b] = [...pointers.values()] as [Point, Point];
      if (gesture === 'swipe' || gesture === 'dismiss') {
        swipeX = 0;
        drag = { x: 0, y: 0 };
      }
      pinch = { from: [a, b], view };
      gesture = 'pinch';
      press = null;
      hideChrome();
    }
  }

  function pointerMove(event: PointerEvent): void {
    if (event.pointerType === 'mouse' && pointers.size === 0) {
      // Движение мыши показывает спрятанные кнопки, как в видеоплеерах.
      if (!chrome) showChrome();
      return;
    }
    if (!pointers.has(event.pointerId) || closing) return;
    const point = local(event);
    pointers.set(event.pointerId, point);
    const natural = gestureSize();
    if (!natural) return;
    if (gesture === 'pinch' && pinch && pointers.size >= 2) {
      const [a, b] = [...pointers.values()] as [Point, Point];
      setView(pinchView(pinch.view, pinch.from, [a, b], size, natural));
      return;
    }
    if (!press) return;
    record(point);
    const dx = point.x - press.point.x;
    const dy = point.y - press.point.y;
    if (gesture === 'idle') {
      const slop = press.type === 'mouse' ? 4 : 8;
      if (Math.hypot(dx, dy) < slop) return;
      clearTimeout(tapTimer);
      lastTap = null;
      if (isZoomed(view, size, natural)) gesture = 'pan';
      else if (many && Math.abs(dx) > Math.abs(dy)) gesture = 'swipe';
      else if (Math.abs(dy) >= Math.abs(dx)) gesture = 'dismiss';
      else if (isClip) return;
      else gesture = 'pan';
      if (gesture !== 'swipe') hideChrome();
    }
    if (gesture === 'pan') {
      setView(rubberView({ scale: press.view.scale, x: press.view.x + dx, y: press.view.y + dy }, size, natural));
    } else if (gesture === 'swipe') {
      // У края списка лента тянется с сопротивлением, только если дальше ничего нет.
      const min = index === list.length - 1 && !hasNewer ? 0 : -Infinity;
      const max = index === 0 && !hasOlder ? 0 : Infinity;
      swipeX = rubber(press.swipeX + dx, min, max, size.width);
      schedule();
    } else if (gesture === 'dismiss') {
      drag = { x: dx * 0.5, y: dy };
      schedule();
    }
  }

  function tap(point: Point): void {
    const now = performance.now();
    if (lastTap && now - lastTap.time < DOUBLE_TAP_MS && Math.hypot(point.x - lastTap.point.x, point.y - lastTap.point.y) < DOUBLE_TAP_SLOP) {
      clearTimeout(tapTimer);
      lastTap = null;
      const natural = naturalOf(shot);
      if (natural) animateTo(toggleZoom(view, point, size, natural));
      return;
    }
    lastTap = { time: now, point };
    clearTimeout(tapTimer);
    tapTimer = setTimeout(() => {
      lastTap = null;
      toggleChrome();
    }, DOUBLE_TAP_MS);
  }

  function pointerUp(event: PointerEvent): void {
    if (!pointers.has(event.pointerId)) return;
    const point = local(event);
    pointers.delete(event.pointerId);
    const natural = gestureSize();
    const cancelled = event.type === 'pointercancel';
    if (gesture === 'pinch') {
      if (pointers.size === 1 && natural) {
        // Оставшийся палец продолжает двигать картинку с того вида, что получился.
        const [rest] = [...pointers.values()] as [Point];
        press = { point: rest, view, swipeX: 0, time: performance.now(), type: event.pointerType };
        samples = [];
        gesture = 'pan';
        return;
      }
      if (pointers.size === 0) {
        gesture = 'idle';
        pinch = null;
        if (natural) animateTo(settle(view, size, natural, point));
      }
      return;
    }
    if (pointers.size > 0) return;
    const was = gesture;
    gesture = 'idle';
    const start = press;
    press = null;
    if (!natural || !start) return;
    const speed = velocity();
    if (was === 'idle') {
      if (!cancelled) tap(point);
      return;
    }
    if (was === 'pan') {
      // Короткий бросок докатывает картинку, затем она оседает в границах.
      const glide = cancelled ? 0 : 140;
      animateTo(settle({ scale: view.scale, x: view.x + speed.x * glide, y: view.y + speed.y * glide }, size, natural));
      return;
    }
    if (was === 'swipe') {
      const step = cancelled ? 0 : swipeStep(swipeX, speed.x, size.width);
      void go(index + step, true);
      return;
    }
    if (was === 'dismiss') {
      if (!cancelled && dismisses(drag.y, speed.y, size.height)) void close();
      else animateTo(view);
    }
  }

  function wheel(event: WheelEvent): void {
    const natural = naturalOf(shot);
    if (!natural || closing) return;
    event.preventDefault();
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? size.height : 1;
    // Двумя пальцами по сенсорной панели без ctrl по приближенной картинке — это сдвиг, а не масштаб.
    if (!event.ctrlKey && event.deltaMode === 0 && zoomed && Math.abs(event.deltaX) > Math.abs(event.deltaY)) {
      stopSettling();
      setView(clampView({ scale: view.scale, x: view.x - event.deltaX, y: view.y - event.deltaY }, size, natural));
      return;
    }
    stopSettling();
    const at = local(event);
    const scale = wheelScale(view, event.deltaY * unit, event.ctrlKey, size, natural);
    const next = clampView(zoomAt(view, at, scale), size, natural);
    setView(isZoomed(next, size, natural) ? next : fitView(size, natural));
  }

  function zoomBy(direction: 1 | -1 | 0): void {
    const natural = naturalOf(shot);
    if (!natural) return;
    if (direction === 0) return animateTo(fitView(size, natural));
    const centre = { x: size.width / 2, y: size.height / 2 };
    const next = clampView(zoomAt(view, centre, stepScale(view, direction, size, natural)), size, natural);
    animateTo(isZoomed(next, size, natural) ? next : fitView(size, natural));
  }

  /** Крайняя кнопка листания остаётся в порядке табуляции и объясняет себя подписью, как IconButton. */
  function edge(off: boolean): { 'aria-disabled'?: 'true' } {
    return off ? { 'aria-disabled': 'true' } : {};
  }

  function focusables(): HTMLElement[] {
    return [...dialog.querySelectorAll<HTMLElement>('button, a[href]')].filter((node) => node.getClientRects().length > 0);
  }

  function keydown(event: KeyboardEvent): void {
    if (closing) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      void close();
      return;
    }
    if (event.key === 'Tab') {
      showChrome();
      const items = focusables();
      const first = items[0];
      const last = items.at(-1);
      const inside = items.includes(document.activeElement as HTMLElement);
      if (event.shiftKey && (!inside || document.activeElement === first)) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && (!inside || document.activeElement === last)) {
        event.preventDefault();
        first?.focus();
      }
      return;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    // Стрелки, пробел и цифры на плеере остаются ему: перемотка, пауза, громкость.
    if (event.target instanceof HTMLVideoElement) return;
    const natural = naturalOf(shot);
    if (event.key === '+' || event.key === '=') zoomBy(1);
    else if (event.key === '-' || event.key === '_') zoomBy(-1);
    else if (event.key === '0') zoomBy(0);
    else if (event.key.startsWith('Arrow') && zoomed && natural) {
      // Приближенную картинку стрелки сдвигают; вписанную листают.
      const stepX = event.key === 'ArrowLeft' ? 80 : event.key === 'ArrowRight' ? -80 : 0;
      const stepY = event.key === 'ArrowUp' ? 80 : event.key === 'ArrowDown' ? -80 : 0;
      animateTo(clampView({ scale: view.scale, x: view.x + stepX, y: view.y + stepY }, size, natural));
    } else if (event.key === 'ArrowLeft') void go(index - 1);
    else if (event.key === 'ArrowRight') void go(index + 1);
    else return;
    event.preventDefault();
  }

  // Загрузка: миниатюра видна сразу, оригинал заменяет её, когда раскодирован, на том же кадре без скачка.
  function loaded(id: string, image: HTMLImageElement): void {
    const natural = { width: image.naturalWidth, height: image.naturalHeight };
    const known = naturalOf(list.find((entry) => entry.media_id === id));
    if (!known || known.width !== natural.width || known.height !== natural.height) {
      naturals = { ...naturals, [id]: natural };
      if (id === currentId) {
        view = fitView(size, natural);
        zoomed = false;
      }
    }
    const reveal = () => {
      if (closing) return;
      ready = { ...ready, [id]: true };
      paintNow();
    };
    if (typeof image.decode === 'function') image.decode().then(reveal, reveal);
    else reveal();
    schedule();
  }

  onMount(() => {
    opener = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : thumbOf(shot)?.closest('button, a') ?? null;
    collectPreviews();
    if (gallery) {
      untrack(() => {
        void gallery!
          .start()
          .then(applyGallery)
          .catch(() => {
            fallback = true;
          });
      });
    }
    dialog.showModal();
    stage.focus({ preventScroll: true });
    layer = pushLayer();
    measure();
    refit();
    paintNow();
    enter();
    showChrome();
    stage.addEventListener('wheel', wheel, { passive: false });
    resize = new ResizeObserver(() => {
      measure();
      refit();
      paintNow();
    });
    resize.observe(stage);
    if (page.state.sheet !== ROUTE_KEY) {
      // Назад на телефоне и в браузере закрывает просмотр, а не уходит из чата.
      // eslint-disable-next-line svelte/no-navigation-without-resolve
      void goto('', { shallow: true, state: { ...page.state, sheet: ROUTE_KEY } }).then(() => {
        entry = true;
      });
    }
  });

  // Шаг назад снимает запись просмотрщика: окно закрывается, а чат остаётся.
  $effect(() => {
    const sheet = page.state.sheet;
    if (entry && sheet !== ROUTE_KEY) untrack(() => void close(true));
  });

  onDestroy(() => {
    stage?.removeEventListener('wheel', wheel);
    resize?.disconnect();
    clearTimeout(settleTimer);
    clearTimeout(chromeTimer);
    clearTimeout(tapTimer);
    if (frame) cancelAnimationFrame(frame);
    if (dialog?.open) {
      dialog.close();
    }
    dropLayer(layer);
  });
</script>

<!-- Сцена ловит указатели сама: щипок, двойное касание и свайпы. Клавиатура работает на всём окне. -->
<dialog
  bind:this={dialog}
  class="viewer"
  aria-label={heading}
  aria-describedby={captionId}
  aria-modal="true"
  data-chrome={chrome ? 'shown' : 'hidden'}
  data-zoomed={zoomed ? '' : undefined}
  data-gesture={gesture}
  onkeydown={keydown}
  oncancel={(event) => { event.preventDefault(); void close(); }}
>
  <div class="backdrop" bind:this={backdrop}></div>
  <div
    class="stage"
    bind:this={stage}
    tabindex="-1"
    role="group"
    aria-roledescription={isClip ? 'видео' : 'картинка'}
    aria-label={many ? (knownTotal === null ? String(position) : `${position} из ${knownTotal}`) : caption || (isClip ? 'Видео' : 'Картинка')}
    onpointerdown={pointerDown}
    onpointermove={pointerMove}
    onpointerup={pointerUp}
    onpointercancel={pointerUp}
    onlostpointercapture={pointerUp}
  >
    {#each slides as i (list[i]?.media_id ?? i)}
      {@const entry = list[i]!}
      <div class="slide" data-index={i} data-current={i === index ? '' : undefined}>
        {#if isVideoShot(entry)}
          <!-- Ролик играет плеер браузера: без автозапуска, звук и перемотка его собственные. -->
          <div class="clip">
            {#if unplayable[entry.media_id]}
              <div class="unplayable" role="status">
                <p>Браузер не может воспроизвести это видео</p>
                <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
                <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
                <a class="control" href={shotSrc(entry)} download={downloadName(entry)}><Icon name="download" />Скачать</a>
              </div>
            {:else}
              <!-- У ролика владельца и агента нет дорожки субтитров: заводить её нечем. -->
              <!-- svelte-ignore a11y_media_has_caption -->
              <video
                src={shotSrc(entry)}
                controls
                playsinline
                preload="metadata"
                aria-label={shotCaption(entry, null) || `Видео ${i + 1}`}
                use:probe={entry}
                onerror={() => { unplayable = { ...unplayable, [entry.media_id]: true }; }}
              ></video>
            {/if}
          </div>
        {:else}
        <div class="frame" data-ready={ready[entry.media_id] ? '' : undefined}>
          {#if !ready[entry.media_id] || i !== index}
            <!-- Фон — уменьшенный показ: оригинал целиком грузится один раз, только для текущей картинки. -->
            <img
              class="preview"
              src={previews[entry.media_id] ?? shotPreviewSrc(entry)}
              alt=""
              draggable="false"
              onerror={(event) => {
                event.currentTarget.remove();
                if (i === index) previewGone = { ...previewGone, [entry.media_id]: true };
              }}
            />
          {/if}
          {#if i === index || ready[entry.media_id]}
            <img
              class="full"
              src={shotSrc(entry)}
              alt={shotCaption(entry, naturalOf(entry)) || `Картинка ${i + 1}`}
              decoding="async"
              draggable="false"
              onload={(event) => loaded(entry.media_id, event.currentTarget as HTMLImageElement)}
              onerror={() => { failed = { ...failed, [entry.media_id]: true }; }}
            />
          {/if}
        </div>
        {#if failed[entry.media_id]}
          <p class="status">Картинка недоступна</p>
        {:else if !ready[entry.media_id] && previewGone[entry.media_id]}
          <p class="status">Картинка загружается</p>
        {/if}
        {/if}
      </div>
    {/each}
  </div>
  <div class="chrome" bind:this={chromeBox} onfocusin={() => showChrome(true)} onfocusout={() => showChrome()}>
    <div class="top">
      <div class="header-actions">
        <button type="button" class="control round" aria-label="Закрыть" onclick={() => void close()}><Icon name="close" /></button>
        {#if currentAnchor}
          <button type="button" class="control round" aria-label="Показать в чате" title="Показать в чате" onclick={() => void reveal()}><Icon name="chats" /></button>
        {/if}
      </div>
      {#if many}<p class="counter" aria-live="polite">{knownTotal === null ? position : `${position} из ${knownTotal}`}</p>{/if}
      <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
      <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
      <a class="control save" href={src} download={downloadName(shot)}><Icon name="download" />Сохранить</a>
    </div>
    {#if many}
      <button type="button" class="control round side prev" aria-label="Предыдущая картинка" {...edge(index === 0 && !hasOlder)} onclick={() => void go(index - 1)}><Icon name="back" /></button>
      <button type="button" class="control round side next" aria-label="Следующая картинка" {...edge(index === list.length - 1 && !hasNewer)} onclick={() => void go(index + 1)}><Icon name="chevron" /></button>
    {/if}
    <div class="bottom">
      <p class="caption" id={captionId}>{caption}{#if title && shot.media_id === openedId && !captioned}<span class="title">{title}</span>{/if}</p>
    </div>
  </div>
</dialog>

<style>
  /* Сцена галереи: во весь экран, чёрная в любой теме. */
  .viewer {
    position: fixed;
    inset: 0;
    width: 100%;
    max-width: none;
    height: 100%;
    max-height: none;
    margin: 0;
    padding: 0;
    border: 0;
    overflow: hidden;
    background: var(--transparent);
    color: var(--viewer-text);
    overscroll-behavior: contain;
  }
  .viewer::backdrop { background: var(--transparent); }
  .backdrop { position: absolute; inset: 0; background: var(--viewer-bg); }
  .stage {
    position: absolute;
    inset: env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left);
    /* Жесты ведёт сама сцена. Долгое нажатие и правая кнопка остаются браузеру: его меню копирует и сохраняет картинку. */
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    cursor: zoom-in;
  }
  .stage:focus, .stage:focus-visible { outline: none; }
  .viewer[data-zoomed] .stage { cursor: grab; }
  .viewer[data-gesture='pan'] .stage { cursor: grabbing; }
  .slide { position: absolute; inset: 0; will-change: transform; }
  .slide:not([data-current]) { pointer-events: none; }
  .frame { position: absolute; top: 0; left: 0; transform-origin: 0 0; will-change: transform; }
  .frame img { position: absolute; inset: 0; display: block; width: 100%; height: 100%; max-width: none; }
  /* Под пальцем и мышью настоящая картинка: пока оригинал не готов, это миниатюра, потом оригинал. */
  .frame:not([data-ready]) .full, .frame[data-ready] .preview { pointer-events: none; }
  .frame .full { opacity: 0; }
  .frame[data-ready] .full { opacity: 1; }
  /* Ролик по центру сцены; поля под шапкой и подписью остаются сцене: по ним листают и закрывают свайпом. */
  .clip {
    position: absolute;
    inset: 0;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: calc(var(--target) + var(--s8)) var(--s4);
  }
  .clip video { display: block; max-width: 100%; max-height: 100%; border-radius: var(--radius-sm); background: var(--viewer-bg); }
  .unplayable {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s4);
    max-width: min(100%, var(--reading-max));
    padding: var(--s6) var(--s8);
    border-radius: var(--radius-sm);
    background: var(--viewer-control);
    color: var(--viewer-text);
    text-align: center;
  }
  .unplayable p { margin: 0; font-size: var(--fs-meta); }
  .stage:global([data-settling]) .slide, .stage:global([data-settling]) .frame { transition: transform var(--duration-expand) var(--ease-expand); }
  .viewer:has(:global(.stage[data-settling])) .backdrop { transition: opacity var(--duration-expand) var(--ease-expand); }
  .status {
    white-space: nowrap;
    position: absolute;
    top: 50%;
    left: 50%;
    margin: 0;
    padding: var(--s3) var(--s8);
    border-radius: var(--radius-pill);
    background: var(--viewer-control);
    color: var(--viewer-text);
    font-size: var(--fs-meta);
    transform: translate(-50%, -50%);
  }

  .chrome { position: absolute; inset: 0; pointer-events: none; transition: opacity var(--duration-fast) var(--ease); }
  .viewer[data-chrome='hidden'] .chrome:not(:focus-within) { opacity: 0; }
  .viewer[data-chrome='hidden'] .chrome:not(:focus-within) .control { pointer-events: none; }
  .top {
    position: absolute;
    top: 0;
    right: 0;
    left: 0;
    /* Три колонки: счётчик стоит ровно по центру, как бы ни отличались кнопки по краям. */
    display: grid;
    /* Боковые колонки равны даже при широких действиях справа: счётчик стоит ровно по центру. */
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    align-items: center;
    gap: var(--s4);
    padding: calc(env(safe-area-inset-top) + var(--s4)) calc(env(safe-area-inset-right) + var(--s4)) var(--s4) calc(env(safe-area-inset-left) + var(--s4));
  }
  .control {
    pointer-events: auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--s4);
    min-width: var(--target);
    min-height: var(--target);
    padding: var(--s0);
    border: 0;
    border-radius: var(--radius-round);
    background: var(--viewer-control);
    color: var(--viewer-text);
    font-size: var(--fs-meta);
  }
  .control:hover { background: var(--viewer-control-hover); }
  .control[aria-disabled='true'] { color: var(--viewer-muted); cursor: default; opacity: var(--opacity-dim); }
  .header-actions { grid-column: 1; justify-self: start; display: flex; align-items: center; gap: var(--s2); }
  .save { white-space: nowrap; grid-column: 3; justify-self: end; padding-inline: var(--s6) var(--s8); border-radius: var(--radius-pill); }
  .counter {
    white-space: nowrap;
    grid-column: 2;
    margin: 0;
    padding: var(--s3) var(--s6);
    border-radius: var(--radius-pill);
    background: var(--viewer-control);
    color: var(--viewer-text);
    font-size: var(--fs-meta);
    font-variant-numeric: tabular-nums;
  }
  .side { position: absolute; top: 50%; display: none; transform: translateY(-50%); }
  .prev { left: calc(env(safe-area-inset-left) + var(--s6)); }
  .next { right: calc(env(safe-area-inset-right) + var(--s6)); }
  /* Кнопки листания нужны мыши; на телефоне листает свайп. */
  @media (hover: hover) and (pointer: fine) { .side { display: inline-flex; } }
  .bottom {
    position: absolute;
    right: 0;
    bottom: 0;
    left: 0;
    display: flex;
    justify-content: center;
    padding: var(--s4) calc(env(safe-area-inset-right) + var(--s6)) calc(env(safe-area-inset-bottom) + var(--s6)) calc(env(safe-area-inset-left) + var(--s6));
  }
  .caption {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s1);
    max-width: min(100%, var(--reading-max));
    margin: 0;
    padding: var(--s3) var(--s8);
    border-radius: var(--radius-sm);
    background: var(--viewer-control);
    color: var(--viewer-text);
    font-size: var(--fs-sm);
    text-align: center;
    overflow-wrap: anywhere;
  }
  .caption:empty { display: none; }
  .title { display: -webkit-box; overflow: hidden; color: var(--viewer-muted); -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; }
</style>
