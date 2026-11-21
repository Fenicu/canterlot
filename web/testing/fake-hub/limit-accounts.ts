import type { Page } from '@playwright/test';
import type { ProviderLimits } from '../../src/lib/stores/limits';
import type { FakeHubApi } from './api';

/** Самое длинное имя учётной записи, которое принимает воркер: 40 знаков из `[a-z0-9-]`. */
export const LONG_ACCOUNT = 'b-opencode-subscription-with-a-long-name';

export interface SecondAccount {
  /** Учётная запись второй подписки OpenCode; по умолчанию `b-opencode`. */
  account?: string;
  /** Имя машины, на которой она живёт. */
  machine?: string;
}

/**
 * Две подписки OpenCode в снимке лимитов: запись по умолчанию (машина-хозяин) и вторая запись другой машины.
 * Лимиты других провайдеров остаются как есть. Вход по машинам приходит отдельным списком у каждой записи.
 */
export function seedSecondOpencodeAccount(api: FakeHubApi, opts: SecondAccount = {}): void {
  const account = opts.account ?? 'b-opencode';
  const machine = opts.machine ?? 'machine-b';
  const now = Date.now();
  const at = (ms: number) => new Date(now + ms).toISOString();
  const fresh = at(-90_000);
  const owner: ProviderLimits = {
    provider: 'opencode',
    weekly_reserve_active: false,
    five_hour: { used_permille: 125, resets_at: at(3 * 3_600_000), observed_at: fresh },
    weekly: { used_permille: 340, resets_at: at(3 * 86_400_000), observed_at: fresh },
    machine_credentials: [{ machine_id: 'machine-a-id', machine_name: 'workstation', expires_at: at(40 * 86_400_000) }]
  };
  const other: ProviderLimits = {
    provider: 'opencode',
    account,
    weekly_reserve_active: false,
    five_hour: { used_permille: 610, resets_at: at(2 * 3_600_000), observed_at: fresh },
    weekly: { used_permille: 720, resets_at: at(4 * 86_400_000), observed_at: fresh },
    machine_credentials: [{ machine_id: 'machine-b-id', machine_name: machine, expires_at: at(60 * 86_400_000) }]
  };
  api.limits = [...api.limits.filter((row) => row.provider !== 'opencode'), owner, other];
}

/** История лимитов с серией и расходом по каждой записи OpenCode: у записей разные недели и разный расход. */
export async function routeAccountHistory(page: Page, conversationId: string, account = 'b-opencode'): Promise<void> {
  await page.route('**/api/v1/limits/history?*', (route) => {
    const query = new URL(route.request().url()).searchParams;
    const provider = query.get('provider') ?? 'claude';
    if (provider !== 'opencode') return route.fulfill({ json: { series: [], usage: [], speed: [] } });
    const start = new Date(query.get('from')!);
    const days = Array.from({ length: 7 }, (_, n) => { const date = new Date(start); date.setDate(date.getDate() + n); date.setHours(22); return date; });
    const today = new Intl.DateTimeFormat('en-CA', { timeZone: query.get('tz') ?? 'UTC' }).format(new Date());
    const weekly = (used: number[]) => days.map((date, n) => ({
      observed_at: (n === 6 ? new Date(Date.now() - 90_000) : date).toISOString(),
      used_permille: used[n]!,
      used_pct: used[n]! / 10
    }));
    const row = (owner: string, delta: number) => ({
      day: today, conversation_id: conversationId, provider, account: owner, agent: 'main', model: 'beam',
      input_tokens: 1_200_000, output_tokens: 90_000, cache_read_tokens: 0, cache_write_tokens: 0,
      weekly_delta_permille: delta, runs: 3, overlapped_runs: 0
    });
    return route.fulfill({ json: {
      series: [
        { provider, account: 'owner', five_hour: [], weekly: weekly([50, 110, 160, 210, 260, 300, 340]), weekly_fable: [] },
        { provider, account, five_hour: [], weekly: weekly([200, 290, 400, 480, 590, 660, 720]), weekly_fable: [] }
      ],
      usage: [row('owner', 25), row(account, 61)],
      speed: []
    } });
  });
}
