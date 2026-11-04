/** Шаг проявления слова: длинный токен без пробелов тоже должен течь по частям. */
const UNIT_CHARS = 24;

export interface RevealUpdate {
  /** Блок больше не изменится: доиграть хвост и отдать готовую разметку. */
  final: boolean;
  now: number;
  /** Системное уменьшение движения или «Выкл» в настройках: показать всё сразу. */
  instant: boolean;
  /** Множитель скорости: 1 обычно, 1.5 медленнее, 0.5 быстрее. */
  scale?: number;
  /** За сколько доиграть хвост, когда блок стал финальным. */
  finishMs?: number;
}

function isSpace(code: number): boolean {
  return (
    code <= 32 ||
    code === 0xa0 ||
    code === 0x1680 ||
    (code >= 0x2000 && code <= 0x200a) ||
    code === 0x2028 ||
    code === 0x2029 ||
    code === 0x202f ||
    code === 0x205f ||
    code === 0x3000
  );
}

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

/**
 * Проявляет принятый текст словами: цель — весь текст блока, показанное — его
 * префикс. Отставание ограничено долей секунды, поэтому рывок дельт
 * размазывается, а не копится.
 */
export class TextReveal {
  #target = "";
  /** Концы слов: по ним отрезается показанный префикс. */
  #units: number[] = [];
  #scanned = 0;
  #runStart = -1;
  /** Последнее слово ещё пишется: его конец сдвигается вместе с текстом. */
  #open = false;
  #shownUnits = 0;
  #shownEnd = 0;
  #shown = "";
  #carry = 0;
  #last = -1;
  #instant = false;
  #final = false;
  #finishAt = 0;
  #scale = 1;

  constructor(
    readonly baseWordsPerSecond = 32,
    readonly maxLagSeconds = 0.6,
  ) {}

  get text(): string {
    return this.#shown;
  }

  get shownWords(): number {
    return this.#shownUnits;
  }

  get totalWords(): number {
    return this.#units.length;
  }

  get pendingWords(): number {
    return Math.max(0, this.#units.length - this.#shownUnits);
  }

  get done(): boolean {
    return (
      this.#instant ||
      (this.#shownUnits >= this.#units.length &&
        (!this.#final || this.#shownEnd >= this.#target.length))
    );
  }

  /** Сколько секунд придётся ждать показа последнего принятого слова при нынешнем темпе. */
  get lagSeconds(): number {
    const backlog = this.pendingWords;
    if (backlog === 0) return 0;
    return Math.min(backlog / this.#speed(), this.maxLagSeconds);
  }

  update(text: string, update: RevealUpdate): void {
    this.#scale = update.scale ?? this.#scale;
    const instant = update.instant || this.#scale <= 0;

    if (!text.startsWith(this.#target)) {
      // Снимок заменил текст: не переигрывать уже показанное.
      this.#reset(text);
      this.#scan(text);
      this.#shownUnits = this.#units.length;
      this.#shownEnd = text.length;
      this.#sync();
      this.#finish(update);
      if (instant) {
        this.#instant = true;
        this.#revealAll();
      } else if (this.#last < 0) {
        this.#last = update.now;
      }
      return;
    }

    if (text !== this.#target) {
      this.#target = text;
      this.#scan(text);
    }
    this.#instant = instant;
    if (this.#shownUnits >= this.#units.length)
      this.#shownEnd = Math.max(this.#shownEnd, this.#open ? text.length : this.#shownEnd);
    if (update.final && this.#shownUnits >= this.#units.length)
      this.#shownEnd = text.length;
    this.#finish(update);
    if (instant) {
      this.#revealAll();
      return;
    }
    // Первое слово видно в том же кадре, что и пришло: пустой блок не мигает.
    if (this.#shownUnits === 0 && this.#units.length > 0) {
      this.#shownUnits = 1;
      this.#shownEnd = this.#units[0]!;
    }
    this.#sync();
    if (this.#last < 0) this.#last = update.now;
  }

  tick(now: number): boolean {
    if (this.#instant) return false;
    const dt =
      this.#last < 0 ? 0 : Math.min(1, Math.max(0, now - this.#last) / 1000);
    this.#last = now;
    if (this.done) return false;

    if (this.#final) {
      if (now >= this.#finishAt) {
        this.#revealAll();
        return true;
      }
      const left = Math.max(1, this.#finishAt - now);
      const count = Math.ceil(
        (this.pendingWords * dt * 1000) / left,
      );
      const before = this.#shownUnits;
      this.#advance(count);
      return this.#shownUnits !== before;
    }

    const backlog = this.pendingWords;
    // Отставание больше предела ускоряет темп ровно настолько, чтобы вернуться в него.
    const speed = Math.max(this.#speed(), backlog / this.maxLagSeconds);
    this.#carry += speed * dt;
    const count = Math.floor(this.#carry);
    if (count <= 0) return false;
    this.#carry -= count;
    const before = this.#shownUnits;
    this.#advance(count);
    return this.#shownUnits !== before;
  }

  #speed(): number {
    return this.baseWordsPerSecond / Math.max(0.01, this.#scale);
  }

  #finish(update: RevealUpdate): void {
    const wasFinal = this.#final;
    this.#final = update.final;
    if (!update.final || this.done) return;
    if (!wasFinal)
      this.#finishAt = update.now + Math.max(0, update.finishMs ?? 180);
  }

  #advance(count: number): void {
    const next = Math.min(
      this.#units.length,
      this.#shownUnits + Math.max(1, count),
    );
    if (next === this.#shownUnits) return;
    this.#shownUnits = next;
    this.#shownEnd = Math.max(
      this.#shownEnd,
      this.#units[next - 1] ?? this.#shownEnd,
    );
    this.#sync();
  }

  #revealAll(): void {
    this.#shownUnits = this.#units.length;
    this.#shownEnd = this.#target.length;
    this.#sync();
  }

  #sync(): void {
    this.#shown = this.#target.slice(
      0,
      Math.min(this.#shownEnd, this.#target.length),
    );
  }

  #reset(text: string): void {
    this.#target = text;
    this.#units = [];
    this.#scanned = 0;
    this.#runStart = -1;
    this.#open = false;
    this.#shownUnits = 0;
    this.#shownEnd = 0;
    this.#shown = "";
    this.#carry = 0;
    this.#last = -1;
    this.#instant = false;
    this.#final = false;
    this.#finishAt = 0;
  }

  #scan(text: string): void {
    for (let index = this.#scanned; index < text.length; index++) {
      const code = text.charCodeAt(index);
      if (isSpace(code)) {
        if (this.#runStart >= 0 && index > this.#runStart)
          this.#emit(index);
        this.#runStart = -1;
        continue;
      }
      if (this.#runStart < 0) this.#runStart = index;
      if (index + 1 - this.#runStart >= UNIT_CHARS) {
        if (isHighSurrogate(code)) continue;
        this.#emit(index + 1);
        this.#runStart = index + 1;
      }
    }
    if (this.#runStart >= 0 && text.length > this.#runStart)
      this.#emit(text.length, true);
    this.#scanned = text.length;
  }

  /** Конец слова; открытое слово переписывает свой конец, а не добавляет новое. */
  #emit(end: number, open = false): void {
    if (this.#open) this.#units[this.#units.length - 1] = end;
    else this.#units.push(end);
    this.#open = open;
  }
}

const WORD = /\S+/g;
const BLOCK_CLOSE = /<\/(?:p|li|h[1-6]|pre|td|th|dt|dd)>/gi;
const CARET = '<span class="caret" aria-hidden="true"></span>';

/**
 * Ставит мигающую каретку в конец последнего блока markdown-хвоста: отдельный
 * inline-block после списка или кода занимал бы лишнюю строку.
 */
export function caretInTail(html: string): string {
  if (html.length === 0) return html;
  let at = -1;
  for (const match of html.matchAll(BLOCK_CLOSE)) at = match.index ?? at;
  if (at < 0) return html + CARET;
  return html.slice(0, at) + CARET + html.slice(at);
}

/**
 * Оборачивает только последние слова готового HTML хвоста: показанное не
 * пересобирается, а новое получает CSS-анимацию появления.
 */
export function markRevealWords(html: string, count: number): string {
  if (count <= 0 || html.length === 0) return html;
  const parts: { tag: boolean; value: string }[] = [];
  let start = 0;
  for (let index = 0; index < html.length; index++) {
    if (html[index] !== "<") continue;
    if (index > start)
      parts.push({ tag: false, value: html.slice(start, index) });
    let quote = "";
    let end = index + 1;
    for (; end < html.length; end++) {
      const char = html[end]!;
      if (quote) {
        if (char === quote) quote = "";
      } else if (char === '"' || char === "'") {
        quote = char;
      } else if (char === ">") {
        end++;
        break;
      }
    }
    parts.push({ tag: true, value: html.slice(index, end) });
    index = end - 1;
    start = end;
  }
  if (start < html.length)
    parts.push({ tag: false, value: html.slice(start) });

  let total = 0;
  for (const part of parts) {
    if (part.tag) continue;
    total += part.value.match(WORD)?.length ?? 0;
  }
  if (total === 0) return html;

  const first = Math.max(0, total - count);
  let seen = 0;
  let out = "";
  for (const part of parts) {
    if (part.tag) {
      out += part.value;
      continue;
    }
    let cursor = 0;
    for (const match of part.value.matchAll(WORD)) {
      out += part.value.slice(cursor, match.index);
      out +=
        seen >= first
          ? `<span class="reveal-word">${match[0]}</span>`
          : match[0];
      cursor = (match.index ?? 0) + match[0].length;
      seen++;
    }
    out += part.value.slice(cursor);
  }
  return out;
}
