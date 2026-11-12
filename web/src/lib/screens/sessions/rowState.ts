import type { Activity } from './activityText';

/** Что значит состояние сессии для владельца: одно слово и один знак на аватаре. */
export type RowState = 'running' | 'waiting' | 'paused' | 'stalled' | 'failed' | 'live' | 'fresh' | 'dormant' | 'stopped';

export interface StateMark {
  state: RowState;
  /** Слово для чтения с экрана и подсказки: видимый текст состояния пишет `activityLine`. */
  word: string;
  /** Кольцо вокруг аватара цветом состояния: идёт ход, ждёт владельца, беда, пауза. */
  halo: '--accent' | '--warning' | '--danger' | null;
  /** Пунктирное кольцо: сессию держат лимиты, а не владелец и не сбой. */
  dashed: boolean;
  /** Точка в углу аватара у спокойной сессии: зелёная — процесс жив. */
  dot: '--success' | '--muted' | null;
  /** Процесса нет: аватар серый, строка гаснет. */
  dim: boolean;
  /** Слово состояния жирным: ожидание владельца важнее всего в списке. */
  strong: boolean;
}

const WORDS: Record<RowState, string> = {
  running: 'Идёт ход',
  waiting: 'Ждёт вас',
  paused: 'Пауза по лимиту',
  stalled: 'Подвисла',
  failed: 'Сбой',
  live: 'На связи',
  fresh: 'Новая',
  dormant: 'Процесс не запущен',
  stopped: 'Остановлена'
};

function mark(state: RowState, patch: Partial<StateMark> = {}): StateMark {
  return { state, word: WORDS[state], halo: null, dashed: false, dot: null, dim: false, strong: false, ...patch };
}

/**
 * Кольцо — что-то происходит или требует внимания; точка — процесс жив и спокоен;
 * серый аватар — процесса нет. Без `process_alive` (старый хаб, нет телеметрии)
 * простой читается как раньше: был ход — на связи, не было — новая.
 */
export function stateMark(activity: Activity): StateMark {
  const evidence = activity.evidence ?? {};
  switch (activity.state) {
    case 'working':
      return mark('running', { halo: '--accent' });
    case 'waiting_for_user':
      return mark('waiting', { halo: '--warning', strong: true });
    case 'quota_paused':
      return mark('paused', { halo: '--warning', dashed: true });
    case 'stalled':
      return mark('stalled', { halo: '--danger' });
    case 'failed':
      return mark('failed', { halo: '--danger' });
    case 'stopped':
      return mark('stopped', { dim: true });
    case 'idle': {
      if ((evidence.background_work ?? 0) > 0) return mark('running', { halo: '--accent' });
      if (evidence.process_alive === false) return mark('dormant', { dim: true });
      if (evidence.process_alive === true || evidence.last_turn_at) return mark('live', { dot: '--success' });
      return mark('fresh', { dot: '--muted' });
    }
  }
}
