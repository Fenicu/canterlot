import { fmt } from "#lib/format/ru.js";

const SPACES = /[\s\u00a0\u202f]/g;

/**
 * Сумма в копейках по-русски: «1 234,50 ₽». Копейки в API — целые: сумма не проходит через
 * плавающую точку, целая часть и остаток считаются отдельно.
 */
export function rubles(kopecks: number): string {
  if (!Number.isFinite(kopecks)) return "—";
  const value = Math.trunc(kopecks);
  const sign = value < 0 ? "-" : "";
  const abs = Math.abs(value);
  const whole = Math.floor(abs / 100);
  const rest = abs % 100;
  return `${sign}${fmt.number(whole)},${String(rest).padStart(2, "0")} ₽`;
}

/** Копейки для поля ввода: «1 234,50», без знака валюты. */
export function kopecksInput(kopecks: number): string {
  const text = rubles(kopecks);
  return text === "—" ? "" : text.slice(0, -2);
}

/**
 * Разбор суммы из поля: рубли и не больше двух знаков после запятой или точки.
 * Возвращает целые копейки; всё остальное — `null`, а не догадка.
 */
export function parseRubles(text: string): number | null {
  const cleaned = text.replace(SPACES, "").replace(/₽$/u, "");
  const match = /^(\d{1,15})(?:[.,](\d{1,2}))?$/u.exec(cleaned);
  if (!match) return null;
  const whole = Number(match[1]);
  const rest = Number((match[2] ?? "").padEnd(2, "0"));
  if (!Number.isSafeInteger(whole) || !Number.isSafeInteger(rest)) return null;
  const kopecks = whole * 100 + rest;
  return Number.isSafeInteger(kopecks) ? kopecks : null;
}
