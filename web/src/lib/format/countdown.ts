// Сколько осталось до момента: сброс окна лимита или исчерпание по прогнозу.
// Один разбор длительности на три вида текста: обычный, сжатый для свёрнутой полосы
// и полный для чтения вслух. Прошедший момент даёт пустую строку.
import { fmt } from '#lib/format/ru.js';

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
/** С двух суток счёт идёт днями: «через 47 ч» ещё читается, «через 70 ч» уже нет. */
const DAYS_FROM = 48 * HOUR;
const NBSP = ' ';

type Parts = { days: number; hours: number; minutes: number } | 'soon' | null;

/**
 * Разбор с округлением вниз: «через 59 мин» показывается, пока до часа не хватает хотя бы секунды.
 * Прогноз грубее: часы и дни до ближайшего целого, минуты ему ни к чему.
 */
function parts(ms: number, coarse = false): Parts {
  if (!Number.isFinite(ms) || ms <= 0) return null;
  if (ms < MINUTE) return 'soon';
  if (ms < HOUR) return { days: 0, hours: 0, minutes: Math.floor(ms / MINUTE) };
  if (coarse) {
    const hours = Math.round(ms / HOUR);
    if (hours < 48) return { days: 0, hours, minutes: 0 };
    return { days: Math.floor(hours / 24), hours: hours % 24, minutes: 0 };
  }
  if (ms < DAYS_FROM) {
    const minutes = Math.floor(ms / MINUTE);
    return { days: 0, hours: Math.floor(minutes / 60), minutes: minutes % 60 };
  }
  const hours = Math.floor(ms / HOUR);
  return { days: Math.floor(hours / 24), hours: hours % 24, minutes: 0 };
}

function join(p: { days: number; hours: number; minutes: number }, units: [string, string, string], gap: string, sep: string): string {
  const out: string[] = [];
  if (p.days) out.push(`${p.days}${gap}${units[0]}`);
  if (p.hours) out.push(`${p.hours}${gap}${units[1]}`);
  if (p.minutes) out.push(`${p.minutes}${gap}${units[2]}`);
  return out.join(sep);
}

/** «2 ч 15 мин», «25 мин», «3 д 4 ч»; меньше минуты — «меньше минуты»; прошедшее — пусто. */
export function spanText(ms: number, coarse = false): string {
  const p = parts(ms, coarse);
  if (p === null) return '';
  if (p === 'soon') return 'меньше минуты';
  return join(p, ['д', 'ч', 'мин'], NBSP, ' ');
}

/** «через 2 ч 15 мин»; под минуту — «меньше минуты» без «через»; прошедшее — пусто. */
export function untilText(ms: number, coarse = false): string {
  const span = spanText(ms, coarse);
  if (!span || span === 'меньше минуты') return span;
  return `через ${span}`;
}

/** Сжатый вид для свёрнутой полосы: «2ч15м», «25м», «3д4ч», «<1м». */
export function untilShort(ms: number): string {
  const p = parts(ms);
  if (p === null) return '';
  if (p === 'soon') return '<1м';
  return join(p, ['д', 'ч', 'м'], '', '');
}

/** Полные слова с верными формами числа для чтения вслух: «2 часа 15 минут», «1 день». */
export function spanLong(ms: number, coarse = false): string {
  const p = parts(ms, coarse);
  if (p === null) return '';
  if (p === 'soon') return 'меньше минуты';
  const out: string[] = [];
  if (p.days) out.push(`${p.days} ${fmt.plural(p.days, ['день', 'дня', 'дней'])}`);
  if (p.hours) out.push(`${p.hours} ${fmt.plural(p.hours, ['час', 'часа', 'часов'])}`);
  if (p.minutes) out.push(`${p.minutes} ${fmt.plural(p.minutes, ['минуту', 'минуты', 'минут'])}`);
  return out.join(' ');
}

/** «через 2 часа 15 минут»; под минуту — «меньше чем через минуту». */
export function untilLong(ms: number, coarse = false): string {
  const span = spanLong(ms, coarse);
  if (!span) return '';
  return span === 'меньше минуты' ? 'меньше чем через минуту' : `через ${span}`;
}

/** «сброс через 2 ч 15 мин»; под минуту — «сброс меньше чем через минуту»; без сброса — пусто. */
export function resetPhrase(until: string): string {
  if (!until) return '';
  return until === 'меньше минуты' ? 'сброс меньше чем через минуту' : `сброс ${until}`;
}
