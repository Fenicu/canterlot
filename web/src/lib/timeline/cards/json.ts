/**
 * JSON вызова и ответа инструмента. История отдаёт только первые 4 КиБ текста вызова,
 * поэтому длинный бриф или список задач приходят оборванными: разбор берёт всё,
 * что успело прийти целиком, и отмечает, что хвоста нет.
 */

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };

export interface Parsed {
  value: Json | undefined;
  /** Текст кончился раньше значения: хвост отрезан. */
  partial: boolean;
}

class Cut extends Error {}

class Reader {
  at = 0;
  partial = false;
  constructor(readonly text: string) {}

  space(): void {
    while (this.at < this.text.length && /\s/.test(this.text[this.at]!)) this.at++;
  }

  peek(): string {
    this.space();
    if (this.at >= this.text.length) throw new Cut();
    return this.text[this.at]!;
  }

  value(): Json {
    const char = this.peek();
    if (char === '{') return this.object();
    if (char === '[') return this.array();
    if (char === '"') return this.string();
    return this.literal();
  }

  object(): JsonObject {
    this.at++;
    const out: JsonObject = {};
    try {
      if (this.peek() === '}') {
        this.at++;
        return out;
      }
      for (;;) {
        if (this.peek() !== '"') throw new SyntaxError('key');
        const key = this.string(true);
        if (this.peek() !== ':') throw new SyntaxError('colon');
        this.at++;
        let value: Json;
        try {
          value = this.value();
        } catch (error) {
          // Оборванная строка — всё же начало значения: начало брифа лучше, чем ничего.
          if (error instanceof CutString) out[key] = error.head;
          throw error;
        }
        out[key] = value;
        const next = this.peek();
        this.at++;
        if (next === '}') return out;
        if (next !== ',') throw new SyntaxError('comma');
      }
    } catch (error) {
      if (error instanceof Cut) {
        this.partial = true;
        return out;
      }
      throw error;
    }
  }

  array(): Json[] {
    this.at++;
    const out: Json[] = [];
    try {
      if (this.peek() === ']') {
        this.at++;
        return out;
      }
      for (;;) {
        // Недописанный объект в списке остаётся своими полями, недописанная строка — нет.
        out.push(this.value());
        const next = this.peek();
        this.at++;
        if (next === ']') return out;
        if (next !== ',') throw new SyntaxError('comma');
      }
    } catch (error) {
      if (error instanceof Cut) {
        this.partial = true;
        return out;
      }
      throw error;
    }
  }

  string(key = false): string {
    this.at++;
    let out = '';
    while (this.at < this.text.length) {
      const char = this.text[this.at]!;
      if (char === '"') {
        this.at++;
        return out;
      }
      if (char === '\\') {
        const next = this.text[this.at + 1];
        if (next === undefined) break;
        if (next === 'u') {
          const hex = this.text.slice(this.at + 2, this.at + 6);
          if (hex.length < 4) break;
          out += String.fromCharCode(Number.parseInt(hex, 16));
          this.at += 6;
          continue;
        }
        out += ({ n: '\n', t: '\t', r: '\r', b: '\b', f: '\f' } as Record<string, string>)[next] ?? next;
        this.at += 2;
        continue;
      }
      out += char;
      this.at++;
    }
    this.partial = true;
    if (key) throw new Cut();
    throw new CutString(out);
  }

  literal(): Json {
    const match = /^(?:true|false|null|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(this.text.slice(this.at));
    if (!match) {
      // Обрыв посреди литерала: `tru`, `-1`.
      if (/^[tfn\d-][a-z\d.eE+-]*$/.test(this.text.slice(this.at).trimEnd())) throw new Cut();
      throw new SyntaxError('literal');
    }
    this.at += match[0].length;
    if (this.at >= this.text.length && /^-?\d/.test(match[0])) {
      // Число в самом конце могло оборваться: доверять ему нельзя.
      throw new Cut();
    }
    const word = match[0];
    return word === 'true' ? true : word === 'false' ? false : word === 'null' ? null : Number(word);
  }
}

class CutString extends Cut {
  constructor(readonly head: string) {
    super();
  }
}

/** Разбирает JSON, допуская оборванный хвост. Мусор посреди текста — `undefined`. */
export function parseJson(text: string | null | undefined): Parsed {
  const raw = (text ?? '').trim();
  if (!raw) return { value: undefined, partial: false };
  try {
    return { value: JSON.parse(raw) as Json, partial: false };
  } catch {
    // Дальше — разбор оборванного текста.
  }
  const reader = new Reader(raw);
  try {
    const value = reader.value();
    reader.space();
    // После значения ещё что-то есть: это не оборванный JSON, а другой текст.
    if (reader.at < raw.length && !reader.partial) return { value: undefined, partial: false };
    return { value, partial: reader.partial };
  } catch {
    return { value: undefined, partial: false };
  }
}

/** Первый объект JSON внутри текста ответа: перед ним бывает подпись, после — подсказка хаба. */
export function jsonObjectIn(text: string | null | undefined): JsonObject | null {
  const raw = text ?? '';
  const start = raw.indexOf('{');
  if (start < 0) return null;
  const end = raw.lastIndexOf('}');
  for (const candidate of end > start ? [raw.slice(start, end + 1), raw.slice(start)] : [raw.slice(start)]) {
    const parsed = parseJson(candidate);
    if (isObject(parsed.value)) return parsed.value;
  }
  return null;
}

export function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function str(value: Json | undefined): string {
  return typeof value === 'string' ? value : '';
}

export function num(value: Json | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function bool(value: Json | undefined): boolean | null {
  return typeof value === 'boolean' ? value : null;
}

export function list(value: Json | undefined): Json[] {
  return Array.isArray(value) ? value : [];
}

export function obj(value: Json | undefined): JsonObject {
  return isObject(value) ? value : {};
}

export function strings(value: Json | undefined): string[] {
  return list(value).filter((item): item is string => typeof item === 'string');
}
