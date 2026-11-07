/**
 * Геометрия просмотрщика картинок без DOM: масштаб и сдвиг от пальцев, колеса и двойного касания, границы и резинка.
 *
 * Картинка лежит в кадре своего натурального размера с началом координат в левом верхнем углу сцены;
 * точка картинки (u, v) попадает на экран в (x + u * scale, y + v * scale).
 */

export interface Size {
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

export interface View {
  scale: number;
  x: number;
  y: number;
}

/** Во сколько раз двойное касание приближает от вписанного размера. */
export const TAP_ZOOM = 2.5;
/** Наибольшее приближение от натуральных пикселей. */
export const MAX_ZOOM = 4;
/** Насколько сильно сцена сопротивляется выходу за край: доля, как у резинки в системных галереях. */
export const RUBBER = 0.55;
/** Щипок может уйти за пределы масштаба, но только на эту долю, и возвращается после отпускания. */
export const PINCH_OVERSHOOT = 0.35;

const EPSILON = 1e-3;

/** Вписанный масштаб: картинка целиком на сцене; мелкую не растягиваем больше натурального размера. */
export function fitScale(stage: Size, natural: Size): number {
  if (natural.width <= 0 || natural.height <= 0 || stage.width <= 0 || stage.height <= 0) return 1;
  return Math.min(stage.width / natural.width, stage.height / natural.height, 1);
}

export function maxScale(stage: Size, natural: Size): number {
  return Math.max(MAX_ZOOM, fitScale(stage, natural));
}

/** Картинка в масштабе `scale` по центру сцены. */
export function centred(stage: Size, natural: Size, scale: number): View {
  return { scale, x: (stage.width - natural.width * scale) / 2, y: (stage.height - natural.height * scale) / 2 };
}

export function fitView(stage: Size, natural: Size): View {
  return centred(stage, natural, fitScale(stage, natural));
}

export function isZoomed(view: View, stage: Size, natural: Size): boolean {
  return view.scale > fitScale(stage, natural) * (1 + EPSILON);
}

/** Пределы сдвига: если картинка уже сцены, она стоит по центру, иначе край не отходит от края сцены. */
export function bounds(stage: Size, natural: Size, scale: number): { minX: number; maxX: number; minY: number; maxY: number } {
  const width = natural.width * scale;
  const height = natural.height * scale;
  const axis = (outer: number, inner: number): [number, number] => (inner <= outer ? [(outer - inner) / 2, (outer - inner) / 2] : [outer - inner, 0]);
  const [minX, maxX] = axis(stage.width, width);
  const [minY, maxY] = axis(stage.height, height);
  return { minX, maxX, minY, maxY };
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** Сдвиг внутри пределов текущего масштаба. */
export function clampView(view: View, stage: Size, natural: Size): View {
  const box = bounds(stage, natural, view.scale);
  return { scale: view.scale, x: clamp(view.x, box.minX, box.maxX), y: clamp(view.y, box.minY, box.maxY) };
}

/** Резинка: за краем значение растёт всё медленнее и никогда не уходит дальше `span`. */
export function rubber(value: number, min: number, max: number, span: number): number {
  if (span <= 0) return clamp(value, min, max);
  const pull = (over: number) => (1 - 1 / ((over * RUBBER) / span + 1)) * span;
  if (value < min) return min - pull(min - value);
  if (value > max) return max + pull(value - max);
  return value;
}

/** Сдвиг с резинкой у краёв: картинку можно потянуть за край, но не потерять. */
export function rubberView(view: View, stage: Size, natural: Size): View {
  const box = bounds(stage, natural, view.scale);
  return { scale: view.scale, x: rubber(view.x, box.minX, box.maxX, stage.width), y: rubber(view.y, box.minY, box.maxY, stage.height) };
}

/** Новый масштаб так, чтобы точка экрана `at` осталась над той же точкой картинки. */
export function zoomAt(view: View, at: Point, scale: number): View {
  const ratio = scale / view.scale;
  return { scale, x: at.x - (at.x - view.x) * ratio, y: at.y - (at.y - view.y) * ratio };
}

/** Масштаб в своих пределах; щипок получает мягкий запас с сопротивлением. */
export function limitScale(scale: number, stage: Size, natural: Size, soft = false): number {
  const min = fitScale(stage, natural);
  const max = maxScale(stage, natural);
  if (!soft) return clamp(scale, min, max);
  if (scale < min) return min * (1 - PINCH_OVERSHOOT * (1 - 1 / ((min - scale) / (min * PINCH_OVERSHOOT) + 1)));
  if (scale > max) return max * (1 + PINCH_OVERSHOOT * (1 - 1 / ((scale - max) / (max * PINCH_OVERSHOOT) + 1)));
  return scale;
}

export function midpoint(a: Point, b: Point): Point {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

export function distance(a: Point, b: Point): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

/**
 * Щипок двумя пальцами: масштаб растёт как расстояние между ними, а точка картинки под серединой
 * в начале жеста идёт за серединой сейчас. Так один жест и приближает, и сдвигает.
 */
export function pinchView(start: View, from: [Point, Point], to: [Point, Point], stage: Size, natural: Size): View {
  const before = distance(from[0], from[1]);
  const after = distance(to[0], to[1]);
  const scale = limitScale(before > 0 ? start.scale * (after / before) : start.scale, stage, natural, true);
  const anchor = midpoint(from[0], from[1]);
  const now = midpoint(to[0], to[1]);
  const ratio = scale / start.scale;
  return rubberView({ scale, x: now.x - (anchor.x - start.x) * ratio, y: now.y - (anchor.y - start.y) * ratio }, stage, natural);
}

/** Куда оседает вид после жеста: масштаб в пределах, держась за точку `at`, затем сдвиг в границах. */
export function settle(view: View, stage: Size, natural: Size, at?: Point): View {
  const scale = limitScale(view.scale, stage, natural);
  const anchor = at ?? { x: stage.width / 2, y: stage.height / 2 };
  const scaled = Math.abs(scale - view.scale) > EPSILON ? zoomAt(view, anchor, scale) : view;
  return clampView(scaled, stage, natural);
}

/** Масштаб двойного касания: 2,5 от вписанного или натуральные пиксели, если это крупнее. */
export function tapZoomScale(stage: Size, natural: Size): number {
  return Math.min(maxScale(stage, natural), Math.max(fitScale(stage, natural) * TAP_ZOOM, 1));
}

/** Двойное касание: из вписанного приближает в точке касания, из приближения возвращает вписанный вид. */
export function toggleZoom(view: View, at: Point, stage: Size, natural: Size): View {
  if (isZoomed(view, stage, natural)) return fitView(stage, natural);
  return clampView(zoomAt(view, at, tapZoomScale(stage, natural)), stage, natural);
}

/** Колесо: шаг пропорционален прокрутке, сенсорная панель с ctrl (щипок) идёт мельче. */
export function wheelScale(view: View, deltaY: number, pinch: boolean, stage: Size, natural: Size): number {
  const factor = Math.exp(-deltaY * (pinch ? 0.01 : 0.002));
  return limitScale(view.scale * factor, stage, natural);
}

/** Клавиши +/−: шаг в полтора раза. */
export function stepScale(view: View, direction: 1 | -1, stage: Size, natural: Size): number {
  return limitScale(view.scale * (direction > 0 ? 1.5 : 1 / 1.5), stage, natural);
}

/** CSS-преобразование вида. */
export function transformOf(view: View): string {
  return `translate3d(${view.x}px, ${view.y}px, 0) scale(${view.scale})`;
}

/**
 * Преобразование, которое кладёт кадр натурального размера на прямоугольник миниатюры.
 * Миниатюра обрезана по `cover`, поэтому кадр покрывает прямоугольник, а лишнее срезает `clip`.
 */
export function coverRect(rect: { x: number; y: number; width: number; height: number }, natural: Size): { view: View; clip: { top: number; right: number; bottom: number; left: number } } {
  const scale = Math.max(rect.width / natural.width, rect.height / natural.height);
  const width = natural.width * scale;
  const height = natural.height * scale;
  const x = rect.x - (width - rect.width) / 2;
  const y = rect.y - (height - rect.height) / 2;
  // Срез в координатах кадра, то есть в пикселях картинки до масштаба.
  const sideX = (width - rect.width) / 2 / scale;
  const sideY = (height - rect.height) / 2 / scale;
  return { view: { scale, x, y }, clip: { top: sideY, right: sideX, bottom: sideY, left: sideX } };
}

/** Решение по горизонтальному свайпу: дальше пятой части ширины или быстрый бросок листает. */
export function swipeStep(dx: number, velocity: number, width: number): -1 | 0 | 1 {
  if (dx <= -width * 0.2 || velocity <= -0.45) return 1;
  if (dx >= width * 0.2 || velocity >= 0.45) return -1;
  return 0;
}

/** Решение по вертикальному свайпу: дальше порога или быстрый бросок закрывает. */
export function dismisses(dy: number, velocity: number, height: number): boolean {
  return Math.abs(dy) >= Math.min(140, height * 0.18) || Math.abs(velocity) >= 0.6;
}

/** Прозрачность фона при вертикальном свайпе: гаснет по мере удаления. */
export function dismissOpacity(dy: number, height: number): number {
  return clamp(1 - Math.abs(dy) / (height * 0.6), 0, 1);
}
