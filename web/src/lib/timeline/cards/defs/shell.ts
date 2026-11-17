// Команда оболочки: Bash у Claude Code, bash у OpenCode, commandExecution у Codex.
import { list, str } from '../json';
import { clip, firstLine } from '../copy';
import { bareArgument, callOutcome, type Outcome } from '../outcome';
import { formatCommand } from '../shell';
import type { CardCall, CardLogic } from '../types';

export interface ShellData {
  /** Команда как её написал агент: её копирует кнопка. */
  command: string;
  /** Команда для показа: длинная строка разложена по операторам. */
  formatted: string;
  /** Пояснение агента к команде (Claude Code). */
  description: string;
  cwd: string;
  /** Запущена в фоне: её вывод придёт позже отдельным инструментом. */
  background: boolean;
  outcome: Outcome;
}

/** Codex старых версий передаёт команду списком: `["bash", "-lc", "…"]`. */
function commandOf(value: unknown): string {
  if (typeof value === 'string') return value;
  const parts = list(value as never).filter((part): part is string => typeof part === 'string');
  if (parts.length >= 3 && /(?:^|\/)(?:ba|z)?sh$/.test(parts[0]!) && /^-\w*c$/.test(parts[1]!)) return parts.slice(2).join(' ');
  return parts.join(' ');
}

/**
 * Codex и OpenCode кладут в текст вызова саму команду, Claude Code — вход JSON.
 * Пока вызов идёт, текста нет: команду даёт сводка воркера.
 */
export function shellCommand(call: CardCall): string {
  if (call.hasInput) return commandOf(call.input.command ?? call.input.cmd).trim();
  const final = call.block.state === 'final' || call.block.state === 'incomplete';
  const raw = final ? call.block.text.trim() : '';
  return raw || bareArgument(call.argument);
}

export const shell: CardLogic<ShellData> = {
  label: 'Команда',
  parse(call) {
    const command = shellCommand(call);
    if (!command) return null;
    const input = call.hasInput ? call.input : {};
    return {
      command,
      formatted: formatCommand(command),
      description: str(input.description).trim(),
      cwd: str(input.cwd) || str(input.workdir) || str(input.workingDirectory),
      background: input.run_in_background === true,
      outcome: callOutcome(call)
    };
  },
  summary: (_call, data) => (data.description ? clip(data.description, 90) : `Команда: ${clip(firstLine(data.command), 90)}`)
};
