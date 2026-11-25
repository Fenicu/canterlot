import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import SendLater from '#lib/composer/SendLater.svelte';
import { fmt } from '#lib/format/ru.js';

const MINUTE = 60_000;

describe('composer/send later', () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
  });

  it('computes the presets from the clock at every opening, not from the first one', () => {
    let clock = new Date('2026-11-25T16:12:00').getTime();
    const onpick = vi.fn();
    const props = $state({ open: false, now: () => clock, panelId: 'later', onclose: () => { props.open = false; }, onpick });
    instance = mount(SendLater, { target: document.body, props });
    const times = () => [...document.querySelectorAll('.presets time')].map((node) => node.textContent);
    const schedule = () => [...document.querySelectorAll('button')].find((node) => node.textContent?.includes('Запланировать на'))!;

    props.open = true;
    flushSync();
    expect(times()).toEqual([fmt.clock(clock + 15 * MINUTE), fmt.clock(clock + 60 * MINUTE)]);

    props.open = false;
    flushSync();
    // Чат остался открытым, прошло 40 минут.
    clock += 40 * MINUTE;
    props.open = true;
    flushSync();

    expect(times()).toEqual([fmt.clock(clock + 15 * MINUTE), fmt.clock(clock + 60 * MINUTE)]);
    expect(schedule().textContent).toContain(fmt.clock(clock + 15 * MINUTE));
    schedule().dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(onpick).toHaveBeenCalledWith(clock + 15 * MINUTE);
  });
});
