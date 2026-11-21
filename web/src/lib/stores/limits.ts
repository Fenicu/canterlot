import { fmt } from "#lib/format/ru.js";
import { providerLabel } from "#lib/format/labels.js";
import { spanText, untilLong, untilShort, untilText } from "#lib/format/countdown.js";

/** Прогноз хаба по средней скорости расхода окна; нет поля — данных для прогноза мало. */
export interface WindowForecast {
  rate_permille_per_hour: number;
  recent_rate_permille_per_hour?: number | null;
  exhausts_at?: string | null;
  before_reset: boolean;
}

export interface LimitWindow {
  used_permille: number;
  resets_at: string | null;
  observed_at: string;
  forecast?: WindowForecast | null;
}

export type ProviderStatusLevel = "ok" | "degraded" | "maintenance" | "partial_outage" | "major_outage" | "unknown";

/** Публичный статус провайдера со страницы statuspage, которую опрашивает хаб. */
export interface ProviderStatus {
  level: ProviderStatusLevel;
  description: string;
  components: string[];
  incidents: { name: string; url: string | null }[];
  url: string;
  fetched_at: string | null;
  changed_at: string;
  stale: boolean;
}

/** Учётная запись лимитов по умолчанию: ключ строки у неё остаётся прежним, просто провайдером. */
export const OWNER_ACCOUNT = "owner";

export function accountOf(item: { account?: string | null }): string {
  return item.account || OWNER_ACCOUNT;
}

/** Ключ строки лимитов: пара (провайдер, учётная запись); у `owner` — сам провайдер. */
export function limitKey(provider: string, account?: string | null): string {
  return account && account !== OWNER_ACCOUNT ? `${provider}@${account}` : provider;
}

/** Порядок записей одного провайдера: `owner` первой, остальные по имени. */
export function accountRank(item: { account?: string | null }): string {
  const account = accountOf(item);
  return account === OWNER_ACCOUNT ? "" : account;
}

/** Провайдеры с несколькими учётными записями: только их строки подписываются именем записи. */
export function multiAccountProviders(items: { provider: string; account?: string | null }[]): Set<string> {
  const accounts = new Map<string, Set<string>>();
  for (const item of items) {
    const known = accounts.get(item.provider) ?? new Set<string>();
    known.add(accountOf(item));
    accounts.set(item.provider, known);
  }
  return new Set([...accounts].filter(([, known]) => known.size > 1).map(([provider]) => provider));
}

/** «OpenCode · b-opencode» у провайдера с несколькими записями, иначе просто «OpenCode». */
export function accountLabel(provider: string, account: string | null | undefined, multi: ReadonlySet<string>): string {
  const name = providerLabel(provider);
  return multi.has(provider) ? `${name} · ${account || OWNER_ACCOUNT}` : name;
}

/** Вход провайдера на одной машине учётной записи. */
export interface MachineCredential {
  machine_id: string;
  machine_name: string;
  expires_at: string | null;
}

/** Сбросы лимитов, о которых сообщила одна машина учётной записи. */
export interface MachineCredits extends ProviderCredits {
  machine_id: string;
  machine_name: string;
}

export interface ProviderLimits {
  provider: string;
  /** Учётная запись лимитов; нет поля или `owner` — запись по умолчанию. */
  account?: string;
  five_hour?: LimitWindow | null;
  weekly?: LimitWindow | null;
  weekly_fable?: LimitWindow | null;
  monthly?: LimitWindow | null;
  weekly_reserve_active: boolean;
  weekly_reserve_resets_at?: string | null;
  /** У провайдера нет пятичасового окна (план без этого лимита): счётчик не показывается. */
  five_hour_absent?: boolean;
  /** `estimated`, когда CodexBar вывел расход из локальных журналов, а не от провайдера. */
  data_confidence?: "exact" | "estimated";
  /** Статус провайдера; `null`, если у провайдера нет страницы статуса или хаб её ещё не читал. */
  status?: ProviderStatus | null;
  /** Самый ранний конец входа провайдера на машинах владельца; `null` — дата неизвестна. */
  credential_expires_at?: string | null;
  /** Сбросы лимитов Codex с самой свежей машины; `null`/нет — провайдер о них молчит. */
  credits?: ProviderCredits | null;
  /** Конец входа по машинам: у каждой машины он свой и в одну дату не сливается. */
  machine_credentials?: MachineCredential[];
  /** Сбросы лимитов по машинам. */
  machine_credits?: MachineCredits[];
}

/** Сброс лимитов, подаренный провайдером: заголовок и срок, когда он сгорит. */
export interface ResetCredit {
  title: string | null;
  expires_at: string | null;
}

export interface ProviderCredits {
  reset_count: number | null;
  resets: ResetCredit[];
  observed_at: string;
}

/** За сколько до сгорания сброса полоса предупреждает. */
export const CREDITS_WARN_MS = 7 * 24 * 60 * 60 * 1000;

/** Заголовки подарков, которые провайдер называет по-английски. */
const CREDIT_TITLES: Record<string, string> = {
  "Full reset (Weekly + 5 hr)": "Полный сброс: неделя и 5 часов",
};

/** Заголовок сброса словами; без заголовка — «Сброс лимитов». */
export function creditTitle(title: string | null | undefined): string {
  if (!title) return "Сброс лимитов";
  return CREDIT_TITLES[title] ?? title;
}

/** Сбросы в порядке показа: каждый со своим заголовком и сроком; `expiresAt` — `null`, если не сгорает. */
export interface CreditEntry {
  title: string;
  expiresAt: number | null;
  /** Машина, чьи это сбросы, когда у учётной записи их несколько; иначе нет поля. */
  machine?: string;
}

export function creditEntries(credits: ProviderCredits | null | undefined, machine?: string): CreditEntry[] {
  if (!credits) return [];
  return credits.resets.map((credit) => {
    const at = credit.expires_at ? Date.parse(credit.expires_at) : Number.NaN;
    return {
      title: creditTitle(credit.title),
      expiresAt: Number.isFinite(at) ? at : null,
      ...(machine ? { machine } : {}),
    };
  });
}

/** Подпись с машиной: «Claude (machine-b)». */
function onMachine(label: string, machine: string | undefined): string {
  return machine ? `${label} (${machine})` : label;
}

/** Сбросы учётной записи по машинам; одна машина (или сводное поле старого хаба) — без имени машины. */
function creditSources(item: ProviderLimits): { machine?: string; credits: ProviderCredits }[] {
  const perMachine = item.machine_credits ?? [];
  if (perMachine.length === 0) return item.credits ? [{ credits: item.credits }] : [];
  const named = perMachine.length > 1;
  return perMachine.map((credits) => ({ credits, ...(named ? { machine: credits.machine_name } : {}) }));
}

/** Каждый сброс учётной записи со сроком; у нескольких машин — с именем машины. */
export function accountCreditEntries(item: ProviderLimits): CreditEntry[] {
  return creditSources(item).flatMap((source) => creditEntries(source.credits, source.machine));
}

/** Строки о сбросах: по одной на машину, у одной машины — одна строка без её имени. */
export function accountCreditLines(item: ProviderLimits, label: string, now: number): CreditsLine[] {
  return creditSources(item).flatMap((source) => {
    const line = creditsLine(item.provider, source.credits, now, onMachine(label, source.machine));
    return line ? [line] : [];
  });
}

/** Строки о конце входа: по одной на машину с известной датой; у одной машины без имени машины. */
export function accountCredentialLines(item: ProviderLimits, label: string, now: number): CredentialLine[] {
  const perMachine = item.machine_credentials ?? [];
  if (perMachine.length === 0) {
    const line = credentialLine(item.provider, item.credential_expires_at, now, label);
    return line ? [line] : [];
  }
  const named = perMachine.length > 1;
  return perMachine.flatMap((credential) => {
    const line = credentialLine(
      item.provider,
      credential.expires_at,
      now,
      onMachine(label, named ? credential.machine_name : undefined),
    );
    return line ? [line] : [];
  });
}

/** Ближайшее сгорание из списка; `null` — сроков нет. */
export function nearestCredit(credits: ProviderCredits | null | undefined): number | null {
  const times = creditEntries(credits).flatMap((entry) =>
    entry.expiresAt === null ? [] : [entry.expiresAt],
  );
  return times.length ? Math.min(...times) : null;
}

/** Строка о сбросах под полосой; `null` — сбросов нет или провайдер о них молчит. */
export interface CreditsLine {
  text: string;
  tone: MeterTone;
}

/**
 * «Codex: сбросы лимитов 2, ближайший сгорит 02.12»: счёт берётся у провайдера, а срок — самый
 * ранний из названных. Ближе недели до сгорания — предупреждение. Сбросы есть только у Codex.
 */
export function creditsLine(
  provider: string,
  credits: ProviderCredits | null | undefined,
  now: number,
  name?: string,
): CreditsLine | null {
  if (provider !== "codex" || !credits) return null;
  const count = credits.reset_count ?? 0;
  if (count <= 0) return null;
  const nearest = nearestCredit(credits);
  const label = name ?? providerLabel(provider);
  if (nearest === null) return { text: `${label}: сбросы лимитов ${count}`, tone: "normal" };
  const burning = nearest - now <= CREDITS_WARN_MS;
  return {
    text: `${label}: сбросы лимитов ${count}, ближайший сгорит ${fmt.shortDate(nearest)}`,
    tone: burning ? "warning" : "normal",
  };
}

/** За сколько до конца входа провайдера полоса предупреждает. */
export const CREDENTIAL_WARN_MS = 7 * 24 * 60 * 60 * 1000;

export interface CredentialLine {
  /** «Вход Claude до 12.12» или предупреждение словами. */
  text: string;
  tone: MeterTone;
}

/**
 * Строка о конце входа: дата далеко — спокойная подпись, ближе недели — предупреждение,
 * дата прошла — вход, вероятно, закончился. Ждёт местную дату, как остальные подсказки.
 */
export function credentialLine(
  provider: string,
  expiresAt: string | null | undefined,
  now: number,
  name?: string,
): CredentialLine | null {
  if (!expiresAt) return null;
  const at = Date.parse(expiresAt);
  if (!Number.isFinite(at)) return null;
  const label = name ?? providerLabel(provider);
  if (at < now) return { text: `Вход ${label}, вероятно, закончился — войдите заново`, tone: "danger" };
  if (at - now <= CREDENTIAL_WARN_MS)
    return { text: `Вход ${label} заканчивается ${fmt.shortDate(at)} — войдите заново`, tone: "warning" };
  return { text: `Вход ${label} до ${fmt.shortDate(at)}`, tone: "normal" };
}

export interface LimitThresholds {
  stale_after_secs: number;
  /** Устарело: первый порог красил полосу до появления остатка в конфиге. */
  warn_pcts: number[];
  /** Действующий резерв как расход, %: совместимо с прежним полем. */
  weekly_reserve_pct: number;
  /** Жёлтый, когда остаток меньше этого процента. */
  warn_remaining_pct: number;
  /** Красный, когда остаток меньше этого процента. */
  danger_remaining_pct: number;
  /** Сколько недельного остатка оставить себе; `0` — без резерва. */
  reserve_remaining_pct: number;
}

export type MeterTone = "normal" | "warning" | "danger";

/** Умолчания, пока конфиг хаба не загружен: те же значения, что у `/api/v1/config`. */
export const DEFAULT_LIMIT_THRESHOLDS: LimitThresholds = {
  stale_after_secs: 2100,
  warn_pcts: [80, 90, 95],
  weekly_reserve_pct: 95,
  warn_remaining_pct: 20,
  danger_remaining_pct: 5,
  reserve_remaining_pct: 5,
};

/** Остаток ниже этих долей окрашивает полосу: предупреждение и тревога. */
export const WARN_REMAINING_PCT = DEFAULT_LIMIT_THRESHOLDS.warn_remaining_pct;
export const DANGER_REMAINING_PCT = DEFAULT_LIMIT_THRESHOLDS.danger_remaining_pct;

export const NO_LIMIT_DATA = "нет данных";
export const STALE_DATA = "устарело";

export interface MeterView {
  /** Окна у провайдера нет: счётчик не рисуется, «устарело» не ставится. */
  absent: boolean;
  /** Хаб знает это окно. */
  known: boolean;
  /** Остаток для полоски, 0..100. */
  fill: number;
  /** «46%», «<1%», «нет данных» или «устарело». */
  text: string;
  tone: MeterTone;
  /** Сброс окна уже прошёл или показание старше порога. */
  stale: boolean;
  /** «через 2 ч 15 мин» до сброса; у устаревшего окна и после сброса пусто. */
  reset: string;
  /** Без «через» для узкой раскрытой полосы: «2 ч 15 мин». */
  resetSpan: string;
  /** То же сжато для свёрнутой полосы: «2ч15м». */
  resetShort: string;
  /** То же словами для чтения вслух: «через 2 часа 15 минут». */
  resetLong: string;
  /** Момент сброса для подсказки с полной датой; `NaN`, когда сброс не показывается. */
  resetAt: number;
  /** Прогноз расхода; `null`, когда хаб его не прислал или окно устарело. */
  forecast: ForecastView | null;
  observedAt: number;
}

export interface ForecastView {
  /** Окно кончится раньше сброса: предупреждение. */
  before: boolean;
  /** «кончится через 3 ч» или «хватит до сброса». */
  text: string;
  /** То же словами: «кончится через 3 часа». */
  long: string;
  /** Средняя скорость окна: «≈4 %/ч». */
  pace: string;
}

/** Скорость расхода в процентах окна за час: целые от процента, десятые ниже. */
export function paceText(permillePerHour: number): string {
  const pct = Math.max(0, permillePerHour) / 10;
  const value = pct >= 1 || pct === 0 ? String(Math.round(pct)) : pct.toFixed(1).replace(".", ",");
  return `≈${value}\u00a0%/ч`;
}

/** Прогноз окна по полю хаба: предупреждение только при `before_reset`, иначе спокойная строка. */
export function forecastView(
  forecast: WindowForecast | null | undefined,
  usedPermille: number,
  now: number,
): ForecastView | null {
  if (!forecast || usedPermille >= 1000) return null;
  const pace = paceText(forecast.rate_permille_per_hour);
  const exhaustsAt = forecast.exhausts_at ? Date.parse(forecast.exhausts_at) : Number.NaN;
  if (!forecast.before_reset || !Number.isFinite(exhaustsAt)) {
    return { before: false, text: "хватит до сброса", long: "хватит до сброса", pace };
  }
  const left = exhaustsAt - now;
  if (left < 60_000) {
    return { before: true, text: "кончится с минуты на минуту", long: "кончится с минуты на минуту", pace };
  }
  return { before: true, text: `кончится ${untilText(left, true)}`, long: `кончится ${untilLong(left, true)}`, pace };
}

export interface LimitView {
  five: MeterView;
  week: MeterView;
  /** Недельная квота Fable есть только у Claude и может отсутствовать в старых данных. */
  fable: MeterView | null;
  /** Месячное окно есть не у всех провайдеров и может отсутствовать в старых данных. */
  month: MeterView | null;
  tone: "default" | "warning" | "danger" | "reserve";
  stale: boolean;
}

export function remainingPct(usedPermille: number): number {
  return Math.min(100, Math.max(0, 100 - usedPermille / 10));
}

export function remainingText(usedPermille: number): string {
  const remaining = remainingPct(usedPermille);
  if (remaining > 0 && remaining < 1) return "<1%";
  return `${Math.round(remaining)}%`;
}

export function meterTone(
  remaining: number,
  thresholds: Pick<LimitThresholds, "warn_remaining_pct" | "danger_remaining_pct"> = DEFAULT_LIMIT_THRESHOLDS,
): MeterTone {
  if (remaining < thresholds.danger_remaining_pct) return "danger";
  if (remaining < thresholds.warn_remaining_pct) return "warning";
  return "normal";
}

const weekdayFormat = new Intl.DateTimeFormat("ru-RU", { weekday: "short" });

export function shortWeekday(ts: number): string {
  return weekdayFormat.format(ts).replace(".", "").toLocaleLowerCase("ru-RU");
}

function sameDay(a: number, b: number): boolean {
  return new Date(a).toDateString() === new Date(b).toDateString();
}

/** Время без даты в тот же день, иначе с днём недели. */
export function dayClock(ts: number, now: number): string {
  return sameDay(ts, now)
    ? fmt.clock(ts)
    : `${shortWeekday(ts)} ${fmt.clock(ts)}`;
}

const NOT_SEEN: MeterView = {
  absent: false,
  known: false,
  fill: 0,
  text: NO_LIMIT_DATA,
  tone: "normal",
  stale: false,
  reset: "",
  resetSpan: "",
  resetShort: "",
  resetLong: "",
  resetAt: Number.NaN,
  forecast: null,
  observedAt: Number.NaN,
};

export function windowText(
  window: LimitWindow | null | undefined,
  now: number,
  options?: {
    staleAfterMs?: number;
    thresholds?: Pick<LimitThresholds, "warn_remaining_pct" | "danger_remaining_pct">;
  },
): MeterView {
  if (!window) return NOT_SEEN;
  const observedAt = Date.parse(window.observed_at);
  const staleAfterMs = options?.staleAfterMs ?? 180_000;
  const resetAt = window.resets_at ? Date.parse(window.resets_at) : Number.NaN;
  const stale =
    Number.isNaN(observedAt) ||
    now - observedAt > staleAfterMs ||
    (Number.isFinite(resetAt) && resetAt <= now);
  const remaining = remainingPct(window.used_permille);
  const resetShown = !stale && Number.isFinite(resetAt);
  const left = resetAt - now;
  return {
    absent: false,
    known: true,
    fill: remaining,
    text: stale ? STALE_DATA : remainingText(window.used_permille),
    tone: stale ? "normal" : meterTone(remaining, options?.thresholds),
    stale,
    reset: resetShown ? untilText(left) : "",
    resetSpan: resetShown ? spanText(left) : "",
    resetShort: resetShown ? untilShort(left) : "",
    resetLong: resetShown ? untilLong(left) : "",
    resetAt: resetShown ? resetAt : Number.NaN,
    forecast: stale ? null : forecastView(window.forecast, window.used_permille, now),
    observedAt,
  };
}

export function limitView(
  provider: ProviderLimits,
  now: number,
  cfg: LimitThresholds,
): LimitView {
  const options = {
    staleAfterMs: cfg.stale_after_secs * 1000,
    thresholds: cfg,
  };
  const five: MeterView = provider.five_hour_absent && !provider.five_hour
    ? { ...NOT_SEEN, absent: true, text: "" }
    : windowText(provider.five_hour, now, options);
  const week = windowText(provider.weekly, now, options);
  const fable = provider.weekly_fable ? windowText(provider.weekly_fable, now, options) : null;
  const month = provider.monthly ? windowText(provider.monthly, now, options) : null;
  let tone: LimitView["tone"] = "default";
  if (provider.weekly_reserve_active) tone = "reserve";
  else if (five.tone === "danger" || week.tone === "danger" || fable?.tone === "danger" || month?.tone === "danger") tone = "danger";
  else if (five.tone === "warning" || week.tone === "warning" || fable?.tone === "warning" || month?.tone === "warning") tone = "warning";
  return { five, week, fable, month, tone, stale: five.stale || week.stale || Boolean(fable?.stale) || Boolean(month?.stale) };
}

/** Метка статуса провайдера в полосе лимитов: слово, цвет и страница статуса. */
export interface StatusBadge {
  /** «сбой», «частичный сбой», «замедление» или «обслуживание». */
  text: string;
  tone: "warning" | "danger";
  url: string;
  /** Подсказка и подпись для чтения вслух: компания, слово и строка страницы. */
  title: string;
  /** Компания, чья страница статуса: «Anthropic», «OpenAI». */
  company: string;
  description: string;
  incidents: { name: string; url: string | null }[];
}

const STATUS_WORDS: Partial<Record<ProviderStatusLevel, { text: string; tone: StatusBadge["tone"] }>> = {
  degraded: { text: "замедление", tone: "warning" },
  maintenance: { text: "обслуживание", tone: "warning" },
  partial_outage: { text: "частичный сбой", tone: "danger" },
  major_outage: { text: "сбой", tone: "danger" },
};

/** Компания, которая публикует статус провайдера. */
export function statusCompany(provider: string): string {
  if (provider === "claude") return "Anthropic";
  if (provider === "codex") return "OpenAI";
  return provider;
}

/** Метка только для заметного уровня: при `ok`, `unknown` и без статуса ничего не рисуется. */
export function statusBadge(provider: string, status: ProviderStatus | null | undefined): StatusBadge | null {
  if (!status) return null;
  const word = STATUS_WORDS[status.level];
  if (!word) return null;
  const company = statusCompany(provider);
  const parts = [status.components.join(", "), status.description].filter((part) => part.length > 0);
  return {
    ...word,
    url: status.url,
    company,
    title: `${company}: ${word.text}${parts.length ? ` (${parts.join("; ")})` : ""}. Открыть страницу статуса`,
    description: status.description,
    incidents: status.incidents,
  };
}
