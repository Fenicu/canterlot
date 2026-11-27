import {
  EDITABLE_MIMES,
  routeFiles,
  type Size,
  type SkippedFile
} from "#lib/composer/imageEdit/model.js";

/** Решение редактора для одного изображения. */
export type EditChoice =
  | { choice: "cancel" }
  | { choice: "original" }
  | { choice: "edited"; file: File };

export interface AttachmentQueueDeps {
  decode(file: File): Promise<Size | null>;
  edit(file: File): Promise<EditChoice>;
  add(file: File): void;
  note(message: string): void;
}

export function skipNotice(skipped: SkippedFile): string {
  const name = skipped.file.name;
  return skipped.reason === "decode"
    ? `Не удалось открыть «${name}»: файл прикреплён без правок`
    : `Изображение «${name}» больше 16 млн пикселей: файл прикреплён без правок`;
}

/**
 * Очередь перед загрузкой: картинка сначала проходит редактор, остальные файлы идут сразу.
 * Пакеты не перехлёстываются: окно редактора показывается по одному изображению за раз.
 */
export class AttachmentQueue {
  readonly #deps: AttachmentQueueDeps;
  #tail: Promise<void> = Promise.resolve();

  constructor(deps: AttachmentQueueDeps) {
    this.#deps = deps;
  }

  enqueue(files: FileList | File[]): void {
    const batch = Array.from(files);
    this.#tail = this.#tail.then(() => this.#run(batch)).catch(() => undefined);
  }

  idle(): Promise<void> {
    return this.#tail;
  }

  async #run(files: readonly File[]): Promise<void> {
    const images: File[] = [];
    for (const file of files) {
      if (EDITABLE_MIMES.has(file.type)) images.push(file);
      else this.#deps.add(file);
    }
    const notices: string[] = [];
    for (const image of images) {
      const size = await this.#deps.decode(image);
      const sizes = new Map<File, Size>();
      if (size) sizes.set(image, size);
      const plan = routeFiles([image], sizes);
      for (const direct of plan.direct) this.#deps.add(direct);
      for (const skipped of plan.skipped) notices.push(skipNotice(skipped));
      for (const editable of plan.edit) {
        const result = await this.#deps.edit(editable);
        if (result.choice === "edited") this.#deps.add(result.file);
        else if (result.choice === "original") this.#deps.add(editable);
      }
    }
    if (notices.length > 0) this.#deps.note(notices.join(" "));
  }
}
