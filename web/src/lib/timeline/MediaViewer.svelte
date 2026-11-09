<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import Icon from '#lib/ui/Icon.svelte';
  import { durationMs, fadeMs } from '#lib/motion.js';
  import { declaredSize, downloadName, preloads, shotCaption, shotSrc, type ViewerShot } from '#lib/viewer/shots.js';
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

  let { shots, start = 0, title, heading = 'Картинки ответа', onclose }: { shots: ViewerShot[]; start?: number; title: string; heading?: string; onclose: () => void } = $props();

  const ROUTE_KEY = 'image-viewer';
  /** Через столько без касаний элементы управления прячутся сами. */
  const CHROME_IDLE_MS = 3000;
  /** Второе касание в этих пределах считается двойным. */
  const DOUBLE_TAP_MS = 300;
  const DOUBLE_TAP_SLOP = 32;
  const captionId = $props.id();

  let index = $state(untrack(() => Math.min(Math.max(0, start), shots.length - 1)));
  let chrome = $state(true);
  let zoomed = $state(false);
  let gesture = $state<'idle' | 'pan' | 'pinch' | 'swipe' | 'dismiss'>('idle');
  /** Натуральный размер каждой картинки: из описания, миниатюры или загруженного оригинала. */
  let naturals = $state<Record<number, Size>>({});
  /** Оригинал раскодирован и заменил миниатюру. */
  let ready = $state<Record<number, boolean>>({});
  let failed = $state<Record<number, boolean>>({});
  /** Миниатюра из ленты, которую можно показать до оригинала. */
  let previews = $state<Record<number, string>>({});

  const shot = $derived(shots[index] ?? shots[0]!);
  const src = $derived(shotSrc(shot));
  const caption = $derived(shotCaption(shot, naturals[index] ?? declaredSize(shot)));
  const many = $derived(shots.length > 1);
  const slides = $derived([index - 1, index, index + 1].filter((i) => i >= 0 && i < shots.length));

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
  let shell: HTMLElement | null = null;
  let previousInert = false;
  let entry = $state(false);
  let closing = false;
  let settleTimer: ReturnType<typeof setTimeout> | undefined;
  let chromeTimer: ReturnType<typeof setTimeout> | undefined;
  let tapTimer: ReturnType<typeof setTimeout> | undefined;
  let frame = 0;
  let resize: ResizeObserver | null = null;

  // Указатели сцены: позиция каждого пальца и начало текущего жеста.
  // Счётчик пальцев живёт вне реактивности: он меняется на каждом событии указателя и разметку не трогает.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const pointers = new Map<number, Point>();
  let press: { point: Point; view: View; swipeX: number; time: number; type: string } | null = null;
  let pinch: { from: [Point, Point]; view: View } | null = null;
  let lastTap: { time: number; point: Point } | null = null;
  let samples: { time: number; x: number; y: number }[] = [];

  function naturalOf(i: number): Size | null {
    return naturals[i] ?? declaredSize(shots[i]!);
  }

  function viewOf(i: number): View {
    const natural = naturalOf(i);
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
      const natural = naturalOf(i);
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
    zoomed = isZoomed(view, size, naturalOf(index) ?? size);
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
    zoomed = isZoomed(view, size, naturalOf(index) ?? size);
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
    const natural = naturalOf(index);
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
  function thumbOf(i: number): HTMLImageElement | null {
    const id = shots[i]?.media_id;
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
    const next: Record<number, string> = {};
    const found: Record<number, Size> = {};
    shots.forEach((_, i) => {
      const thumb = thumbOf(i);
      if (thumb?.complete && thumb.naturalWidth > 0) {
        next[i] = thumb.currentSrc || thumb.src;
        if (!declaredSize(shots[i]!)) found[i] = { width: thumb.naturalWidth, height: thumb.naturalHeight };
      }
    });
    previews = next;
    naturals = { ...found, ...naturals };
  }

  function easeToken(): string {
    return getComputedStyle(document.documentElement).getPropertyValue('--ease-viewer-open').trim() || 'ease-out';
  }

  function clip(rect: { top: number; right: number; bottom: number; left: number }, radius: number): string {
    return `inset(${rect.top}px ${rect.right}px ${rect.bottom}px ${rect.left}px round ${radius}px)`;
  }

  /** Въезд: кадр ложится на миниатюру и раскрывается в вписанный вид, фон проявляется. */
  function enter(): void {
    const natural = naturalOf(index);
    const target = frameOf(index);
    const fade = fadeMs('--duration-viewer-fade');
    const duration = durationMs('--duration-viewer-open');
    const rect = visibleRect(thumbOf(index));
    const stageBox = stage.getBoundingClientRect();
    if (duration > 0 && natural && target && rect) {
      const { view: from, clip: cut } = coverRect({ x: rect.left - stageBox.left, y: rect.top - stageBox.top, width: rect.width, height: rect.height }, natural);
      const radius = parseFloat(getComputedStyle(thumbOf(index)!.closest('button, a') ?? document.body).borderTopLeftRadius) || 0;
      target.animate(
        [
          { transform: transformOf(from), clipPath: clip(cut, radius / from.scale) },
          { transform: transformOf(view), clipPath: clip({ top: 0, right: 0, bottom: 0, left: 0 }, 0) }
        ],
        { duration: durationMs('--duration-viewer-open'), easing: easeToken() }
      );
      backdrop.animate([{ opacity: 0 }, { opacity: 1 }], { duration: durationMs('--duration-viewer-open'), easing: easeToken() });
      chromeBox.animate([{ opacity: 0 }, { opacity: 1 }], { duration: fadeMs('--duration-viewer-fade') });
      return;
    }
    if (fade > 0) dialog.animate([{ opacity: 0 }, { opacity: 1 }], { duration: fadeMs('--duration-viewer-fade') });
  }

  /** Уход: кадр с текущего места возвращается на миниатюру; если её не видно, просмотрщик гаснет. */
  async function leave(): Promise<void> {
    const natural = naturalOf(index);
    const target = frameOf(index);
    const duration = durationMs('--duration-viewer-open');
    const fade = fadeMs('--duration-viewer-fade');
    const thumb = thumbOf(index);
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
      const { view: to, clip: cut } = coverRect({ x: rect.left - stageBox.left, y: rect.top - stageBox.top, width: rect.width, height: rect.height }, natural);
      const radius = parseFloat(getComputedStyle(thumb!.closest('button, a') ?? document.body).borderTopLeftRadius) || 0;
      chrome = false;
      const motion = target.animate(
        [
          { transform: transformOf(current), clipPath: clip({ top: 0, right: 0, bottom: 0, left: 0 }, 0) },
          { transform: transformOf(to), clipPath: clip(cut, radius / to.scale) }
        ],
        { duration: durationMs('--duration-viewer-open'), easing: easeToken(), fill: 'forwards' }
      );
      backdrop.animate([{ opacity }, { opacity: 0 }], { duration: durationMs('--duration-viewer-open'), easing: easeToken(), fill: 'forwards' });
      await motion.finished.catch(() => undefined);
      return;
    }
    if (fade > 0) await dialog.animate([{ opacity: 1 }, { opacity: 0 }], { duration: fadeMs('--duration-viewer-fade'), fill: 'forwards' }).finished.catch(() => undefined);
  }

  function restoreFocus(): void {
    const thumb = thumbOf(index)?.closest<HTMLElement>('button, a');
    (thumb ?? opener)?.focus({ preventScroll: true });
  }

  async function close(fromHistory = false): Promise<void> {
    if (closing) return;
    closing = true;
    clearTimeout(chromeTimer);
    clearTimeout(tapTimer);
    await leave();
    if (dialog.open) dialog.close();
    if (shell) shell.inert = previousInert;
    restoreFocus();
    if (!fromHistory && entry && page.state.sheet === ROUTE_KEY) history.back();
    entry = false;
    onclose();
  }

  async function go(next: number, fromSwipe = false): Promise<void> {
    if (next < 0 || next >= shots.length || next === index) {
      if (fromSwipe) animateTo(view);
      return;
    }
    const step = next - index;
    // Лента остаётся на месте: новый индекс сдвигает её на ширину кадра, а остаток уезжает переходом.
    const carried = swipeX + step * (size.width + gap());
    index = next;
    view = fitView(size, naturalOf(next) ?? size);
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
    stage.setPointerCapture?.(event.pointerId);
    const point = local(event);
    pointers.set(event.pointerId, point);
    if (pointers.size === 1) {
      stopSettling();
      press = { point, view, swipeX, time: performance.now(), type: event.pointerType };
      samples = [];
      record(point);
    } else if (pointers.size === 2) {
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
    const natural = naturalOf(index);
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
      else gesture = 'pan';
      if (gesture !== 'swipe') hideChrome();
    }
    if (gesture === 'pan') {
      setView(rubberView({ scale: press.view.scale, x: press.view.x + dx, y: press.view.y + dy }, size, natural));
    } else if (gesture === 'swipe') {
      // У первой и последней картинки лента тянется с сопротивлением.
      const min = index === shots.length - 1 ? 0 : -Infinity;
      const max = index === 0 ? 0 : Infinity;
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
      const natural = naturalOf(index);
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
    const natural = naturalOf(index);
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
    const natural = naturalOf(index);
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
    const natural = naturalOf(index);
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
    const natural = naturalOf(index);
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
  function loaded(i: number, image: HTMLImageElement): void {
    const natural = { width: image.naturalWidth, height: image.naturalHeight };
    const known = naturalOf(i);
    if (!known || known.width !== natural.width || known.height !== natural.height) {
      naturals = { ...naturals, [i]: natural };
      if (i === index) {
        view = fitView(size, natural);
        zoomed = false;
      }
    }
    const reveal = () => {
      if (closing) return;
      ready = { ...ready, [i]: true };
      paintNow();
    };
    if (typeof image.decode === 'function') image.decode().then(reveal, reveal);
    else reveal();
    schedule();
  }

  onMount(() => {
    opener = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : thumbOf(index)?.closest('button, a') ?? null;
    shell = document.getElementById('app-shell');
    previousInert = shell?.inert ?? false;
    collectPreviews();
    dialog.showModal();
    stage.focus({ preventScroll: true });
    if (shell) shell.inert = true;
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
      if (shell) shell.inert = previousInert;
    }
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
    aria-roledescription="картинка"
    aria-label={many ? `${index + 1} из ${shots.length}` : caption || 'Картинка'}
    onpointerdown={pointerDown}
    onpointermove={pointerMove}
    onpointerup={pointerUp}
    onpointercancel={pointerUp}
    onlostpointercapture={pointerUp}
  >
    {#each slides as i (i)}
      <div class="slide" data-index={i} data-current={i === index ? '' : undefined}>
        <div class="frame" data-ready={ready[i] ? '' : undefined}>
          {#if previews[i] && !ready[i]}
            <img class="preview" src={previews[i]} alt="" draggable="false" />
          {/if}
          {#if i === index || preloads(shots[i]!)}
            <img
              class="full"
              src={shotSrc(shots[i]!)}
              alt={shotCaption(shots[i]!, naturalOf(i)) || `Картинка ${i + 1}`}
              decoding="async"
              draggable="false"
              onload={(event) => loaded(i, event.currentTarget as HTMLImageElement)}
              onerror={() => { failed = { ...failed, [i]: true }; }}
            />
          {/if}
        </div>
        {#if failed[i]}
          <p class="status">Картинка недоступна</p>
        {:else if !ready[i] && !previews[i]}
          <p class="status">Картинка загружается</p>
        {/if}
      </div>
    {/each}
  </div>
  <div class="chrome" bind:this={chromeBox} onfocusin={() => showChrome(true)} onfocusout={() => showChrome()}>
    <div class="top">
      <button type="button" class="control round" aria-label="Закрыть" onclick={() => void close()}><Icon name="close" /></button>
      {#if many}<p class="counter" aria-live="polite">{index + 1} из {shots.length}</p>{/if}
      <!-- Адрес служебный, маршрутизатор здесь не участвует. -->
      <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
      <a class="control save" href={src} download={downloadName(shot)}><Icon name="download" />Сохранить</a>
    </div>
    {#if many}
      <button type="button" class="control round side prev" aria-label="Предыдущая картинка" {...edge(index === 0)} onclick={() => void go(index - 1)}><Icon name="back" /></button>
      <button type="button" class="control round side next" aria-label="Следующая картинка" {...edge(index === shots.length - 1)} onclick={() => void go(index + 1)}><Icon name="chevron" /></button>
    {/if}
    <div class="bottom">
      <p class="caption" id={captionId}>{caption}{#if title}<span class="title">{title}</span>{/if}</p>
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
  .stage:global([data-settling]) .slide, .stage:global([data-settling]) .frame { transition: transform var(--duration-expand) var(--ease-expand); }
  .viewer:has(:global(.stage[data-settling])) .backdrop { transition: opacity var(--duration-expand) var(--ease-expand); }
  .status {
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
    grid-template-columns: 1fr auto 1fr;
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
  .top > .control:first-child { justify-self: start; }
  .save { grid-column: 3; justify-self: end; padding-inline: var(--s6) var(--s8); border-radius: var(--radius-pill); }
  .counter {
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
