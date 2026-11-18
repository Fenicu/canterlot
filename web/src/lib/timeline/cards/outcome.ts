// Итог вызова команды или правки файла по тому, что прислал харнесс.
import type { CardCall, StateWord } from './types';

/**
 * Codex и OpenCode дописывают итог в сводку вызова: `…, exit 1`, `…, declined`, `…, completed`.
 * Claude Code пишет код в начало ответа (`Exit code 1`) и отказ владельца — текстом ответа.
 */
const SUMMARY_STATUS = /,\s*(exit (-?\d+)|completed|failed|declined|interrupted|inProgress|in_progress)\s*$/;
/** Итог результата от воркера: успешный Bash Claude Code называет кодом 0. */
const RESULT_EXIT = /^exit (-?\d+)$/;
const CLAUDE_EXIT = /^Exit code (-?\d+)\b/;
const CLAUDE_DENIED = /^(?:Denied by the owner|Permission to use \S+.* (?:has been|was) denied|The user doesn't want to proceed|.*tool use was rejected)/i;

export interface Outcome {
  /** Код выхода, если харнесс его назвал. */
  exit: number | null;
  /** Владелец или правило отказали в разрешении. */
  denied: boolean;
  /** Харнесс назвал вызов сорвавшимся без кода выхода (`…, failed` у Codex и OpenCode). */
  failed: boolean;
}

/** Аргумент из сводки без итога в хвосте: `npm test, exit 1` → `npm test`. */
export function bareArgument(argument: string): string {
  return argument.replace(SUMMARY_STATUS, '').trim();
}

export function callOutcome(call: CardCall): Outcome {
  const status = SUMMARY_STATUS.exec(call.block.summary ?? '');
  if (status) {
    const code = status[2] !== undefined ? Number(status[2]) : null;
    return { exit: code, denied: status[1] === 'declined', failed: status[1] === 'failed' };
  }
  const resultExit = RESULT_EXIT.exec(call.result?.summary?.trim() ?? '');
  if (resultExit) return { exit: Number(resultExit[1]), denied: false, failed: false };
  const exit = CLAUDE_EXIT.exec(call.output);
  const denied = call.state === 'error' && CLAUDE_DENIED.test(call.output.trim());
  return { exit: exit ? Number(exit[1]) : null, denied, failed: false };
}

/** Слово справа сверх обычного состояния: отказ и ненулевой код; иначе `null`, и реестр берёт слово по состоянию. */
export function outcomeWord(outcome: Outcome): StateWord | null {
  if (outcome.denied) return { word: 'отклонено', tone: 'warning' };
  if (outcome.exit !== null && outcome.exit !== 0) return { word: `код ${outcome.exit}`, tone: 'danger' };
  if (outcome.failed) return { word: 'ошибка', tone: 'danger' };
  return null;
}
