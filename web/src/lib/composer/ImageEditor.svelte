<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Button from '#lib/ui/Button.svelte';
  import { loadImage, type LoadedImage } from '#lib/composer/imageEdit/decode.js';
  import {
    MARK_COLORS,
    addStroke,
    clampRect,
    createEdit,
    fitImage,
    outputPlan,
    resizeRect,
    setCrop,
    strokeWidth,
    toImagePoint,
    toScreenPoint,
    undo,
    type CropHandle,
    type Edit,
    type MarkColor,
    type Point,
    type Rect,
    type Size
  } from '#lib/composer/imageEdit/model.js';

  let {
    file,
    ondone,
    onoriginal,
    oncancel
  }: {
    file: File;
    ondone: (edited: File) => void;
    onoriginal: () => void;
    oncancel: () => void;
  } = $props();

  let dialog: HTMLDialogElement;
  let stage: HTMLDivElement;
  let canvas: HTMLCanvasElement;
  let doneButton: HTMLSpanElement;
  let loaded: LoadedImage | null = null;
  let image = $state<Size>({ width: 0, height: 0 });
  let view = $state<Rect>({ x: 0, y: 0, width: 0, height: 0 });
  let edit = $state<Edit>(createEdit({ width: 1, height: 1 }));
  let draft = $state<Rect | null>(null);
  let tool = $state<'crop' | 'pencil'>('crop');
  let color = $state<MarkColor>('red');
  let busy = $state(true);
  let exporting = $state(false);
  let error = $state<string | null>(null);
  const COLOR_LABELS: Record<MarkColor, string> = {
    red: 'красный',
    yellow: 'жёлтый',
    green: 'зелёный',
    blue: 'синий'
  };
  let paint: Record<MarkColor, string> = { red: '', yellow: '', green: '', blue: '' };
  let frame = '';
  let outline = '';
  let scrim = '';
  let backdrop = '';
  let dpr = 1;
  let active: { color: MarkColor; points: Point[] } | null = null;
  let drag: {
    mode: 'move' | 'resize' | 'new';
    handle?: CropHandle;
    start: Point;
    rect: Rect;
  } | null = null;

  function local(event: PointerEvent): Point {
    const box = canvas.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  }

  function shownCrop(): Rect {
    return draft ?? edit.crop;
  }

  function handles(rect: Rect): { handle: CropHandle; point: Point }[] {
    const topLeft = toScreenPoint({ x: rect.x, y: rect.y }, view, image);
    const bottomRight = toScreenPoint(
      { x: rect.x + rect.width, y: rect.y + rect.height },
      view,
      image
    );
    const middleX = (topLeft.x + bottomRight.x) / 2;
    const middleY = (topLeft.y + bottomRight.y) / 2;
    return [
      { handle: 'nw', point: topLeft },
      { handle: 'n', point: { x: middleX, y: topLeft.y } },
      { handle: 'ne', point: { x: bottomRight.x, y: topLeft.y } },
      { handle: 'e', point: { x: bottomRight.x, y: middleY } },
      { handle: 'se', point: bottomRight },
      { handle: 's', point: { x: middleX, y: bottomRight.y } },
      { handle: 'sw', point: { x: topLeft.x, y: bottomRight.y } },
      { handle: 'w', point: { x: topLeft.x, y: middleY } }
    ];
  }

  function hitHandle(point: Point): CropHandle | null {
    for (const { handle, point: at } of handles(shownCrop())) {
      if (Math.abs(point.x - at.x) <= 14 && Math.abs(point.y - at.y) <= 14) return handle;
    }
    return null;
  }

  function strokePath(ctx: CanvasRenderingContext2D, points: Point[], offset?: Point): void {
    if (points.length === 0) return;
    const first = toScreenPoint(
      { x: points[0]!.x - (offset?.x ?? 0), y: points[0]!.y - (offset?.y ?? 0) },
      view,
      image
    );
    ctx.beginPath();
    ctx.moveTo(first.x, first.y);
    for (const point of points.slice(1)) {
      const at = toScreenPoint(
        { x: point.x - (offset?.x ?? 0), y: point.y - (offset?.y ?? 0) },
        view,
        image
      );
      ctx.lineTo(at.x, at.y);
    }
    if (points.length === 1) ctx.lineTo(first.x + 0.01, first.y + 0.01);
    ctx.stroke();
  }

  function draw(): void {
    const ctx = canvas?.getContext('2d');
    if (!ctx || !loaded || !image.width || canvas.width === 0) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = backdrop;
    ctx.fillRect(0, 0, canvas.width / dpr, canvas.height / dpr);
    ctx.drawImage(loaded.source, view.x, view.y, view.width, view.height);

    const strokes = active ? [...edit.strokes, active] : edit.strokes;
    const scale = view.width / image.width;
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.lineWidth = strokeWidth(image) * scale;
    for (const stroke of strokes) {
      ctx.strokeStyle = outline;
      ctx.lineWidth = strokeWidth(image) * scale + 2;
      strokePath(ctx, stroke.points);
      ctx.strokeStyle = paint[stroke.color];
      ctx.lineWidth = strokeWidth(image) * scale;
      strokePath(ctx, stroke.points);
    }

    if (tool === 'crop') {
      const rect = shownCrop();
      const topLeft = toScreenPoint({ x: rect.x, y: rect.y }, view, image);
      const bottomRight = toScreenPoint(
        { x: rect.x + rect.width, y: rect.y + rect.height },
        view,
        image
      );
      const left = topLeft.x;
      const top = topLeft.y;
      const right = bottomRight.x;
      const bottom = bottomRight.y;
      const width = canvas.width / dpr;
      const height = canvas.height / dpr;
      ctx.fillStyle = scrim;
      ctx.fillRect(0, 0, width, top);
      ctx.fillRect(0, bottom, width, height - bottom);
      ctx.fillRect(0, top, left, bottom - top);
      ctx.fillRect(right, top, width - right, bottom - top);
      ctx.strokeStyle = frame;
      ctx.lineWidth = 2;
      ctx.strokeRect(left, top, right - left, bottom - top);
      ctx.fillStyle = frame;
      for (const { point } of handles(rect)) ctx.fillRect(point.x - 5, point.y - 5, 10, 10);
    } else {
      const rect = edit.crop;
      const topLeft = toScreenPoint({ x: rect.x, y: rect.y }, view, image);
      const bottomRight = toScreenPoint(
        { x: rect.x + rect.width, y: rect.y + rect.height },
        view,
        image
      );
      ctx.strokeStyle = frame;
      ctx.lineWidth = 1;
      ctx.setLineDash([5, 5]);
      ctx.strokeRect(topLeft.x, topLeft.y, bottomRight.x - topLeft.x, bottomRight.y - topLeft.y);
      ctx.setLineDash([]);
    }
  }

  function refit(): void {
    if (!stage || !canvas || !image.width) return;
    const width = stage.clientWidth;
    const height = stage.clientHeight;
    if (!width || !height) return;
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    view = fitImage(image, { width, height });
    draw();
  }

  onMount(() => {
    const styles = getComputedStyle(dialog);
    // Диалог в верхнем слое: превращённый предок не ограничивает его размер.
    dialog.showModal();
    for (const name of MARK_COLORS) paint[name] = styles.getPropertyValue(`--mark-${name}`).trim();
    frame = styles.getPropertyValue('--mark-frame').trim();
    outline = styles.getPropertyValue('--mark-outline').trim();
    scrim = styles.getPropertyValue('--mark-scrim').trim();
    backdrop = styles.getPropertyValue('--surface').trim();
    let disposed = false;
    void loadImage(file).then((result) => {
      if (disposed) {
        result?.close();
        return;
      }
      if (!result) {
        busy = false;
        error = 'Не удалось открыть изображение';
        return;
      }
      loaded = result;
      image = { width: result.width, height: result.height };
      edit = createEdit(image);
      busy = false;
      void tick().then(refit);
    });
    const observer = new ResizeObserver(() => refit());
    observer.observe(stage);
    void tick().then(() => doneButton?.focus({ preventScroll: true }));
    return () => {
      disposed = true;
      observer.disconnect();
      loaded?.close();
      if (dialog.open) dialog.close();
    };
  });

  function commitCrop(): void {
    if (!draft) return;
    const next = setCrop(edit, draft, image);
    if (next !== edit) edit = next;
    draft = null;
  }

  function setTool(next: 'crop' | 'pencil'): void {
    if (next !== tool && tool === 'crop' && draft) commitCrop();
    tool = next;
    draw();
  }

  function down(event: PointerEvent): void {
    if (!loaded || busy || exporting) return;
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    event.preventDefault();
    canvas.setPointerCapture(event.pointerId);
    const point = local(event);
    if (tool === 'pencil') {
      active = { color, points: [toImagePoint(point, view, image)] };
      draw();
      return;
    }
    const handle = hitHandle(point);
    if (handle) {
      drag = { mode: 'resize', handle, start: point, rect: edit.crop };
      return;
    }
    const topLeft = toScreenPoint({ x: edit.crop.x, y: edit.crop.y }, view, image);
    const bottomRight = toScreenPoint(
      { x: edit.crop.x + edit.crop.width, y: edit.crop.y + edit.crop.height },
      view,
      image
    );
    if (
      point.x >= topLeft.x &&
      point.x <= bottomRight.x &&
      point.y >= topLeft.y &&
      point.y <= bottomRight.y
    ) {
      drag = { mode: 'move', start: point, rect: edit.crop };
      return;
    }
    const start = toImagePoint(point, view, image);
    draft = { x: start.x, y: start.y, width: 0, height: 0 };
    drag = { mode: 'new', start: point, rect: draft };
    draw();
  }

  function move(event: PointerEvent): void {
    if (!drag && !active) return;
    const point = local(event);
    if (active) {
      active.points.push(toImagePoint(point, view, image));
      draw();
      return;
    }
    const dx = ((point.x - drag!.start.x) * image.width) / view.width;
    const dy = ((point.y - drag!.start.y) * image.height) / view.height;
    if (drag!.mode === 'move') {
      draft = clampRect({ ...drag!.rect, x: drag!.rect.x + dx, y: drag!.rect.y + dy }, image);
    } else if (drag!.mode === 'resize') {
      draft = resizeRect(drag!.rect, drag!.handle!, dx, dy, image, 2);
    } else {
      const start = toImagePoint(drag!.start, view, image);
      const end = toImagePoint(point, view, image);
      draft = clampRect(
        {
          x: Math.min(start.x, end.x),
          y: Math.min(start.y, end.y),
          width: Math.abs(end.x - start.x),
          height: Math.abs(end.y - start.y)
        },
        image,
        2
      );
    }
    draw();
  }

  function up(event: PointerEvent): void {
    if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
    if (active) {
      if (active.points.length > 0) edit = addStroke(edit, active);
      active = null;
      draw();
      return;
    }
    if (!drag) return;
    if (drag.mode === 'new' && draft && (draft.width < 3 || draft.height < 3)) draft = null;
    else commitCrop();
    drag = null;
    draw();
  }

  function undoLast(): void {
    edit = undo(edit);
    draft = null;
    draw();
  }

  async function done(): Promise<void> {
    if (!loaded || exporting) return;
    if (tool === 'crop' && draft) commitCrop();
    exporting = true;
    try {
      const plan = outputPlan(file.name, file.type, image, edit.crop);
      const out = document.createElement('canvas');
      out.width = plan.width;
      out.height = plan.height;
      const ctx = out.getContext('2d');
      if (!ctx) throw new Error('canvas');
      ctx.drawImage(
        loaded.source,
        edit.crop.x,
        edit.crop.y,
        edit.crop.width,
        edit.crop.height,
        0,
        0,
        plan.width,
        plan.height
      );
      const scale = (plan.width / edit.crop.width + plan.height / edit.crop.height) / 2;
      for (const stroke of edit.strokes) {
        ctx.lineCap = 'round';
        ctx.lineJoin = 'round';
        ctx.strokeStyle = outline;
        ctx.lineWidth = strokeWidth(image) * scale + 2;
        ctx.beginPath();
        stroke.points.forEach((point, index) => {
          const x = (point.x - edit.crop.x) * (plan.width / edit.crop.width);
          const y = (point.y - edit.crop.y) * (plan.height / edit.crop.height);
          if (index === 0) ctx.moveTo(x, y);
          else ctx.lineTo(x, y);
        });
        ctx.stroke();
        ctx.strokeStyle = paint[stroke.color];
        ctx.lineWidth = strokeWidth(image) * scale;
        ctx.beginPath();
        stroke.points.forEach((point, index) => {
          const x = (point.x - edit.crop.x) * (plan.width / edit.crop.width);
          const y = (point.y - edit.crop.y) * (plan.height / edit.crop.height);
          if (index === 0) ctx.moveTo(x, y);
          else ctx.lineTo(x, y);
        });
        ctx.stroke();
      }
      const blob = await new Promise<Blob | null>((resolve) =>
        out.toBlob(resolve, plan.mime, plan.quality)
      );
      if (!blob) throw new Error('blob');
      ondone(new File([blob], plan.name, { type: plan.mime }));
    } catch {
      error = 'Не удалось сохранить правки';
      exporting = false;
    }
  }

  function keydown(event: KeyboardEvent): void {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'z') {
      event.preventDefault();
      undoLast();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      oncancel();
    }
  }
</script>

<svelte:window onkeydown={keydown} />

<dialog
  class="editor"
  aria-label="Правка изображения"
  bind:this={dialog}
  oncancel={(event) => {
    event.preventDefault();
    oncancel();
  }}
>
  <header>
    <Button variant="text" onclick={oncancel}>Отмена</Button>
    <Button variant="text" onclick={onoriginal}>Без правок</Button>
    <Button variant="primary" busy={exporting} onclick={() => void done()}>
      <span bind:this={doneButton} tabindex="-1">Готово</span>
    </Button>
  </header>
  <div class="stage" bind:this={stage}>
    <canvas
      bind:this={canvas}
      aria-label="Снимок с рамкой обрезки и рисунками от руки"
      onpointerdown={down}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={up}
    ></canvas>
    {#if busy}<p class="status">Загрузка изображения…</p>{/if}
    {#if error}<p class="status">{error}</p>{/if}
  </div>
  <div class="tools" role="group" aria-label="Инструменты правки">
    <button type="button" class:active={tool === 'crop'} aria-pressed={tool === 'crop'} onclick={() => setTool('crop')}>Обрезать</button>
    <button type="button" class:active={tool === 'pencil'} aria-pressed={tool === 'pencil'} onclick={() => setTool('pencil')}>Карандаш</button>
    <button type="button" onclick={undoLast}>Отменить</button>
  </div>
  <div class="palette" role="group" aria-label="Цвет карандаша">
    {#each MARK_COLORS as name (name)}
      <button
        type="button"
        class={name}
        class:active={color === name}
        aria-label={COLOR_LABELS[name]}
        title={COLOR_LABELS[name]}
        aria-pressed={color === name}
        onclick={() => {
          color = name;
          setTool('pencil');
        }}
      ></button>
    {/each}
  </div>
</dialog>

<style>
  .editor {
    position: fixed;
    inset: 0;
    width: 100%;
    height: 100%;
    max-width: none;
    max-height: none;
    margin: 0;
    border: 0;
    border-radius: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--text);
    padding: var(--s4) var(--s4) calc(var(--s4) + env(safe-area-inset-bottom));
    gap: var(--s4);
  }
  .editor::backdrop { background: var(--scrim); }
  header {
    flex: none;
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    align-items: center;
  }
  header :global(button:nth-child(1)) { justify-self: start; }
  header :global(button:nth-child(2)) { justify-self: center; }
  header :global(button:nth-child(3)) { justify-self: end; }
  .stage {
    flex: 1 1 auto;
    min-height: 0;
    position: relative;
    overflow: hidden;
    border-radius: var(--radius);
  }
  canvas {
    display: block;
    width: 100%;
    height: 100%;
    touch-action: none;
  }
  .status {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    margin: 0;
    background: var(--scrim);
    color: var(--text);
  }
  .tools {
    flex: none;
    display: flex;
    justify-content: center;
    gap: var(--s6);
  }
  .tools button {
    min-height: var(--target);
    white-space: nowrap;
    padding: var(--s2) var(--s6);
    border: var(--s0);
    border-radius: var(--radius-pill);
    background: var(--raised);
    color: var(--text);
  }
  .tools button.active,
  .tools button.active:hover,
  .tools button.active:active {
    background: var(--accent);
    color: var(--accent-ink);
  }
  .palette {
    flex: none;
    display: flex;
    justify-content: center;
    gap: var(--s6);
  }
  .palette button {
    width: var(--target);
    height: var(--target);
    border: var(--s2) solid var(--transparent);
    border-radius: var(--radius-round);
    background: var(--swatch);
  }
  .palette button:hover,
  .palette button:active {
    background: var(--swatch);
  }
  .palette button.active {
    border-color: var(--text);
  }
  .red { --swatch: var(--mark-red); }
  .yellow { --swatch: var(--mark-yellow); }
  .green { --swatch: var(--mark-green); }
  .blue { --swatch: var(--mark-blue); }
  :global([data-layout='desktop']) .editor { padding: var(--s6); }
</style>
