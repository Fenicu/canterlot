import { test, expect } from '@playwright/test';
import { getStack } from './global-setup';
import { synthetic, type SyntheticStep } from './synthetic';
import { answerText, createHubClient, createSessionUI, enableProbe, fetchFrames, sendChaos, sendScript, verifyLedger } from './hub';

const tag = { tag: '@stack' };
const chromiumOnly = { tag: ['@stack', '@chromium'] };

function markedScript(count: number, delayMs: number, marker: (i: number) => string): { script: string; expected: string } {
  const steps: SyntheticStep[] = [];
  let expected = '';
  for (let i = 0; i < count; i++) {
    expected += marker(i);
    steps.push(synthetic.delayMs(delayMs), synthetic.delta(marker(i)));
  }
  return { script: synthetic.script(steps), expected };
}

test.describe('stack: streaming', () => {
  test('smoke_create_session_send_and_stream_to_idle', tag, async ({ page }) => {
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    const script = synthetic.script([
      synthetic.delta('[0001] Шаг один\n\n'),
      synthetic.delayMs(50),
      synthetic.delta('[0002] Шаг два\n\n'),
      synthetic.delayMs(50),
      synthetic.delta('[0003] Готово')
    ]);
    await sendScript(page, script);

    await expect(page.locator('article.card [data-block]')).toContainText('[0003] Готово', { timeout: 20_000 });
    await expect(page.locator('.feed')).toHaveAttribute('aria-busy', 'false', { timeout: 15_000 });
    await hub.waitIdle(id);

    const inputs = await hub.inputs(id);
    expect(inputs).toHaveLength(1);
    expect(inputs[0]!.content[0]!.text).toBe(script);
    expect(await hub.textOf(id)).toBe('[0001] Шаг один\n\n[0002] Шаг два\n\n[0003] Готово');
    expect(await answerText(page)).toContain('[0002] Шаг два');
  });

  test('stream_text_is_exact_across_cuts_and_forced_resync', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    await enableProbe(page);
    const id = await createSessionUI(page, stack);
    await sendChaos(stack.control, { action: 'record', on: true });

    const { script, expected } = markedScript(100, 20, (i) => `[${String(i).padStart(4, '0')}]`);
    await sendScript(page, script);
    await expect(page.locator('article.card [data-block]')).toContainText('[0003]', { timeout: 20_000 });

    // Пять обрывов в случайные моменты, принудительный cursor_ahead, затем чёрная дыра на 3 с.
    for (let cut = 0; cut < 5; cut++) {
      await page.waitForTimeout(80 + Math.floor(Math.random() * 120));
      await sendChaos(stack.control, { action: 'cut' });
    }
    await sendChaos(stack.control, { action: 'rewrite_subscribe_after', delta: 1000 });
    await sendChaos(stack.control, { action: 'cut' });
    await expect.poll(async () => (await fetchFrames(stack.control)).some((f) => f.direction === 'hub_to_client' && f.delivered && f.op === 'resync_required'), { timeout: 20_000 }).toBe(true);
    await sendChaos(stack.control, { action: 'blackhole', on: true });
    await page.waitForTimeout(3000);
    await sendChaos(stack.control, { action: 'blackhole', on: false });

    await expect.poll(() => answerText(page), { timeout: 60_000, intervals: [250] }).toBe(expected);
    await hub.waitIdle(id);

    expect(await hub.textOf(id)).toBe(expected);
    // Хвост, съеденный чёрной дырой, ничто не обнаружит раньше переподключения: max_connection_age на стенде 20 с.
    await expect.poll(() => verifyLedger(page, hub, id), { timeout: 30_000 }).toMatchObject({ ok: true, holes: [], doubled: [] });

    const frames = await fetchFrames(stack.control);
    const subscribes = frames.filter((f) => f.direction === 'client_to_hub' && f.op === 'subscribe' && f.scope === `conversation:${id}`);
    expect(subscribes.length, 'обрывы вызвали повторные подписки').toBeGreaterThanOrEqual(3);
    expect(frames.some((f) => f.direction === 'hub_to_client' && f.op === 'resync_required'), 'cursor_ahead дошёл до клиента').toBe(true);
    await sendChaos(stack.control, { action: 'record', on: false });
  });

  test('ws_reauth_close_every_20s_is_seamless', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    await enableProbe(page);
    const id = await createSessionUI(page, stack);
    await sendChaos(stack.control, { action: 'record', on: true });

    // Хаб закрывает канал каждые 20 с (ws.max_connection_age): поток идёт 36 с и пересекает минимум одно закрытие.
    const { script, expected } = markedScript(360, 100, (i) => `{${String(i).padStart(3, '0')}}`);
    await sendScript(page, script);
    await expect.poll(() => answerText(page), { timeout: 90_000, intervals: [500] }).toBe(expected);
    await hub.waitIdle(id);

    expect(await hub.textOf(id)).toBe(expected);
    await expect.poll(() => verifyLedger(page, hub, id), { timeout: 15_000 }).toMatchObject({ ok: true });
    const frames = await fetchFrames(stack.control);
    const closes = frames.filter((f) => f.direction === 'hub_to_client' && f.op === 'close');
    expect(closes.length, 'хаб закрыл канал минимум один раз за поток').toBeGreaterThanOrEqual(1);
    await sendChaos(stack.control, { action: 'record', on: false });
  });

  test('sleep_resume_recovers_exact_text_within_budget', chromiumOnly, async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'phone', 'Page.setWebLifecycleState есть только в Chromium');
    test.setTimeout(90_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);

    // Наблюдатель в странице: момент первой отрисовки каждого маркера по сырым Date.now().
    await page.evaluate(() => {
      const seen: Record<number, number> = {};
      (window as unknown as { __seen: Record<number, number> }).__seen = seen;
      const step = () => {
        const text = [...document.querySelectorAll('article.card [data-block]')].map((el) => el.textContent ?? '').join('');
        for (const m of text.matchAll(/\[S(\d+)\]/g)) {
          const n = Number(m[1]);
          if (seen[n] === undefined) seen[n] = Date.now();
        }
        requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    });

    await sendChaos(stack.control, { action: 'record', on: true });
    const { script, expected } = markedScript(100, 150, (i) => `[S${i}]`);
    await sendScript(page, script);
    await expect(page.locator('article.card [data-block]')).toContainText('[S4]', { timeout: 20_000 });

    const cdp = await page.context().newCDPSession(page);
    await sendChaos(stack.control, { action: 'blackhole', on: true });
    await cdp.send('Page.setWebLifecycleState', { state: 'frozen' });
    await new Promise((resolve) => setTimeout(resolve, 8000));
    const resumedAt = Date.now();
    await sendChaos(stack.control, { action: 'blackhole', on: false });
    await cdp.send('Page.setWebLifecycleState', { state: 'active' });

    // Транспорт восстановлен, когда первый кадр хаба дошёл до браузера.
    await expect.poll(async () => (await fetchFrames(stack.control)).some((f) => f.direction === 'hub_to_client' && f.delivered && f.timestamp >= resumedAt), { timeout: 15_000 }).toBe(true);
    const frames = await fetchFrames(stack.control);
    const recoveredAt = frames.find((f) => f.direction === 'hub_to_client' && f.delivered && f.timestamp >= resumedAt)!.timestamp;

    // Бэклог: маркеры, которые хаб успел зафиксировать к моменту восстановления.
    const events = await hub.events(`conversation:${id}`, '0');
    let backlog = -1;
    for (const event of events) {
      if (event.kind !== 'block_delta' || Date.parse(event.committed_at) > recoveredAt) continue;
      for (const m of JSON.stringify(event.payload).matchAll(/\[S(\d+)\]/g)) backlog = Math.max(backlog, Number(m[1]));
    }
    expect(backlog, 'за сон хаб зафиксировал бэклог').toBeGreaterThan(20);
    expect(events.length, 'бэклог короче 1000 событий (форма P-08)').toBeLessThan(1000);

    await expect.poll(() => answerText(page), { timeout: 60_000, intervals: [250] }).toBe(expected);
    const seen = await page.evaluate(() => (window as unknown as { __seen: Record<number, number> }).__seen);
    const paintedAt = seen[backlog]!;
    expect(paintedAt, `маркер [S${backlog}] отрисован`).toBeDefined();
    const lag = paintedAt - recoveredAt;
    testInfo.annotations.push({ type: 'sleep-resume', description: `backlog=[S${backlog}] events=${events.length} recovery_to_paint_ms=${lag}` });
    expect(lag, `первая верная отрисовка бэклога через ${lag} мс после восстановления транспорта`).toBeLessThanOrEqual(1000);

    await hub.waitIdle(id);
    expect(await hub.textOf(id)).toBe(expected);
    await sendChaos(stack.control, { action: 'record', on: false });
  });
});
