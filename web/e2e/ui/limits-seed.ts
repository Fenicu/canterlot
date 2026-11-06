import type { Page } from '@playwright/test';
import { seedConversation } from '../../testing/fake-hub/seed';
import { bootSecondary } from './secondary-fixture';

/**
 * Правдоподобная неделя: три провайдера, у OpenCode есть месяц, Claude почти исчерпан,
 * показание Codex устарело, восемь запусков сегодня с крупными счётчиками токенов.
 */
export async function bootRealisticLimits(page: Page, opts: { history?: 'full' | 'empty' } = {}) {
  let sessions: string[] = [];
  const boot = await bootSecondary(page, '/limits', null, (api) => {
    const now = Date.now();
    const at = (ms: number) => new Date(now + ms).toISOString();
    const fresh = at(-90_000);
    const stale = at(-3 * 3_600_000);
    api.limits = [
      { provider: 'claude', weekly_reserve_active: false,
        five_hour: { used_permille: 620, resets_at: at(2 * 3_600_000 + 14 * 60_000), observed_at: fresh },
        weekly: { used_permille: 962, resets_at: at(2 * 86_400_000 + 5 * 3_600_000), observed_at: fresh } },
      { provider: 'codex', weekly_reserve_active: false,
        five_hour: { used_permille: 90, resets_at: at(-3_600_000), observed_at: stale },
        weekly: { used_permille: 410, resets_at: at(4 * 86_400_000), observed_at: stale } },
      { provider: 'opencode', weekly_reserve_active: false,
        five_hour: { used_permille: 125, resets_at: at(3 * 3_600_000 + 40 * 60_000), observed_at: fresh },
        weekly: { used_permille: 340, resets_at: at(3 * 86_400_000 + 7 * 3_600_000), observed_at: fresh },
        monthly: { used_permille: 507, resets_at: at(17 * 86_400_000), observed_at: fresh } }
    ];
    const machine = { machine: { id: api.id(), name: 'workstation' } };
    sessions = [
      'Редизайн экрана лимитов', 'Разбор очереди ввода на воркере', 'Миграция схемы журналов',
      'Повторная доставка после разрыва', 'Ревью патча адаптера'
    ].map((title) => seedConversation(api, { ...machine, title }));
  });
  const { ids } = boot;
  await page.route('**/api/v1/limits/history?*', (route) => {
    const query = new URL(route.request().url()).searchParams;
    const provider = query.get('provider') ?? 'claude';
    if (opts.history === 'empty') return route.fulfill({ json: { series: [], usage: [], speed: [] } });
    const zone = query.get('tz') ?? 'UTC';
    const start = new Date(query.get('from')!);
    const days = Array.from({ length: 7 }, (_, n) => { const date = new Date(start); date.setDate(date.getDate() + n); date.setHours(22); return date; });
    const today = new Intl.DateTimeFormat('en-CA', { timeZone: zone }).format(new Date());
    // Расход недели на конец дня, %; null — снимка за день нет. У Codex неделя сбросилась посреди окна.
    const weekly: Record<string, (number | null)[]> = {
      claude: [42, 51, 63, 71, 80, 88, 96.2],
      codex: [71, 86, 94, 3, 18, 33, 41],
      opencode: [5, 11, null, 19, 24, 30, 34]
    };
    const series = weekly[provider]
      ? [{ provider, five_hour: [], weekly: days.flatMap((date, n) => {
        const used = weekly[provider]![n];
        if (used == null) return [];
        // Последний день — текущее показание: оно не может быть позже «сейчас».
        const observed = n === 6 ? new Date(Date.now() - 90_000) : date;
        return [{ observed_at: observed.toISOString(), used_permille: Math.round(used * 10), used_pct: used }];
      }) }]
      : [];
    const row = (conversation: string, agent: string, model: string, input: number, cache: number, output: number, delta: number, overlapped = 0) => ({
      day: today, conversation_id: conversation, provider, agent, model, input_tokens: input, output_tokens: output,
      cache_read_tokens: cache, cache_write_tokens: Math.round(cache / 20), weekly_delta_permille: delta, runs: 3, overlapped_runs: overlapped
    });
    const usage = {
      claude: [
        row(sessions[0]!, 'main', 'claude-opus-5-5', 1_840_000, 38_400_000, 412_000, 74),
        row(sessions[0]!, 'agent-1', 'claude-haiku-5-5', 610_000, 4_100_000, 96_000, 9, 1),
        row(sessions[1]!, 'main', 'claude-opus-5-5', 920_000, 21_700_000, 188_000, 41),
        row(sessions[2]!, 'main', 'claude-sonnet-5-5', 402_000, 9_300_000, 61_000, 12),
        row(ids.pinned, 'main', 'claude-opus-5-5', 96_000, 2_400_000, 18_500, 4)
      ],
      codex: [
        row(sessions[3]!, 'main', 'gpt-6.1-sol', 3_100_000, 52_000_000, 240_000, 38),
        row(sessions[4]!, 'main', 'gpt-6.1-sol', 740_000, 6_800_000, 52_000, 6)
      ],
      opencode: [row(sessions[1]!, 'main', 'beam', 2_250_000, 0, 310_000, 15)]
    }[provider] ?? [];
    const stats = (runs: number, tps: number | null, first: number | null) => ({ runs, tokens_per_second: tps, first_token_ms: first });
    const speed = {
      claude: [
        { provider, model: 'claude-opus-5-5', today: stats(9, 66.1, 2300), period: stats(58, 71.2, 1900) },
        { provider, model: 'claude-haiku-5-5', today: stats(4, 182.4, 640), period: stats(21, 176.9, 610) }
      ],
      codex: [{ provider, model: 'gpt-6.1-sol', today: stats(0, null, null), period: stats(33, 94.5, 1350) }],
      opencode: [{ provider, model: 'beam', today: stats(2, 1188, 450), period: stats(12, 1240, 420) }]
    }[provider] ?? [];
    return route.fulfill({ json: { series, usage, speed } });
  });
  await page.reload();
  await page.locator('main[data-secondary-ready]').waitFor();
  return boot;
}

export const SHOT_SIZES = [
  { name: 'desktop-1280', width: 1280, height: 800 },
  { name: 'desktop-1440', width: 1440, height: 900 },
  { name: 'phone-390', width: 390, height: 844 },
  { name: 'phone-320', width: 320, height: 720 }
] as const;
