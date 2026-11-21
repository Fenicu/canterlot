import { afterEach, describe, expect, it, vi } from 'vitest';
import { inView } from '../../../src/lib/ui/in-view';

type Callback = (entries: { target: Element; isIntersecting: boolean }[]) => void;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

async function fresh() {
  vi.resetModules();
  return (await import('../../../src/lib/ui/in-view')).inView;
}

describe('inView', () => {
  it('reports_visible_without_an_observer_so_motion_stays_the_css_concern', () => {
    vi.stubGlobal('IntersectionObserver', undefined);
    const seen: boolean[] = [];
    const node = document.createElement('div');
    const action = inView(node, (value) => seen.push(value));
    expect(seen).toEqual([true]);
    action.destroy();
  });

  it('shares_one_observer_between_nodes_and_reports_each_its_own_state', async () => {
    let callback: Callback = () => {};
    const observed = new Set<Element>();
    let created = 0;
    class Fake {
      constructor(next: Callback) {
        created += 1;
        callback = next;
      }
      observe(node: Element) { observed.add(node); }
      unobserve(node: Element) { observed.delete(node); }
    }
    vi.stubGlobal('IntersectionObserver', Fake);
    const watch = await fresh();
    const a = document.createElement('div');
    const b = document.createElement('div');
    const seenA: boolean[] = [];
    const seenB: boolean[] = [];
    const first = watch(a, (value) => seenA.push(value));
    const second = watch(b, (value) => seenB.push(value));
    expect(created).toBe(1);
    expect(observed.size).toBe(2);
    callback([{ target: a, isIntersecting: true }, { target: b, isIntersecting: false }]);
    callback([{ target: a, isIntersecting: false }]);
    expect(seenA).toEqual([true, false]);
    expect(seenB).toEqual([false]);
    first.destroy();
    expect(observed.has(a)).toBe(false);
    callback([{ target: a, isIntersecting: true }]);
    expect(seenA).toEqual([true, false]);
    second.destroy();
  });

  it('takes_a_new_callback_on_update', async () => {
    let callback: Callback = () => {};
    class Fake {
      constructor(next: Callback) { callback = next; }
      observe() {}
      unobserve() {}
    }
    vi.stubGlobal('IntersectionObserver', Fake);
    const watch = await fresh();
    const node = document.createElement('div');
    const old = vi.fn();
    const next = vi.fn();
    const action = watch(node, old);
    action.update(next);
    callback([{ target: node, isIntersecting: true }]);
    expect(old).not.toHaveBeenCalled();
    expect(next).toHaveBeenCalledWith(true);
    action.destroy();
  });
});
