import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import ChatHeader from "#lib/screens/chat/ChatHeader.svelte";
import { criticalLimit } from "#lib/screens/chat/criticalLimit.js";
import { DEFAULT_LIMIT_THRESHOLDS, type ProviderLimits } from "#lib/stores/limits.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";

vi.mock("#lib/api/client.js", async (original) => ({
  ...(await original<typeof import("#lib/api/client.js")>()),
  getRoutingPolicy: () =>
    Promise.resolve({ data: { policy: { task_types: { brief: { title: "Реализация по брифу" } } }, builtin: null } }),
}));

const NOW = Date.parse("2026-11-25T10:00:00.000Z");
const at = (minutes: number) => new Date(NOW + minutes * 60_000).toISOString();
/** Отсчёты пишутся с неразрывными пробелами: сравниваем слова. */
const plain = (text: string | null | undefined) => (text ?? "").replace(/\s+/g, " ").trim();

function chatWith(patch: Record<string, unknown> = {}, detail: Record<string, unknown> = {}): ChatStore {
  return {
    id: "conv-1",
    detail: {
      title: "Сверка харнессов",
      harness: { id: "codex", title: "Codex", provider: "codex" },
      machine: { name: "workstation" },
      model: "gpt-6.1-sol",
      ...detail,
    },
    settings: null,
    execution: { state: "running", live: true },
    activity: {
      state: "working",
      evidence: { phase: "running_tool", tool_name: "shell", summary: "shell: cd /srv/worktrees/a1 && cargo test", phase_since: at(-1) },
      since: at(-2),
    },
    phase: null,
    queue: { pause_reasons: [], hold_reasons: [], run_slot: "free", queued: 0 },
    requests: { items: new Map() },
    alarms: [],
    spawnQueue: [],
    timeline: { entries: [] },
    nudge: null,
    goal: null,
    turnRecorded: true,
    ...patch,
  } as unknown as ChatStore;
}

function limits(provider: string, fiveUsed: number, weekUsed = 200): ProviderLimits {
  return {
    provider,
    weekly_reserve_active: false,
    five_hour: { used_permille: fiveUsed, resets_at: at(70), observed_at: at(-1) },
    weekly: { used_permille: weekUsed, resets_at: at(3 * 24 * 60), observed_at: at(-1) },
  } as ProviderLimits;
}

describe("chat/ChatHeader", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
  });

  function show(chat: ChatStore, props: Record<string, unknown> = {}): HTMLElement {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(ChatHeader, { target, props: { chat, now: NOW, ...props } });
    flushSync();
    return target;
  }

  function openMenu(target: HTMLElement): HTMLElement {
    target.querySelector<HTMLButtonElement>('[aria-label="Меню сессии"]')!.click();
    flushSync();
    return target.querySelector<HTMLElement>('[role="menu"]')!;
  }

  const delegated = () =>
    chatWith(
      { nudge: { enabled: true, state: "watching" } },
      { parent_id: "parent-1", routing: { task_type: "brief", harness: "codex", model: "gpt-6.1-sol" } },
    );

  it("header_has_one_meta_line_with_short_state_harness_model_and_machine", () => {
    const target = show(chatWith());
    const meta = target.querySelector<HTMLElement>('[data-role="header-meta"]')!;
    expect(plain(meta.textContent)).toBe("Выполняет shell · Codex · GPT-6.1 Sol · workstation");
    // Полный текст состояния с аргументом остаётся подсказкой строки.
    expect(meta.title).toContain("Выполняет shell: cd /srv/worktrees/a1 && cargo test");
    expect(meta.title).toContain("Codex · GPT-6.1 Sol · workstation");
  });

  it("idle_session_meta_line_is_just_the_subtitle", () => {
    const target = show(chatWith({ activity: { state: "idle", evidence: { last_turn_at: at(-5) } }, execution: null }));
    const subtitle = target.querySelector<HTMLElement>('[data-role="header-meta"] .subtitle')!;
    expect(subtitle.textContent).toBe("Codex · GPT-6.1 Sol · workstation");
  });

  it("parent_route_nudge_and_limits_are_not_rows_of_the_header", () => {
    const target = show(delegated(), { parentTitle: "Кантерлот", providers: [limits("codex", 400)] });
    const header = target.querySelector<HTMLElement>(".chat-header")!;
    expect(header.textContent).not.toContain("Запущена сессией");
    expect(header.textContent).not.toContain("Автопинок");
    expect(header.querySelector("[data-parent-link]")).toBeNull();
    expect(header.querySelector('[data-role="route-state-line"]')).toBeNull();
    expect(header.querySelector('[data-role="nudge-state-line"]')).toBeNull();
    expect(header.querySelector(".header-limits, details.limits")).toBeNull();
    expect(header.querySelector('[data-role="limit-alert"]')).toBeNull();
  });

  it("menu_starts_with_parent_route_and_nudge_items", async () => {
    const onOpenRoute = vi.fn();
    const onOpenNudge = vi.fn();
    const target = show(delegated(), { parentTitle: "Кантерлот", onOpenRoute, onOpenNudge });
    await vi.waitFor(() => {
      openMenu(target);
      expect(target.querySelector('[data-role="menu-route"]')?.textContent).toBe("Маршрут: Реализация по брифу");
    });
    const items = [...target.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    expect(items[0]!.textContent).toBe("Запущена сессией Кантерлот");
    expect(items[0]!.getAttribute("href")).toBe("c/parent-1");
    expect(items[1]!.textContent).toBe("Маршрут: Реализация по брифу");
    expect(items[2]!.textContent).toBe("Автопинок: следит");

    items[1]!.click();
    flushSync();
    expect(onOpenRoute).toHaveBeenCalledOnce();
    expect(target.querySelector('[role="menu"]')).toBeNull();

    openMenu(target);
    target.querySelector<HTMLElement>('[data-role="menu-nudge"]')!.closest<HTMLElement>('[role="menuitem"]')!.click();
    flushSync();
    expect(onOpenNudge).toHaveBeenCalledOnce();
  });

  it("menu_has_no_parent_route_or_nudge_items_when_they_do_not_apply", () => {
    const target = show(chatWith(), { onOpenRoute: vi.fn(), onOpenNudge: vi.fn() });
    const menu = openMenu(target);
    expect(menu.querySelector('[data-role="menu-parent"]')).toBeNull();
    expect(menu.querySelector('[data-role="menu-route"]')).toBeNull();
    expect(menu.querySelector('[data-role="menu-nudge"]')).toBeNull();
    expect(menu.textContent).not.toContain("Автопинок");
  });

  it("critical_limit_of_the_session_provider_shows_one_line_linking_to_limits", () => {
    const target = show(chatWith(), { providers: [limits("claude", 990), limits("codex", 970)] });
    const alert = target.querySelectorAll<HTMLAnchorElement>('[data-role="limit-alert"]');
    expect(alert).toHaveLength(1);
    expect(plain(alert[0]!.textContent)).toBe("Лимит 5 ч: осталось 3% · сброс через 1 ч 10 мин");
    expect(alert[0]!.getAttribute("href")).toBe("limits");
  });

  it("warning_level_or_other_provider_limits_stay_out_of_the_chat", () => {
    // 10% остатка — жёлтый порог, не тревожный; Claude на нуле — не провайдер этой сессии.
    const target = show(chatWith(), { providers: [limits("claude", 1000), limits("codex", 900)] });
    expect(target.querySelector('[data-role="limit-alert"]')).toBeNull();
    expect(target.textContent).not.toContain("Лимит");
  });
});

describe("chat/criticalLimit", () => {
  const cfg = DEFAULT_LIMIT_THRESHOLDS;

  it("picks_the_window_with_the_least_left_among_critical_ones", () => {
    expect(plain(criticalLimit([limits("codex", 970, 990)], "codex", NOW, cfg)?.text)).toBe(
      "Недельный лимит: осталось 1% · сброс через 3 д",
    );
  });

  it("follows_the_configured_critical_threshold", () => {
    expect(criticalLimit([limits("codex", 900)], "codex", NOW, cfg)).toBeNull();
    expect(plain(criticalLimit([limits("codex", 900)], "codex", NOW, { ...cfg, danger_remaining_pct: 15 })?.text)).toBe(
      "Лимит 5 ч: осталось 10% · сброс через 1 ч 10 мин",
    );
  });

  it("stale_readings_do_not_alarm", () => {
    const stale = limits("codex", 990);
    stale.five_hour!.observed_at = at(-120);
    expect(criticalLimit([stale], "codex", NOW, cfg)).toBeNull();
  });
});
