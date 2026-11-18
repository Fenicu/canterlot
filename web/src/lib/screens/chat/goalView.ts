import type { components } from '#lib/api/generated/openapi.js';
import { fmt } from '#lib/format/ru.js';

export type Goal = components["schemas"]["GoalView"];

export type GoalTone = "success" | "warning" | "danger";

const WORDS: Record<string, { text: string; tone: GoalTone }> = {
  active: { text: "активна", tone: "success" },
  paused: { text: "на паузе", tone: "warning" },
  blocked: { text: "заблокирована", tone: "danger" },
  usage_limited: { text: "упёрлась в лимит", tone: "warning" },
  budget_limited: { text: "бюджет исчерпан", tone: "danger" },
  complete: { text: "достигнута", tone: "success" },
};

export function goalState(status: string): { text: string; tone: GoalTone } {
  return WORDS[status] ?? { text: status, tone: "warning" };
}

/** Короткая строка расхода: токены, ходы и время — только известные значения. */
export function goalUsage(goal: Goal): string {
  const parts: string[] = [];
  if (goal.tokens_used > 0) {
    parts.push(`${fmt.tokensCompact(goal.tokens_used, { short: true })} токенов`);
  }
  if (goal.turns_used != null && goal.turns_used > 0) {
    parts.push(`${fmt.number(goal.turns_used)} ходов`);
  }
  if (goal.time_used_seconds > 0) {
    parts.push(fmt.durationCoarse(goal.time_used_seconds * 1000));
  }
  return parts.join(" · ");
}
