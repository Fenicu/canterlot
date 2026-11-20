import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import InkIcon from '../../../src/lib/ui/InkIcon.svelte';
import { icons } from '../../../src/lib/ui/icons';
import { setMotionScale } from '../../../src/lib/motion';
import { inkProps } from './ink-icon.svelte';

let reduced = false;
let target: HTMLElement;
let instance: ReturnType<typeof mount> | null = null;

beforeEach(() => {
  reduced = false;
  vi.stubGlobal('matchMedia', (query: string) => ({
    media: query,
    matches: query.includes('prefers-reduced-motion') && reduced,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
    onchange: null
  }));
  setMotionScale(1);
  target = document.createElement('div');
  document.body.append(target);
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  vi.unstubAllGlobals();
  document.documentElement.style.removeProperty('--motion-scale');
  delete document.documentElement.dataset.motion;
  document.body.textContent = '';
});

const svg = () => target.querySelector('svg')!;

describe('InkIcon', () => {
  it('draws_the_registry_geometry_twice_a_sketch_under_an_ink_stroke', () => {
    instance = mount(InkIcon, { target, props: inkProps('dry', 'hammer') });
    const paths = [...target.querySelectorAll('path')];
    expect(paths.map((p) => p.getAttribute('d'))).toEqual([icons.hammer, icons.hammer]);
    expect(svg().getAttribute('aria-hidden')).toBe('true');
  });

  it('follows_the_state_prop_through_sketch_ink_dry', () => {
    const props = inkProps('sketch');
    instance = mount(InkIcon, { target, props });
    expect(svg().dataset.state).toBe('sketch');
    props.state = 'ink';
    flushSync();
    expect(svg().dataset.state).toBe('ink');
    props.state = 'dry';
    flushSync();
    expect(svg().dataset.state).toBe('dry');
  });

  it('a_refusal_is_its_own_state_and_never_the_ink_one', () => {
    instance = mount(InkIcon, { target, props: inkProps('refused') });
    expect(svg().dataset.state).toBe('refused');
  });

  it('size_normal_uses_the_larger_box', () => {
    instance = mount(InkIcon, { target, props: { ...inkProps('dry'), size: 'normal' } });
    expect(svg().classList.contains('normal')).toBe(true);
  });

  describe('motion', () => {
    it('is_on_by_default', () => {
      instance = mount(InkIcon, { target, props: inkProps('ink') });
      expect(svg().dataset.motion).toBe('on');
    });

    it('is_off_under_system_reduced_motion', () => {
      reduced = true;
      instance = mount(InkIcon, { target, props: inkProps('sketch') });
      expect(svg().dataset.motion).toBe('off');
    });

    it('is_off_at_motion_scale_zero', () => {
      setMotionScale(0);
      instance = mount(InkIcon, { target, props: inkProps('ink') });
      expect(svg().dataset.motion).toBe('off');
    });

    it('is_read_again_when_the_state_changes', () => {
      const props = inkProps('sketch');
      instance = mount(InkIcon, { target, props });
      expect(svg().dataset.motion).toBe('on');
      setMotionScale(0);
      props.state = 'ink';
      flushSync();
      expect(svg().dataset.motion).toBe('off');
      expect(svg().dataset.state).toBe('ink');
    });
  });
});
