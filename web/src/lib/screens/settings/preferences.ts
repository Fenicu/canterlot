import { cell } from '#lib/stores/cell.svelte.js';
import { setMotionScale } from '#lib/motion.js';
export type MotionPref = 'off' | 'slower' | 'normal' | 'faster';
const scales = { off: 0, slower: 1.5, normal: 1, faster: 0.5 } as const;
export function motionPref(): MotionPref {
  try { const value = localStorage.getItem('canterlot.motion'); return value && value in scales ? value as MotionPref : 'normal'; }
  catch { return 'normal'; }
}
export function applyMotion(): void { setMotionScale(scales[motionPref()]); }
export function saveMotion(value: MotionPref): void {
  try { localStorage.setItem('canterlot.motion', value); } catch { /* Выбор действует до закрытия приложения. */ }
  setMotionScale(scales[value]);
}

/** Как показывать рассуждения: строкой, живым окном хвоста или целиком. */
export type ReasoningPref = 'line' | 'live' | 'expanded';
const REASONING_KEY = 'canterlot.reasoning';
const REASONING_DEFAULT: ReasoningPref = 'live';
// Счётчик меняется при записи: открытая лента перечитывает выбор, не дожидаясь перезагрузки.
// Номер записи лежит рядом с ячейкой: сохранение не читает реактивное значение, которое пишет.
const revision = cell(0);
let revisionSerial = 0;

function readReasoning(): ReasoningPref {
  try {
    const value = localStorage.getItem(REASONING_KEY);
    return value === 'line' || value === 'expanded' ? value : REASONING_DEFAULT;
  }
  catch { return REASONING_DEFAULT; }
}

export function reasoningPref(): ReasoningPref {
  revision.get();
  return readReasoning();
}

export function saveReasoningPref(value: ReasoningPref): void {
  try { localStorage.setItem(REASONING_KEY, value); } catch { /* Выбор действует до закрытия приложения. */ }
  revisionSerial += 1;
  revision.set(revisionSerial);
}
