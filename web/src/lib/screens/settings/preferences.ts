import { setMotionScale } from '$lib/motion';
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
