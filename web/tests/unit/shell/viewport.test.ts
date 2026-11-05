import { describe, expect, it } from 'vitest';
import { compactViewport, keyboardInset } from '#lib/shell/viewport.js';

describe('shell/viewport', () => {
  it('uses the VisualViewport height only when the keyboard overlays the layout', () => {
    expect(keyboardInset(915, 915, 0)).toBe(0);
    expect(keyboardInset(915, 615, 0)).toBe(300);
    expect(keyboardInset(915, 615, 10)).toBe(290);
  });

  it('does not mistake pinch zoom or bad values for a keyboard', () => {
    expect(keyboardInset(915, 500, 0, 2)).toBe(0);
    expect(keyboardInset(Number.NaN, 500, 0)).toBe(0);
  });

  it('compacts the composer on a keyboard-sized viewport', () => {
    expect(compactViewport(915, 0)).toBe(false);
    expect(compactViewport(520, 0)).toBe(true);
    expect(compactViewport(915, 300)).toBe(true);
  });
});
