import { SESSION_STATE_WORD } from '#lib/timeline/hubEvent.js';
import type { Voice } from './types';

/** Глагол от лица агента: «Запустила» у своего оркестратора, безличное имя действия у делегата. */
export function say(voice: Voice, she: string, neutral: string): string {
  return voice === 'she' ? she : neutral;
}

export function quoted(title: string): string {
  const clean = title.trim();
  return clean ? `«${clean}»` : 'сессию';
}

export type Tone = 'accent' | 'success' | 'warning' | 'danger' | 'muted';

export interface SessionWord {
  word: string;
  tone: Tone;
  /** Сессия занята делом: точка горит акцентом. */
  live: boolean;
}

/**
 * Состояние чужой сессии одним словом. Те же слова, что у строк хаба в ленте
 * («завершена», «задала вопрос», «зависла»), чтобы запуск и итог читались одним языком.
 */
export function sessionWord(state: string | null | undefined, opts: { accepted?: boolean } = {}): SessionWord {
  if (opts.accepted) return { word: 'принята', tone: 'muted', live: false };
  switch (state) {
    case 'working':
      return { word: 'работает', tone: 'accent', live: true };
    case 'idle':
      return { word: SESSION_STATE_WORD.idle!, tone: 'success', live: false };
    case 'waiting_for_user':
      return { word: SESSION_STATE_WORD.waiting_for_user!, tone: 'warning', live: false };
    case 'stalled':
      return { word: SESSION_STATE_WORD.stalled!, tone: 'warning', live: false };
    case 'quota_paused':
      return { word: SESSION_STATE_WORD.quota_paused!, tone: 'warning', live: false };
    case 'failed':
      return { word: SESSION_STATE_WORD.failed!, tone: 'danger', live: false };
    case 'stopped':
      return { word: SESSION_STATE_WORD.stopped!, tone: 'muted', live: false };
    case 'deleted':
      return { word: 'удалена', tone: 'muted', live: false };
    default:
      return { word: 'запущена', tone: 'accent', live: false };
  }
}

/** Краткое слово о состоянии для сводки списка: «3 работают, 2 ждут». */
export function stateCountWord(state: string, count: number): string {
  const forms: Record<string, [string, string]> = {
    working: ['работает', 'работают'],
    idle: ['ждёт сообщения', 'ждут сообщения'],
    waiting_for_user: ['ждёт ответа', 'ждут ответа'],
    stalled: ['подвисла', 'подвисли'],
    quota_paused: ['ждёт лимита', 'ждут лимита'],
    failed: ['сорвалась', 'сорвались'],
    stopped: ['остановлена', 'остановлены']
  };
  const pair = forms[state] ?? ['в другом состоянии', 'в другом состоянии'];
  return `${count} ${count === 1 ? pair[0] : pair[1]}`;
}

/** Ошибка хаба приходит строкой `код: сообщение`; владельцу — только сообщение. */
export function hubError(output: string): string {
  const line = output.split('\n').find((part) => part.trim()) ?? '';
  const match = /^([a-z_]+):\s*(.+)$/.exec(line.trim());
  return (match?.[2] ?? line).trim();
}

export function firstLine(text: string): string {
  return text.split('\n').find((line) => line.trim())?.trim() ?? '';
}

export function clip(text: string, max = 80): string {
  const clean = text.replace(/\s+/g, ' ').trim();
  return clean.length > max ? `${clean.slice(0, max - 1).trimEnd()}…` : clean;
}
