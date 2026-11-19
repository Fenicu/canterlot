// Чистые помощники экрана «Расписание»: cron, человекочитаемый текст, исходы и хвост предпроверки.
// Никаких обращений к API и Svelte: всё доступно юнит-тестам и редактору.
import type { components } from "#lib/api/generated/openapi.js";

export type Schedule = components["schemas"]["Schedule"];
export type ScheduleRun = components["schemas"]["ScheduleRun"];
export type ScheduleTarget = components["schemas"]["ScheduleTarget"];
export type RunState = components["schemas"]["RunState"];

export const CRON_PRESETS = [
  { id: "daily", label: "Каждый день", cron: "0 9 * * *" },
  { id: "weekdays", label: "По будням", cron: "0 9 * * 1-5" },
  { id: "hourly", label: "Каждый час", cron: "0 * * * *" }
] as const;

export const OVERLAP_OPTIONS = [
  { value: "skip", label: "Пропускать пересечения" },
  { value: "allow", label: "Разрешить пересечения" }
] as const;

export type Tone = "ok" | "warn" | "danger" | "neutral";

const FIELDS: [number, number][] = [[0, 59], [0, 23], [1, 31], [1, 12], [0, 7]];

interface Cron {
  minute: number[];
  hour: number[];
  dom: number[];
  month: number[];
  dow: number[];
}

const WEEKDAYS = [
  "воскресеньям", "понедельникам", "вторникам", "средам",
  "четвергам", "пятницам", "субботам"
];

function inside(value: number, min: number, max: number): boolean {
  return Number.isInteger(value) && value >= min && value <= max;
}

/** Пятиполевый cron с минутным разрешением: `*`, шаги, числа, диапазоны и списки. */
export function parseCron(cron: string): Cron | null {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return null;
  const result: number[][] = [];
  for (let index = 0; index < 5; index++) {
    const [min, max] = FIELDS[index]!;
    const field = parts[index]!;
    const values = new Set<number>();
    if (field === "*") {
      for (let value = min; value <= max; value++) values.add(value);
    } else {
      let valid = true;
      for (const token of field.split(",")) {
        const stepped = token.match(/^(\*|\d+|\d+-\d+)\/(\d+)$/);
        const whole = token.match(/^\d+$/);
        const range = token.match(/^(\d+)-(\d+)$/);
        if (stepped) {
          const stride = Number(stepped[2]);
          if (!Number.isInteger(stride) || stride < 1) { valid = false; break; }
          const base = stepped[1]!;
          let from = min;
          let to = max;
          if (base !== "*") {
            const edges = base.split("-").map(Number);
            from = edges[0]!;
            to = edges.length === 2 ? edges[1]! : max;
          }
          if (!inside(from, min, max) || !inside(to, min, max) || from > to) { valid = false; break; }
          for (let value = from; value <= to; value += stride) values.add(value);
        } else if (whole) {
          const value = Number(token);
          if (!inside(value, min, max)) { valid = false; break; }
          values.add(value);
        } else if (range) {
          const from = Number(range[1]);
          const to = Number(range[2]);
          if (!inside(from, min, max) || !inside(to, min, max) || from > to) { valid = false; break; }
          for (let value = from; value <= to; value++) values.add(value);
        } else {
          valid = false;
          break;
        }
      }
      if (!valid) return null;
    }
    if (values.size === 0) return null;
    result.push([...values].sort((a, b) => a - b));
  }
  return {
    minute: result[0]!, hour: result[1]!, dom: result[2]!, month: result[3]!, dow: result[4]!
  };
}

function full(values: number[], min: number, max: number): boolean {
  return values.length === max - min + 1;
}

function stepOf(values: number[], min: number): number | null {
  if (values.length < 2 || values[0] !== min) return null;
  const stride = values[1]! - min;
  if (stride < 2) return null;
  return values.every((value, index) => value === min + index * stride) ? stride : null;
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

function dayText(cron: Cron): string {
  const dow = [...new Set(cron.dow.map((value) => value === 7 ? 0 : value))].sort((a, b) => a - b);
  if (dow.length === 5 && dow.every((value) => value >= 1 && value <= 5)) return "По будням";
  if (dow.length === 2 && dow.includes(0) && dow.includes(6)) return "По выходным";
  if (dow.length === 1) return `По ${WEEKDAYS[dow[0]!]}`;
  if (full(cron.dom, 1, 31)) return "Каждый день";
  if (cron.dom.length === 1) return `Каждого ${cron.dom[0]}-го`;
  const domStep = stepOf(cron.dom.map((value) => value - 1), 0);
  if (domStep) return `Каждые ${domStep} дн.`;
  return "По расписанию";
}

/** Короткая строка для списка и редактора, без знаков и пиктограмм. */
export function cronHuman(cron: string): string {
  const preset = CRON_PRESETS.find((item) => item.cron === cron.trim());
  if (preset) {
    if (preset.id === "hourly") return "Каждый час";
    return `${preset.label} в ${pad(Number(preset.cron.split(" ")[1]))}:00`;
  }
  const parsed = parseCron(cron);
  if (!parsed) return "Некорректное расписание";
  if (full(parsed.minute, 0, 59)) return "Каждую минуту";
  const minuteStep = stepOf(parsed.minute, 0);
  if (minuteStep) return `Каждые ${minuteStep} мин.`;
  const day = dayText(parsed);
  const minutes = parsed.minute.map(pad);
  let when: string;
  if (full(parsed.hour, 0, 23)) {
    when = minutes.length === 1 && minutes[0] === "00" ? "" : `в ${Number(minutes[0])} мин. каждого часа`;
  } else if (parsed.hour.length <= 3) {
    when = `в ${parsed.hour.map((hour) => `${pad(hour)}:${minutes[0]}`).join(", ")}`;
  } else {
    when = `с ${pad(parsed.hour[0]!)}:00 до ${pad(parsed.hour.at(-1)!)}:00`;
  }
  return [day, when].filter(Boolean).join(" ");
}

export const OUTCOMES: Record<RunState, { word: string; tone: Tone }> = {
  claimed: { word: "Заявка", tone: "neutral" },
  prechecking: { word: "Предпроверка", tone: "neutral" },
  started: { word: "Запущено", tone: "ok" },
  skipped_precheck: { word: "Пропущено: предпроверка", tone: "warn" },
  skipped_limits: { word: "Пропущено: лимиты", tone: "warn" },
  skipped_overlap: { word: "Пропущено: пересечение", tone: "warn" },
  skipped_machine_offline: { word: "Пропущено: машина не в сети", tone: "warn" },
  skipped_changed: { word: "Пропущено: расписание изменено", tone: "warn" },
  missed: { word: "Пропущено: время ушло", tone: "warn" },
  failed: { word: "Ошибка", tone: "danger" }
};

/** Слово статуса: без значков, только текст и тон для цвета. */
export function outcomeWord(state: RunState): { word: string; tone: Tone } {
  return OUTCOMES[state] ?? { word: "Неизвестно", tone: "neutral" };
}

export interface RunDetails {
  code: number | null;
  tail: string;
  window: string;
  missedBefore: number;
  runAnyway: boolean;
  reason: string;
  /** Текст отчёта рутины: хаб хранит его в истории запусков 30 дней. */
  reportText: string;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** `detail` от хаба — свободный объект: читаем известные ключи, не падая на неизвестных. */
export function runDetails(raw: unknown): RunDetails {
  const value = raw && typeof raw === "object" ? raw as Record<string, unknown> : {};
  const code = value.precheck_exit_code ?? value.exit_code;
  const missed = value.missed_before;
  const report = value.report && typeof value.report === "object" ? value.report as Record<string, unknown> : {};
  return {
    code: typeof code === "number" && Number.isInteger(code) ? code : null,
    tail: text(value.precheck_tail ?? value.output_tail ?? value.tail),
    window: text(value.limits_window ?? value.window),
    missedBefore: typeof missed === "number" && Number.isInteger(missed) && missed > 0 ? missed : 0,
    runAnyway: value.run_anyway === true,
    reason: text(value.reason ?? value.error ?? value.detail ?? value.failure_reason),
    reportText: text(report.text)
  };
}

export const FALLBACK_TIMEZONE = "Europe/Moscow";

function validZone(zone: string | null | undefined): zone is string {
  if (!zone) return false;
  try {
    new Intl.DateTimeFormat("ru-RU", { timeZone: zone });
    return true;
  } catch {
    return false;
  }
}

/** Пояс нового расписания: пояс владельца, затем пояс браузера, затем Москва. */
export function defaultTimezone(owner?: string | null, browser?: string | null): string {
  if (validZone(owner)) return owner;
  if (validZone(browser)) return browser;
  return FALLBACK_TIMEZONE;
}

/** Время срабатывания в поясе расписания (или в местном, если пояс не задан). */
export function fireTime(iso: string, timeZone?: string): string {
  const date = new Date(iso);
  if (!Number.isFinite(date.getTime())) return "—";
  try {
    return new Intl.DateTimeFormat("ru-RU", {
      day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", hourCycle: "h23",
      ...(timeZone ? { timeZone } : {})
    }).format(date);
  } catch {
    return new Intl.DateTimeFormat("ru-RU", {
      day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", hourCycle: "h23"
    }).format(date);
  }
}

const TARGET_KINDS: Record<ScheduleTarget["kind"], string> = {
  new_session: "Новая сессия",
  existing_session: "Сообщение в сессию",
  session_report: "Отчёт из сессии"
};

/** Вид цели и имя сессии для списка и карточки расписания. */
export function targetLabel(
  target: ScheduleTarget,
  sessions: ReadonlyMap<string, { title: string }>
): { kind: string; session: string | null } {
  const kind = TARGET_KINDS[target.kind];
  if (target.kind === "new_session") return { kind, session: null };
  return { kind, session: sessions.get(target.conversation_id)?.title ?? "Сессия недоступна" };
}
