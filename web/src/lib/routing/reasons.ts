// Причины маршрута и очереди приходят от хаба одной английской строкой: строки
// известного вида переводятся здесь, незнакомые показываются как есть.
import { fmt } from '#lib/format/ru.js';
import { modelLabel, providerLabel } from '#lib/format/labels.js';

const NBSP = ' ';

const WINDOWS: Record<string, string> = {
  weekly: 'неделя',
  five_hour: '5 часов',
  monthly: 'месяц'
};

const UNITS: Record<string, string> = { min: 'мин', h: 'ч', d: 'дн.' };

const SESSION_STATES: Record<string, string> = {
  idle: 'простаивает',
  stopped: 'остановлена',
  failed: 'сорвалась',
  stalled: 'зависла'
};

/** Число хаба («83», «2.1») по-русски: десятичная запятая. */
function number(value: string): string {
  return value.replace('.', ',');
}

function percent(value: string): string {
  return `${number(value)}${NBSP}%`;
}

function provider(value: string): string {
  const label = providerLabel(value);
  return label === 'Другой провайдер' ? value : label;
}

function windowName(value: string): string {
  return WINDOWS[value] ?? value;
}

/**
 * «14:32 UTC» хаба во время владельца: ближайший такой момент, не дальше
 * полусуток в прошлом (хаб пишет сброс, который ещё впереди).
 */
export function utcClock(hhmm: string, now: number = Date.now()): string {
  const match = /^(\d{1,2}):(\d{2})$/.exec(hhmm);
  if (!match) return hhmm;
  const base = new Date(now);
  let at = Date.UTC(base.getUTCFullYear(), base.getUTCMonth(), base.getUTCDate(), Number(match[1]), Number(match[2]));
  if (at < now - 12 * 3_600_000) at += 86_400_000;
  return fmt.clock(at);
}

type Rule = [RegExp, (match: RegExpExecArray, now: number) => string];

const RULES: Rule[] = [
  [
    /^([a-z_]+) (weekly|five_hour|monthly) ([\d.]+)% > ([\d.]+)%$/,
    (m) => `${provider(m[1]!)} — ${windowName(m[2]!)} ${percent(m[3]!)} при пороге ${percent(m[4]!)}`
  ],
  [
    /^quota paused(?: until (\d{1,2}:\d{2}) UTC)?$/,
    (m, now) => (m[1] ? `квота на паузе до ${utcClock(m[1], now)}` : 'квота на паузе')
  ],
  [
    /^([a-z_]+) (weekly|five_hour|monthly) limits unknown(?: \(observed (\d+) (min|h|d) ago\))?$/,
    (m) =>
      `лимиты ${provider(m[1]!)} (${windowName(m[2]!)}) неизвестны` +
      (m[3] ? `: последнее наблюдение ${m[3]}${NBSP}${UNITS[m[4]!] ?? m[4]} назад` : '')
  ],
  [/^limits unknown$/, () => 'лимиты неизвестны'],
  [/^(\d+) of (\d+) (.+) sessions busy$/, (m) => `занято ${m[1]} из ${m[2]} сессий ${modelLabel(m[3]!)}`],
  [/^not in the owner's list$/, () => 'не в белом списке владельца'],
  [/^not available on this machine$/, () => 'нет на этой машине'],
  [/^excluded for workdir (.+)$/, (m) => `исключена для каталога ${m[1]}`],
  [/^level (\S+) not in candidate levels$/, (m) => `уровень ${m[1]} не входит в уровни кандидата`],
  [
    /^effort (?:Some\()?"?([^")]+)"?\)? not available for model (.+)$/,
    (m) => `усилие ${m[1]} недоступно для модели ${modelLabel(m[2]!)}`
  ],
  [/^permission mode (\S+) not available for (\S+)$/, (m) => `режим разрешений ${m[1]} недоступен для ${m[2]}`],
  [
    /^disk ([\d.]+) GB free < ([\d.]+) GB$/,
    (m) => `диск: свободно ${number(m[1]!)}${NBSP}ГБ при минимуме ${number(m[2]!)}${NBSP}ГБ`
  ],
  [/^disk ([\d.]+)% free < ([\d.]+)%$/, (m) => `диск: свободно ${percent(m[1]!)} при минимуме ${percent(m[2]!)}`],
  [/^load ([\d.]+) > ([\d.]+) per core$/, (m) => `нагрузка ${number(m[1]!)} на ядро при пороге ${number(m[2]!)}`],
  [
    /^memory ([\d.]+) GB available < ([\d.]+) GB$/,
    (m) => `память: доступно ${number(m[1]!)}${NBSP}ГБ при минимуме ${number(m[2]!)}${NBSP}ГБ`
  ],
  [
    /^(\d+) spawns? in the last hour; the next slot opens at (\d{1,2}:\d{2}) UTC$/,
    (m, now) => `${m[1]} ${fmt.plural(Number(m[1]), ['запуск', 'запуска', 'запусков'])} за час, следующий в ${utcClock(m[2]!, now)}`
  ],
  [/^the machine is not connected$/, () => 'машина не подключена'],
  [/^machine metrics stale$/, () => 'метрики машины устарели'],
  [/^nearest limit reset at (\d{1,2}:\d{2}) UTC$/, (m, now) => `ближайший сброс лимита в ${utcClock(m[1]!, now)}`]
];

/** Одна причина: известный вид по-русски, иначе исходный текст. */
export function reasonText(raw: string | null | undefined, now: number = Date.now()): string {
  const text = (raw ?? '').trim().replace(/\.$/, '');
  if (!text) return '';
  for (const [pattern, render] of RULES) {
    const match = pattern.exec(text);
    if (match) return render(match, now);
  }
  return text;
}

function cleanupText(list: string): string | null {
  const names = list.split(', ').map((item) => {
    const match = /^(.+) (idle|stopped|failed|stalled) (\d+) h$/.exec(item);
    return match ? `«${match[1]}» ${SESSION_STATES[match[2]!] ?? match[2]} ${match[3]}${NBSP}ч` : null;
  });
  if (names.some((name) => name === null)) return null;
  return `Место освободит приём результата: ${names.join(', ')}`;
}

/**
 * Строка `detail` очереди или стража: части через «; », после точки — кандидаты
 * на уборку. Каждая часть переводится отдельно.
 */
export function detailText(raw: string | null | undefined, now: number = Date.now()): string {
  const text = (raw ?? '').trim();
  if (!text) return '';
  const whole = reasonText(text, now);
  if (whole !== text.replace(/\.$/, '')) return whole;
  const cleanup = /^(.*?)\.\s+(.+): session_accept frees their worktrees and target dirs\.?$/.exec(text);
  const main = cleanup ? cleanup[1]! : text;
  const parts = main
    .replace(/\.$/, '')
    .split('; ')
    .map((part) => reasonText(part, now))
    .filter(Boolean);
  const head = parts.join('; ');
  const tail = cleanup ? cleanupText(cleanup[2]!) : null;
  if (cleanup && !tail) return text;
  return tail ? `${head}. ${tail}` : head;
}

const QUEUE_REASONS: Record<string, string> = {
  rate: 'частота запусков',
  concurrency: 'исполнители заняты',
  limits: 'лимиты провайдера',
  resources: 'машина занята'
};

/** Причина постановки в очередь: `rate`, `concurrency`, `limits`, `resources`. */
export function queueReasonLabel(reason: string): string {
  return QUEUE_REASONS[reason] ?? reason;
}

const VERDICTS: Record<string, string> = {
  chosen: 'выбран',
  skipped: 'пропущен',
  level: 'не тот уровень',
  limits_over: 'выше порога',
  limits_unknown: 'лимиты неизвестны',
  quota_paused: 'квота на паузе',
  parallel_busy: 'занят',
  excluded_workdir: 'исключён для каталога'
};

export function verdictLabel(verdict: string): string {
  return VERDICTS[verdict] ?? verdict;
}
