import { test, expect, vi, afterEach, beforeEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import StageTrack from '../../src/lib/ui/StageTrack.svelte';
import * as announceModule from '../../src/lib/a11y/announce';

beforeEach(() => {
  vi.stubGlobal('matchMedia', (query: string) => ({
    media: query,
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    onchange: null,
    dispatchEvent: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn()
  }));
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.textContent = '';
});

test('stage_track_does_not_announce_initial_state_on_mount_and_announces_on_transition', async () => {
  const announceSpy = vi.spyOn(announceModule, 'announce');
  const target = document.createElement('div');
  document.body.appendChild(target);

  const stages = [
    { id: '1', label: 'Этап 1' },
    { id: '2', label: 'Этап 2' },
    { id: '3', label: 'Этап 3' }
  ];

  const currentIdx = 0;
  const instance = mount(StageTrack, {
    target,
    props: {
      stages,
      get index() { return currentIdx; },
      state: 'active',
      label: 'Тестовый трек'
    }
  });

  flushSync();
  // On mount: must NOT announce initial state
  expect(announceSpy).not.toHaveBeenCalled();

  unmount(instance);
});

test('stage_track_mounted_already_done_does_not_set_data_landed', async () => {
  const target = document.createElement('div');
  document.body.appendChild(target);

  const stages = [
    { id: '1', label: 'Этап 1' },
    { id: '2', label: 'Этап 2' }
  ];

  // Mounted with terminal stage already done
  const instance = mount(StageTrack, {
    target,
    props: {
      stages,
      index: 1,
      state: 'active',
      label: 'Трек'
    }
  });

  flushSync();
  // Simulate two animation frames when instant becomes false
  await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  flushSync();

  const terminalPoint = target.querySelector('.st-point[aria-current="step"]');
  expect(terminalPoint).not.toBeNull();
  expect(terminalPoint?.hasAttribute('data-landed')).toBe(false);

  unmount(instance);
});

test('stage_track_countdown_uses_real_duration_and_transition_none', async () => {
  const target = document.createElement('div');
  document.body.appendChild(target);

  const animateSpy = vi.fn().mockReturnValue({ cancel: vi.fn() });
  window.Element.prototype.animate = animateSpy;

  const stages = [
    { id: '1', label: 'Лимит', kind: 'ring' as const },
    { id: '2', label: 'Готово' }
  ];

  const instance = mount(StageTrack, {
    target,
    props: {
      stages,
      index: 0,
      state: 'active',
      label: 'Кранч',
      countdown: { from: 0.25, durationMs: 15000 }
    }
  });

  flushSync();

  expect(animateSpy).toHaveBeenCalled();
  const [, options] = animateSpy.mock.calls[0] as [unknown, KeyframeAnimationOptions];
  expect(options.duration).toBe(15000);
  expect(options.easing).toBe('linear');

  const arc = target.querySelector<SVGCircleElement>('.st-ring-arc');
  expect(arc).not.toBeNull();
  expect(arc?.style.transition || getComputedStyle(arc!).transition).toMatch(/none/);

  unmount(instance);
});
