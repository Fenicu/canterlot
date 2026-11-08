import { failureLabel, providerLabel, retryLabel } from "#lib/format/labels.js";
import { fmt } from "#lib/format/ru.js";
import { leaseWaitText } from "#lib/format/lease.js";

export type Tone = "accent" | "default" | "warning" | "danger";

export interface ActivityEvidence {
  kind?: string;
  last_turn_at?: string | null;
  background_work?: number | null;
  phase?: string;
  phase_since?: string | null;
  summary?: string | null;
  tool_name?: string | null;
  question?: string | null;
  last_event_at?: string;
  reason?: string;
  provider?: string | null;
  until?: string | null;
  outcome?: string;
  /** Процесс ждёт очереди тяжёлых задач машины. */
  lease_wait?: { kind: string; position: number; since?: string } | null;
}

export interface Activity {
  state:
    | "idle"
    | "working"
    | "waiting_for_user"
    | "stalled"
    | "quota_paused"
    | "failed"
    | "stopped";
  evidence?: ActivityEvidence;
  since?: string;
}

export function activityLine(
  activity: Activity,
  ctx: { now: number },
): { text: string; tone: Tone; live: boolean } {
  const evidence = activity.evidence ?? {};
  switch (activity.state) {
    case "idle": {
      const background = evidence.background_work ?? 0;
      if (background > 0)
        return {
          text: `Ожидает сообщения · ${background} ${fmt.plural(background, ["фоновая", "фоновые", "фоновых"])}`,
          tone: "accent",
          live: true,
        };
      return {
        text: evidence.last_turn_at ? "Ожидает сообщения" : "Новая сессия",
        tone: "default",
        live: false,
      };
    }
    case "working":
      return working(evidence, activity.since, ctx.now);
    case "waiting_for_user": {
      const question = (evidence.question ?? "").trim().slice(0, 48);
      return {
        text: question ? `Ждёт ответа · ${question}` : "Ждёт ответа",
        tone: "warning",
        live: false,
      };
    }
    case "stalled": {
      const from = Date.parse(evidence.last_event_at ?? activity.since ?? "");
      const duration = Number.isFinite(from)
        ? fmt.durationCoarse(Math.max(0, ctx.now - from))
        : fmt.duration(0);
      return {
        text: `Подвисла? Нет событий ${duration}`,
        tone: "warning",
        live: false,
      };
    }
    case "quota_paused":
      return quota(evidence, ctx.now);
    case "failed":
      return {
        text: `Сбой: ${failureLabel(evidence.reason)}`,
        tone: "danger",
        live: false,
      };
    case "stopped":
      if (evidence.outcome === "user_stop")
        return { text: "Остановлена вами", tone: "default", live: false };
      if (evidence.outcome === "force_stopped")
        return {
          text: "Остановлена принудительно",
          tone: "default",
          live: false,
        };
      return { text: "Завершена", tone: "default", live: false };
  }
}

/** Идёт ли ход: живой процесс, который ждёт ввода, никакой работы не ведёт. */
export function turnRunning(activity: Activity): boolean {
  if (activity.state === "waiting_for_user" || activity.state === "stalled")
    return true;
  if (activity.state !== "working") return false;
  const phase = activity.evidence?.phase;
  return phase !== "starting" && phase !== "stopping";
}

/** Что идёт прямо сейчас: продолжение фразы в предупреждениях меню. */
export function turnText(activity: Activity, now: number): string {
  const line = activityLine(activity, { now }).text;
  return line.charAt(0).toLocaleLowerCase("ru-RU") + line.slice(1);
}

/** Сколько идёт фаза: её начало не раньше начала хода, иначе старая отметка тянула бы часы из прошлого хода. */
function phaseElapsed(
  phaseSince: string | null | undefined,
  since: string | undefined,
  now: number,
): number {
  const phase = Date.parse(phaseSince ?? "");
  const turn = Date.parse(since ?? "");
  const from = Number.isFinite(phase)
    ? Number.isFinite(turn)
      ? Math.max(phase, turn)
      : phase
    : Number.isFinite(turn)
      ? turn
      : Number.NaN;
  return Number.isFinite(from) ? Math.max(0, now - from) : 0;
}

function working(
  evidence: ActivityEvidence,
  since: string | undefined,
  now: number,
): { text: string; tone: Tone; live: boolean } {
  const phase = evidence.phase ?? "";
  const wait = evidence.lease_wait;
  if (wait && typeof wait.kind === "string" && typeof wait.position === "number")
    return { text: leaseWaitText(wait), tone: "accent", live: false };
  if (phase === "no_link")
    return { text: "Нет связи с машиной", tone: "warning", live: false };
  if (phase === "generating")
    return {
      text: retryLabel(evidence.summary) ?? "Генерирует",
      tone: "accent",
      live: false,
    };
  if (phase === "starting")
    return { text: "Запускается", tone: "accent", live: false };
  if (phase === "stopping")
    return { text: "Останавливается", tone: "accent", live: false };
  if (phase === "context_maintenance")
    return { text: "Сжимает контекст", tone: "accent", live: false };
  if (phase === "delivering_attachments")
    return { text: "Передаю файлы на машину", tone: "accent", live: false };
  if (phase === "running_tool") {
    const name = evidence.summary || evidence.tool_name || "инструмент";
    const duration = fmt.durationCoarse(
      phaseElapsed(evidence.phase_since, since, now),
    );
    return {
      text: `Выполняет ${name} · ${duration}`,
      tone: "accent",
      live: false,
    };
  }
  return { text: "Работает", tone: "accent", live: true };
}

function quota(
  evidence: ActivityEvidence,
  now: number,
): { text: string; tone: Tone; live: boolean } {
  const until = evidence.until ? Date.parse(evidence.until) : Number.NaN;
  if (evidence.reason === "weekly_reserve")
    return { text: "Ожидание: недельный лимит", tone: "warning", live: false };
  if (evidence.reason === "limit_reached") {
    const who = providerLabel(evidence.provider ?? "claude");
    const when = Number.isFinite(until) ? fmt.resetAt(until, now) : "";
    return {
      text: when
        ? `Остановлена: лимит ${who} до ${when}`
        : `Остановлена: лимит ${who}`,
      tone: "warning",
      live: false,
    };
  }
  const clock = Number.isFinite(until) ? fmt.clock(until) : "";
  return {
    text: clock
      ? `Пауза: лимит на 5 часов до ${clock}`
      : "Пауза: лимит на 5 часов",
    tone: "warning",
    live: false,
  };
}
