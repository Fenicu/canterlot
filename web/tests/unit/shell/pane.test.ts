import { beforeEach, describe, expect, it } from 'vitest';
import { LANE_CLOSED_KEY, Pane, laneAutoOpen, pane } from '#lib/shell/pane.svelte.js';
import { LANE_ROOM_PX, laneRoom } from '#lib/shell/layout.js';

describe('shell/pane: вкладка панели делегирования', () => {
  beforeEach(() => {
    pane.close();
    pane.forChat('reset');
  });

  it('только openPlan открывает вкладку «План»', () => {
    pane.forChat('a');
    expect(pane.delegationTab).toBe('tree');
    pane.openPlan();
    expect(pane.delegationTab).toBe('plan');
  });

  it('смена чата возвращает вкладку к дереву делегатов', () => {
    pane.forChat('a');
    pane.openPlan();
    pane.forChat('a');
    expect(pane.delegationTab).toBe('plan');
    pane.forChat('b');
    expect(pane.delegationTab).toBe('tree');
  });

  it('кнопка «Делегирование» в шапке открывает дерево, а не прошлую вкладку', () => {
    pane.forChat('a');
    pane.openPlan();
    pane.view = null;
    pane.toggleDelegation();
    expect(pane.view).toBe('delegation');
    expect(pane.delegationTab).toBe('tree');
    pane.toggleDelegation();
    expect(pane.view).toBeNull();
    pane.openPlan();
    pane.showDelegationTree();
    expect(pane.delegationTab).toBe('tree');
  });
});

describe('shell/layout: порог панели плана и делегатов', () => {
  it('порог 1600 CSS-пикселей, ниже места нет', () => {
    expect(LANE_ROOM_PX).toBe(1600);
    expect(laneRoom(1600)).toBe(true);
    expect(laneRoom(1599)).toBe(false);
    expect(laneRoom(1920)).toBe(true);
  });
});

describe('shell/pane: правило «открыта сама»', () => {
  const wide = { mode: 'desktop', width: 1920, delegated: true, closed: false } as const;

  it('стол, окно от 1600 px, есть план или делегаты, не закрывали: открыта', () => {
    expect(laneAutoOpen(wide)).toBe(true);
    expect(laneAutoOpen({ ...wide, width: 1600 })).toBe(true);
    expect(laneAutoOpen({ ...wide, width: 1871 })).toBe(true);
  });

  it('окно уже порога, планшет и телефон: как сегодня, сама не открывается', () => {
    expect(laneAutoOpen({ ...wide, width: 1599 })).toBe(false);
    expect(laneAutoOpen({ ...wide, width: 1440 })).toBe(false);
    expect(laneAutoOpen({ ...wide, mode: 'tablet' })).toBe(false);
    expect(laneAutoOpen({ ...wide, mode: 'phone' })).toBe(false);
  });

  it('у сессии без плана и делегатов не открывается', () => {
    expect(laneAutoOpen({ ...wide, delegated: false })).toBe(false);
  });

  it('закрытая на этом устройстве не открывается', () => {
    expect(laneAutoOpen({ ...wide, closed: true })).toBe(false);
  });
});

/** Память устройства без браузера: та же поверхность, что у `localStorage`. */
function memoryStorage(): Storage {
  const data = new Map<string, string>();
  return {
    get length() { return data.size; },
    key: (index: number) => [...data.keys()][index] ?? null,
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => { data.set(key, value); },
    removeItem: (key: string) => { data.delete(key); },
    clear: () => data.clear()
  };
}

describe('shell/pane: панель на широком окне', () => {
  let storage: Storage;
  let lane: Pane;

  function desk(width = 1920): Pane {
    const next = new Pane(() => storage);
    next.mode = 'desktop';
    next.width = width;
    return next;
  }

  beforeEach(() => {
    storage = memoryStorage();
    lane = desk();
  });

  it('открыта сама у сессии с планом или делегатами, без вкладки и кнопки', () => {
    expect(lane.shown).toBeNull();
    lane.delegated = true;
    expect(lane.view).toBeNull();
    expect(lane.shown).toBe('delegation');
    expect(lane.stacked).toBe(true);
    expect(lane.planDocked).toBe(true);
  });

  it('уже порога панель закрыта, а открытая кнопкой показывает вкладки', () => {
    lane = desk(1599);
    lane.delegated = true;
    expect(lane.shown).toBeNull();
    expect(lane.planDocked).toBe(false);
    lane.toggleDelegation();
    expect(lane.shown).toBe('delegation');
    expect(lane.stacked).toBe(false);
    // Полоса плана уже порога остаётся в колонке, как сегодня.
    expect(lane.planDocked).toBe(false);
    lane.toggleDelegation();
    expect(lane.shown).toBeNull();
    expect(storage.getItem(LANE_CLOSED_KEY)).toBeNull();
  });

  it('окно сузилось ниже порога: открытая сама панель уходит, расширилось: возвращается', () => {
    lane.delegated = true;
    lane.width = 1500;
    expect(lane.shown).toBeNull();
    lane.width = 1700;
    expect(lane.shown).toBe('delegation');
  });

  it('крестик закрывает и запоминает: следующий вход в чат её не открывает', () => {
    lane.delegated = true;
    lane.closeDelegation();
    expect(lane.shown).toBeNull();
    expect(lane.planDocked).toBe(false);
    expect(storage.getItem(LANE_CLOSED_KEY)).toBe('closed');
    // Ушли из чата и вернулись: шапка снова сообщает о делегатах.
    lane.delegated = false;
    lane.delegated = true;
    expect(lane.shown).toBeNull();
  });

  it('закрытие переживает перезагрузку', () => {
    lane.delegated = true;
    lane.closeDelegation();
    const reloaded = desk();
    reloaded.delegated = true;
    expect(reloaded.closed).toBe(true);
    expect(reloaded.shown).toBeNull();
  });

  it('кнопка «Делегирование» открывает закрытую и стирает память, дальше панель открывается сама', () => {
    lane.delegated = true;
    lane.toggleDelegation();
    expect(lane.shown).toBeNull();
    expect(storage.getItem(LANE_CLOSED_KEY)).toBe('closed');
    lane.toggleDelegation();
    expect(lane.shown).toBe('delegation');
    expect(storage.getItem(LANE_CLOSED_KEY)).toBeNull();
    // Открыта по правилу, а не явно: в чате без делегатов она не висит пустой.
    expect(lane.view).toBeNull();
    lane.delegated = false;
    expect(lane.shown).toBeNull();
    lane.delegated = true;
    expect(lane.shown).toBe('delegation');
    const reloaded = desk();
    reloaded.delegated = true;
    expect(reloaded.shown).toBe('delegation');
  });

  it('полоса плана открывает закрытую панель так же, как кнопка', () => {
    lane.delegated = true;
    lane.closeDelegation();
    lane.openPlan();
    lane.open('delegation');
    expect(lane.shown).toBe('delegation');
    expect(lane.closed).toBe(false);
  });

  it('другая панель стола временно занимает место, а после её закрытия план и делегаты возвращаются', () => {
    lane.delegated = true;
    lane.toggle('messages');
    expect(lane.shown).toBe('messages');
    expect(lane.planDocked).toBe(false);
    lane.close();
    expect(lane.shown).toBe('delegation');
    expect(lane.closed).toBe(false);
  });

  it('без хранилища панель работает, выбор живёт до перезагрузки', () => {
    const bare = new Pane(() => null);
    bare.mode = 'desktop';
    bare.width = 1920;
    bare.delegated = true;
    expect(bare.shown).toBe('delegation');
    bare.closeDelegation();
    expect(bare.shown).toBeNull();
  });
});

describe('shell/pane: стол стоя', () => {
  it('правой панели нет: настройки, сообщения и поиск открываются листом', () => {
    const portrait = new Pane(() => memoryStorage());
    portrait.mode = 'desktop';
    portrait.width = 1080;
    expect(portrait.lane).toBe(true);
    portrait.dock = true;
    expect(portrait.desktop).toBe(true);
    expect(portrait.lane).toBe(false);
    portrait.mode = 'tablet';
    portrait.dock = false;
    expect(portrait.lane).toBe(false);
  });
});
