/** Чистая модель правки изображения: без DOM, без загрузки файлов. */

export interface Point {
  x: number;
  y: number;
}

export interface Size {
  width: number;
  height: number;
}

export interface Rect extends Point, Size {}

export type CropHandle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w";

export type MarkColor = "red" | "yellow" | "green" | "blue";

export const MARK_COLORS: readonly MarkColor[] = ["red", "yellow", "green", "blue"];

export interface Stroke {
  color: MarkColor;
  points: Point[];
}

/** Каждое действие знает, как откатить только его. */
export type EditAction = { kind: "crop"; previous: Rect } | { kind: "stroke" };

export interface Edit {
  crop: Rect;
  strokes: Stroke[];
  history: EditAction[];
}

export function createEdit(image: Size): Edit {
  return { crop: { x: 0, y: 0, width: image.width, height: image.height }, strokes: [], history: [] };
}

export function setCrop(edit: Edit, rect: Rect, bounds: Size): Edit {
  const crop = clampRect(rect, bounds);
  if (
    crop.x === edit.crop.x &&
    crop.y === edit.crop.y &&
    crop.width === edit.crop.width &&
    crop.height === edit.crop.height
  )
    return edit;
  return {
    ...edit,
    crop,
    history: [...edit.history, { kind: "crop", previous: edit.crop }],
  };
}

export function addStroke(edit: Edit, stroke: Stroke): Edit {
  return {
    ...edit,
    strokes: [...edit.strokes, stroke],
    history: [...edit.history, { kind: "stroke" }],
  };
}

export function undo(edit: Edit): Edit {
  const last = edit.history.at(-1);
  if (!last) return edit;
  const history = edit.history.slice(0, -1);
  if (last.kind === "crop") return { ...edit, crop: last.previous, history };
  return { ...edit, strokes: edit.strokes.slice(0, -1), history };
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/** Прямоугольник целиком внутри изображения, с минимальным размером. */
export function clampRect(rect: Rect, bounds: Size, minSize = 1): Rect {
  const width = clamp(rect.width, Math.min(minSize, bounds.width), bounds.width);
  const height = clamp(rect.height, Math.min(minSize, bounds.height), bounds.height);
  return {
    x: clamp(rect.x, 0, bounds.width - width),
    y: clamp(rect.y, 0, bounds.height - height),
    width,
    height,
  };
}

export function moveRect(rect: Rect, dx: number, dy: number, bounds: Size): Rect {
  return clampRect({ ...rect, x: rect.x + dx, y: rect.y + dy }, bounds);
}

/** Тянет сторону или угол: противоположная сторона остаётся на месте. */
export function resizeRect(
  rect: Rect,
  handle: CropHandle,
  dx: number,
  dy: number,
  bounds: Size,
  minSize = 1,
): Rect {
  let { x, y, width, height } = rect;
  if (handle.includes("w")) {
    const right = x + width;
    x = clamp(x + dx, 0, right - minSize);
    width = right - x;
  }
  if (handle.includes("e")) width = clamp(width + dx, minSize, bounds.width - x);
  if (handle.includes("n")) {
    const bottom = y + height;
    y = clamp(y + dy, 0, bottom - minSize);
    height = bottom - y;
  }
  if (handle.includes("s")) height = clamp(height + dy, minSize, bounds.height - y);
  return { x, y, width, height };
}

/** Вписанное в экран изображение: его прямоугольник в координатах экрана. */
export function fitImage(image: Size, viewport: Size): Rect {
  const scale = Math.min(viewport.width / image.width, viewport.height / image.height);
  const width = image.width * scale;
  const height = image.height * scale;
  return { x: (viewport.width - width) / 2, y: (viewport.height - height) / 2, width, height };
}

export function toImagePoint(point: Point, view: Rect, image: Size): Point {
  return {
    x: clamp(((point.x - view.x) * image.width) / view.width, 0, image.width),
    y: clamp(((point.y - view.y) * image.height) / view.height, 0, image.height),
  };
}

export function toScreenPoint(point: Point, view: Rect, image: Size): Point {
  return {
    x: view.x + (point.x * view.width) / image.width,
    y: view.y + (point.y * view.height) / image.height,
  };
}

/** Толщина обводки в пикселях исходника: видна и на большом снимке. */
export function strokeWidth(image: Size): number {
  return Math.max(2, Math.round(Math.min(image.width, image.height) * 0.006));
}

export const EDITABLE_MIMES: ReadonlySet<string> = new Set([
  "image/png",
  "image/jpeg",
  "image/webp",
]);

/** Предел сторона*сторона, который принимает canvas на iOS. */
export const MAX_IMAGE_PIXELS = 16_777_216;

export type SkipReason = "decode" | "too_large";

export interface SkippedFile {
  file: File;
  reason: SkipReason;
}

export interface RoutePlan {
  edit: File[];
  /** Все файлы, которые прикрепляются без редактора, включая пропущенные. */
  direct: File[];
  skipped: SkippedFile[];
}

export function routeFiles(
  files: readonly File[],
  sizes: ReadonlyMap<File, Size>,
): RoutePlan {
  const plan: RoutePlan = { edit: [], direct: [], skipped: [] };
  for (const file of files) {
    if (!EDITABLE_MIMES.has(file.type)) {
      plan.direct.push(file);
      continue;
    }
    const size = sizes.get(file);
    if (!size) {
      plan.direct.push(file);
      plan.skipped.push({ file, reason: "decode" });
    } else if (size.width * size.height > MAX_IMAGE_PIXELS) {
      plan.direct.push(file);
      plan.skipped.push({ file, reason: "too_large" });
    } else {
      plan.edit.push(file);
    }
  }
  return plan;
}

export interface OutputPlan {
  width: number;
  height: number;
  mime: string;
  name: string;
  quality?: number;
}

/** Обрезка без уменьшения: размер, тип и имя будущего файла. */
export function outputPlan(name: string, type: string, image: Size, crop: Rect): OutputPlan {
  const x = clamp(Math.round(crop.x), 0, image.width);
  const y = clamp(Math.round(crop.y), 0, image.height);
  const width = clamp(Math.round(crop.width), 1, image.width - x);
  const height = clamp(Math.round(crop.height), 1, image.height - y);
  if (type === "image/jpeg")
    return { width, height, mime: type, name, quality: 0.92 };
  if (type === "image/webp")
    return { width, height, mime: "image/png", name: name.replace(/\.[^./\\]+$/, "") + ".png" };
  return { width, height, mime: "image/png", name };
}
