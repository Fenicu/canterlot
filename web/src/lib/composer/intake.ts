export interface FilePayloadItem {
  readonly kind: string;
  getAsFile(): File | null;
}

export interface FilePayload {
  readonly files?: ArrayLike<File> | null;
  readonly items?: ArrayLike<FilePayloadItem | null> | null;
  readonly types?: ArrayLike<string> | null;
}

const IMAGE_TYPES: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp",
  "image/svg+xml": "svg",
  "image/avif": "avif",
  "image/heic": "heic",
  "image/heif": "heif",
  "image/tiff": "tif",
  "image/x-icon": "ico",
  "image/vnd.microsoft.icon": "ico",
};

const PLACEHOLDER = /^(?:image|screenshot|snapshot|clipboard|pasted|blob)(?:[ _-]?\(?\d+\)?)?(?:\.[a-z0-9]+)?$/i;

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

function imageExtension(type: string): string {
  const mapped = IMAGE_TYPES[type.toLowerCase()];
  if (mapped) return mapped;
  const subtype = type.toLowerCase().split("/")[1]?.split("+")[0] ?? "";
  return /^[a-z0-9]+$/.test(subtype) ? subtype : "png";
}

function placeholderName(name: string): boolean {
  const trimmed = name.trim();
  return trimmed === "" || PLACEHOLDER.test(trimmed);
}

/** Безымянный снимок из буфера получает имя по местному времени, а не «image.png». */
export function readableFileName(file: File, now: Date = new Date()): File {
  if (!file.type.startsWith("image/") || !placeholderName(file.name)) return file;
  const name = `Снимок экрана ${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())} ${pad(now.getHours())}-${pad(now.getMinutes())}.${imageExtension(file.type)}`;
  return new File([file], name, {
    type: file.type,
    lastModified: file.lastModified,
  });
}

function signature(file: File): string {
  return `${file.name}\u0000${file.size}\u0000${file.type}\u0000${file.lastModified}`;
}

/** Файлы из буфера или перетаскивания: items и files вместе, без повторов. */
export function filesFromPayload(
  payload: FilePayload | null | undefined,
  now: Date = new Date(),
): File[] {
  const found: File[] = [];
  const identities = new Set<File>();
  const fromItems = new Set<File>();
  const pending = new Map<string, number>();
  const count = (file: File, step: number) => {
    const mark = signature(file);
    pending.set(mark, (pending.get(mark) ?? 0) + step);
  };

  const items = payload?.items;
  if (items) {
    for (let index = 0; index < items.length; index++) {
      const item = items[index];
      if (item?.kind !== "file") continue;
      const file = item.getAsFile();
      if (!file || identities.has(file)) continue;
      identities.add(file);
      fromItems.add(file);
      count(file, 1);
      found.push(file);
    }
  }

  const files = payload?.files;
  if (files) {
    for (let index = 0; index < files.length; index++) {
      const file = files[index];
      if (!file) continue;
      if (fromItems.has(file)) {
        count(file, -1);
        continue;
      }
      if (identities.has(file)) continue;
      // files повторяет items: пропускаем ровно столько повторов, сколько пришло из items.
      const mark = signature(file);
      const repeats = pending.get(mark) ?? 0;
      identities.add(file);
      if (repeats > 0) {
        pending.set(mark, repeats - 1);
        continue;
      }
      found.push(file);
    }
  }

  return found.map((file) => readableFileName(file, now));
}

/** Перетаскивание файлов видно по типам ещё до того, как браузер отдаст сами файлы. */
export function hasFileDrag(payload: FilePayload | null | undefined): boolean {
  if (!payload) return false;
  const types = payload.types;
  if (types) {
    for (let index = 0; index < types.length; index++) {
      if (types[index] === "Files") return true;
    }
  }
  if (payload.files && payload.files.length > 0) return true;
  const items = payload.items;
  if (items) {
    for (let index = 0; index < items.length; index++) {
      if (items[index]?.kind === "file") return true;
    }
  }
  return false;
}
