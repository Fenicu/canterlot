import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const motion = vi.hoisted(() => ({ press: 120, enter: 280 }));
vi.mock('../../../src/lib/motion', () => ({
  durationMs: (token: string) => (token === '--duration-press' ? motion.press : token === '--duration-enter' ? motion.enter : 0)
}));

import { launch } from '../../../src/lib/composer/launch';

interface FakeAnimation {
  keyframes: Keyframe[];
  onfinish: (() => void) | null;
  cancelled: boolean;
  cancel(): void;
}

let button: HTMLButtonElement;
let arrow: SVGSVGElement;
let animations: FakeAnimation[];

beforeEach(() => {
  motion.press = 120;
  motion.enter = 280;
  animations = [];
  button = document.createElement('button');
  arrow = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  button.append(arrow);
  document.body.append(button);
  (arrow as unknown as { animate: unknown }).animate = vi.fn((keyframes: Keyframe[]) => {
    const animation: FakeAnimation = {
      keyframes,
      onfinish: null,
      cancelled: false,
      cancel() {
        this.cancelled = true;
      }
    };
    animations.push(animation);
    return animation;
  });
});

afterEach(() => {
  button.remove();
});

describe('launch', () => {
  it('sends_the_arrow_out_and_brings_a_new_one_in_from_below', () => {
    launch(button);
    expect(animations).toHaveLength(1);
    expect(animations[0]!.keyframes[1]).toMatchObject({ opacity: 0 });
    expect(String(animations[0]!.keyframes[1]!.transform)).toMatch(/^translateY\(-/);
    animations[0]!.onfinish!();
    expect(animations[0]!.cancelled).toBe(true);
    expect(animations).toHaveLength(2);
    expect(animations[1]!.keyframes[0]).toMatchObject({ opacity: 0 });
    expect(String(animations[1]!.keyframes[0]!.transform)).toMatch(/^translateY\(\d/);
  });

  it('leaves_the_arrow_alone_when_motion_is_off', () => {
    motion.press = 0;
    launch(button);
    expect(animations).toHaveLength(0);
  });

  it('tolerates_a_missing_button_or_arrow', () => {
    expect(() => launch(null)).not.toThrow();
    arrow.remove();
    expect(() => launch(button)).not.toThrow();
    expect(animations).toHaveLength(0);
  });
});
