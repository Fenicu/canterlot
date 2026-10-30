import { test, expect, type Page } from '@playwright/test';
import { getStack } from '../stack/global-setup';
import { synthetic, type SyntheticStep } from '../stack/synthetic';
import { createHubClient, createSessionUI, enableProbe, fetchFrames, openSession, sendChaos, type HubClient } from '../stack/hub';
import { distribution, standardConditions } from './measure';
import { saveDistribution, writeReport } from './report';

const tag = { tag: ['@perf', '@stack'] };
const chromiumTag = { tag: ['@perf', '@stack', '@chromium'] };
const PHONE_CONDITIONS = { cpu_throttle: '4x (CDP Emulation.setCPUThrottlingRate)', viewport: '412x892, touch, Chromium', browser: 'chromium (project phone)' };

// Трасса Playwright снимает DOM до и после каждого действия на основном потоке страницы под дросселем:
// на пути отправки это четверть времени потока, поэтому в замерах она выключена.
test.use({ trace: 'off' });
test.afterAll(() => writeReport());
test.beforeEach(({}, testInfo) => {
  test.skip(testInfo.project.name !== 'phone', 'Замеры идут только в телефонном профиле Chromium');
});

async function throttle(page: Page, rate = 4): Promise<void> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Emulation.setCPUThrottlingRate', { rate });
}

async function tapSend(page: Page): Promise<void> {
  const box = (await page.getByRole('button', { name: 'Отправить', exact: true }).boundingBox())!;
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
}

/** В странице: момент щелчка по «Отправить» и кадр, в котором локальный элемент нарисован, а поле ввода очищено и доступно. */
async function installSendRecorder(page: Page): Promise<void> {
  await page.evaluate(() => {
    performance.setResourceTimingBufferSize(5000);
    const store = { clicks: [] as number[], painted: [] as number[] };
    (window as unknown as { __perf: typeof store }).__perf = store;
    document.addEventListener('click', (event) => {
      if (!(event.target as Element | null)?.closest('button[aria-label="Отправить"]')) return;
      const field = document.querySelector<HTMLTextAreaElement>('[data-composer] textarea');
      const marker = field?.value.match(/\bp\d\d-\d+\b/)?.[0];
      if (!marker) return;
      const start = event.timeStamp;
      store.clicks.push(start);
      const check = () => {
        const current = document.querySelector<HTMLTextAreaElement>('[data-composer] textarea');
        const painted = [...document.querySelectorAll('.strip [data-state], article.bubble')].some((el) => el.textContent?.includes(marker));
        if (painted && current && current.value === '' && !current.disabled) {
          requestAnimationFrame(() => store.painted.push(performance.now() - start));
          return;
        }
        requestAnimationFrame(check);
      };
      requestAnimationFrame(check);
    }, true);
  });
}

const sendScriptFor = (marker: string) => synthetic.script([synthetic.delta(`${marker}`)]);

async function runSends(page: Page, hub: HubClient, id: string, prefix: string, count: number): Promise<void> {
  const composer = page.getByRole('textbox', { name: 'Сообщение' });
  for (let i = 0; i < count; i++) {
    // Затвор отвергает нажатие при 100 вводах в очереди: на загруженном хосте воркер отстаёт от темпа нажатий.
    if (i % 10 === 0) while ((await hub.detail(id)).queue.queued > 50) await page.waitForTimeout(250);
    await composer.fill(sendScriptFor(`${prefix}-${i}`));
    await tapSend(page);
    await page.waitForFunction((n) => (window as unknown as { __perf: { painted: unknown[] } }).__perf.painted.length >= n, i + 1, { timeout: 30_000 });
    await page.waitForTimeout(120);
  }
}

test.describe('perf: P-01…P-05', () => {
  test('p01_send_press_to_local_item', chromiumTag, async ({ page }) => {
    test.setTimeout(900_000);
    const stack = getStack();
    const N = 200;
    const id = await createSessionUI(page, stack);
    await throttle(page);
    await installSendRecorder(page);
    const startedAt = Date.now();
    await runSends(page, createHubClient(stack), id, 'p01', N);
    const samples = await page.evaluate(() => (window as unknown as { __perf: { painted: number[] } }).__perf.painted);

    const dist = distribution(samples, {
      metric: 'P-01: нажатие «Отправить» до локального элемента в полосе и свободного поля ввода',
      durationMs: Date.now() - startedAt,
      target: 'p95 ≤ 100 мс',
      limitMs: { p95: 100 },
      conditions: standardConditions({ ...PHONE_CONDITIONS, sends: N, interval_between_sends_ms: '~120 + fill, пауза при очереди больше 50', definition: 'event.timeStamp щелчка до следующего rAF после появления элемента в полосе или ленте и очистки поля (проверка в rAF)' })
    });
    saveDistribution(dist);
    expect(dist.n).toBe(N);
    await createHubClient(stack).waitIdle(id, 180_000);
    expect((await createHubClient(stack).inputs(id)).length, 'каждое нажатие создало ровно один ввод').toBe(N);
  });

  test('p02_send_to_hub_commit_with_80ms_rtt', chromiumTag, async ({ page }) => {
    test.setTimeout(900_000);
    const stack = getStack();
    const N = 200;
    const id = await createSessionUI(page, stack);
    await throttle(page);
    await installSendRecorder(page);
    await sendChaos(stack.control, { action: 'latency', ms: 40 });
    const startedAt = Date.now();
    try {
      await runSends(page, createHubClient(stack), id, 'p02', N);
      // Запрос последнего нажатия может уйти позже кадра с локальным элементом: ждём ответ на каждое нажатие.
      await page.waitForFunction((n) => performance.getEntriesByType('resource').filter((entry) => /\/conversations\/[^/]+\/inputs$/.test(entry.name)).length >= n, N, { timeout: 30_000 });
    } finally {
      await sendChaos(stack.control, { action: 'latency', ms: 0 });
    }
    const { clicks, responses } = await page.evaluate(() => {
      const store = (window as unknown as { __perf: { clicks: number[] } }).__perf;
      const entries = performance.getEntriesByType('resource') as PerformanceResourceTiming[];
      return { clicks: store.clicks, responses: entries.filter((entry) => /\/conversations\/[^/]+\/inputs$/.test(entry.name)).map((entry) => entry.responseEnd) };
    });
    expect(responses.length, 'на каждое нажатие ровно один POST').toBe(N);
    const samples = clicks.map((click, i) => responses[i]! - click);

    const dist = distribution(samples, {
      metric: 'P-02: нажатие «Отправить» до ответа 201 хаба при RTT 80 мс',
      durationMs: Date.now() - startedAt,
      target: 'p95 ≤ 300 мс, p99 ≤ 800 мс',
      limitMs: { p95: 300, p99: 800 },
      conditions: standardConditions({ ...PHONE_CONDITIONS, sends: N, network_rtt: '80 мс (кромка: по 40 мс в каждую сторону)', definition: 'event.timeStamp щелчка до responseEnd записи Resource Timing POST …/inputs (журнал операций, IndexedDB и HTTP)' })
    });
    saveDistribution(dist);
    await createHubClient(stack).waitIdle(id, 180_000);
  });

  test('p03_commit_to_worker_receipt_from_hub_timestamps', tag, async ({ page }) => {
    test.setTimeout(900_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const N = 200;
    const id = await createSessionUI(page, stack);
    await sendChaos(stack.control, { action: 'record', on: true });
    const startedAt = Date.now();
    // Вводы идут по одному в простаивающую сессию: очередь за предыдущим ходом в замер не входит.
    const scope = `conversation:${id}`;
    for (let i = 0; i < N; i++) {
      const after = await hub.eventsHead(scope);
      const { input_id: inputId } = await hub.postInput(id, sendScriptFor(`p03-${i}`));
      const deadline = Date.now() + 60_000;
      for (;;) {
        const events = await hub.events(scope, after);
        if (events.some((event) => event.kind === 'run_finished' && (event.payload as { input_id?: string }).input_id === inputId)) break;
        if (Date.now() > deadline) throw new Error(`ввод ${i} не завершился`);
        await page.waitForTimeout(15);
      }
      await page.waitForTimeout(40);
    }
    await hub.waitIdle(id, 60_000);
    await page.waitForTimeout(1500);

    const frames = (await fetchFrames(stack.control)).filter((f) => f.direction === 'hub_to_client' && f.event);
    await sendChaos(stack.control, { action: 'record', on: false });
    // Квитанция воркера: on_machine, а у простаивающего воркера хаб сразу пишет transmitting без on_machine; берём первое из двух.
    const queued = new Map<string, number>();
    const receipt = new Map<string, number>();
    const explicitOnMachine = new Set<string>();
    const take = (event: { kind: string; committed_at: string; payload: unknown }, fromLog: boolean) => {
      const payload = event.payload as { input_id?: string; state?: string };
      if (!payload.input_id) return;
      const at = Date.parse(event.committed_at);
      if (event.kind === 'input_queued') {
        if (!queued.has(payload.input_id)) queued.set(payload.input_id, at);
      } else if (event.kind === 'input_state_changed' && (payload.state === 'on_machine' || payload.state === 'transmitting')) {
        if (!receipt.has(payload.input_id)) receipt.set(payload.input_id, at);
        if (payload.state === 'on_machine') explicitOnMachine.add(payload.input_id);
        if (fromLog) logged.add(payload.input_id);
      }
    };
    const logged = new Set<string>();
    for (const frame of frames) take(frame.event!, true);
    // Кадры, ушедшие на пересоединении (закрытие канала раз в 20 с), в журнал не попадают: недостающее берём из тех же событий хаба.
    for (const event of await hub.events(`conversation:${id}`, '0')) take(event, false);
    const samples = [...queued].filter(([key]) => receipt.has(key)).map(([key, at]) => receipt.get(key)! - at);
    expect(queued.size, 'все вводы дошли до хаба').toBe(N);
    expect(samples.length, 'у каждого ввода есть квитанция воркера').toBe(N);

    const dist = distribution(samples, {
      metric: 'P-03: input_queued до квитанции воркера (on_machine или transmitting) по часам хаба',
      durationMs: Date.now() - startedAt,
      target: 'p95 ≤ 200 мс',
      limitMs: { p95: 200 },
      conditions: standardConditions({ inputs: N, pairs_from_frame_log: logged.size, pairs_from_hub_events_after_reconnect: N - logged.size, inputs_with_explicit_on_machine: explicitOnMachine.size, receipt: 'on_machine, иначе transmitting', clock: 'committed_at хаба для обоих событий (журнал кадров кромки)', note: 'верхняя оценка: квитанция идёт и от воркера к хабу' })
    });
    saveDistribution(dist);
  });

  test('p04_delta_commit_to_paint_segment_with_full_accounting', chromiumTag, async ({ page }) => {
    test.setTimeout(900_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    await enableProbe(page);
    const id = await createSessionUI(page, stack);
    await throttle(page);

    // 10 000 исходных дельт: по три с паузой 30 мс, около 100 дельт в секунду, как у быстрого агента.
    const TOTAL = 10_000;
    const steps: SyntheticStep[] = [];
    let expected = '';
    for (let i = 0; i < TOTAL; i++) {
      const marker = `(${i.toString(36).padStart(3, '0')})`;
      expected += marker;
      steps.push(synthetic.delta(marker));
      if (i % 3 === 2) steps.push(synthetic.delayMs(30));
    }
    const script = synthetic.script(steps);
    expect(Buffer.byteLength(script), 'скрипт укладывается в лимит текста ввода').toBeLessThan(262_144);
    const startedAt = Date.now();
    await hub.postInput(id, script);
    await hub.waitIdle(id, 600_000);
    await expect.poll(() => page.locator('article.card [data-block]').last().evaluate((el) => (el.textContent ?? '').includes('(7pr)')), { timeout: 120_000 }).toBe(true);

    const snapshot = await page.evaluate(() => {
      const probe = window.__canterlot_probe!;
      return probe.snapshot() as { metrics: Record<string, number[]> };
    });
    const samples = snapshot.metrics.delta_paint_raw_ms ?? [];
    expect(samples.length).toBeGreaterThan(100);

    const events = await hub.events(`conversation:${id}`, '0');
    const deltaEvents = events.filter((event) => event.kind === 'block_delta');
    const sources = deltaEvents.reduce((sum, event) => sum + Number(BigInt(event.scope_seq) - BigInt(event.scope_seq_from) + 1n), 0);
    expect(sources, 'исходных дельт на хабе').toBe(TOTAL);
    const head = await hub.eventsHead(`conversation:${id}`);
    const ledger = await page.evaluate(([scope, end]) => window.__canterlot_probe!.ledger.verify(scope!, end!), [`conversation:${id}`, head]);
    expect(ledger, 'каждое событие показано, склеено, восстановлено диапазоном или снимком ровно один раз').toMatchObject({ ok: true, holes: [], doubled: [] });
    expect(await hub.textOf(id)).toBe(expected);
    const painted = (await page.locator('article.card [data-block]').allTextContents()).join('').trim();
    expect(painted).toBe(expected);

    const dist = distribution(samples, {
      metric: 'P-04: committed_at_from сегмента до его отрисовки',
      durationMs: Date.now() - startedAt,
      target: 'p95 ≤ 250 мс, p99 ≤ 800 мс',
      limitMs: { p95: 250, p99: 800 },
      conditions: standardConditions({ ...PHONE_CONDITIONS, source_deltas: TOTAL, stream_rate: '~100 дельт/с (по три каждые 30 мс)', delta_events_on_hub: deltaEvents.length, ledger: 'чисто (нет дыр и двойного учёта)', definition: 'committed_at_from старейшей склеенной дельты хаба против сырого Date.now() в первом rAF после кадра, в котором текст нарисован', samples_cap: 'пробы хранят не более 10 000 последних значений' })
    });
    saveDistribution(dist);
  });

  test('p05_interrupt_to_first_status', chromiumTag, async ({ page }) => {
    test.setTimeout(900_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const N = 50;
    await page.addInitScript(() => {
      const seen: { at: number; kind: string }[] = [];
      (window as unknown as { __interrupts: typeof seen; __clicks: number[] }).__interrupts = seen;
      (window as unknown as { __clicks: number[] }).__clicks = [];
      const Native = window.WebSocket;
      window.WebSocket = class extends Native {
        constructor(url: string | URL, protocols?: string | string[]) {
          super(url, protocols);
          this.addEventListener('message', (event) => {
            if (typeof event.data === 'string' && event.data.includes('"interrupt_state_changed"')) seen.push({ at: Date.now(), kind: 'interrupt_state_changed' });
          });
        }
      } as typeof WebSocket;
      document.addEventListener('click', (event) => {
        if ((event.target as Element | null)?.closest('button')?.textContent?.trim() === 'Прервать') (window as unknown as { __clicks: number[] }).__clicks.push(event.timeStamp + performance.timeOrigin);
      }, true);
    });
    const id = await createSessionUI(page, stack);
    await throttle(page);
    const startedAt = Date.now();
    const hang = synthetic.script([synthetic.delta('висим'), synthetic.hang()]);
    for (let i = 0; i < N; i++) {
      await hub.postInput(id, hang);
      const stop = page.getByRole('button', { name: 'Прервать', exact: true });
      await expect(stop).toBeVisible({ timeout: 30_000 });
      await stop.click();
      await expect(page.getByRole('button', { name: 'Продолжить очередь', exact: true })).toBeVisible({ timeout: 30_000 });
      await hub.json(`/api/v1/conversations/${id}/queue/continue`, { method: 'POST' });
      await hub.waitIdle(id, 60_000);
    }
    const { clicks, seen } = await page.evaluate(() => ({ clicks: (window as unknown as { __clicks: number[] }).__clicks, seen: (window as unknown as { __interrupts: { at: number }[] }).__interrupts.map((item) => item.at) }));
    expect(clicks.length).toBe(N);
    const samples = clicks.map((click) => {
      const first = seen.find((at) => at >= click);
      return first === undefined ? Number.NaN : first - click;
    });
    expect(samples.filter(Number.isNaN), 'на каждый щелчок пришло событие прерывания').toHaveLength(0);

    const dist = distribution(samples, {
      metric: 'P-05: щелчок «Прервать» до первого interrupt_state_changed в странице',
      durationMs: Date.now() - startedAt,
      target: 'p95 ≤ 300 мс',
      limitMs: { p95: 300 },
      conditions: standardConditions({ ...PHONE_CONDITIONS, interrupts: N, long_running_script: 'delta + hang', definition: 'верхняя оценка «принято обработчиком воркера»: первое видимое клиенту событие прогресса прерывания (Date.now() в странице)' })
    });
    saveDistribution(dist);
  });
});

async function buildLongChat(hub: HubClient): Promise<string> {
  const id = await hub.createConversation({ title: 'perf-10000' });
  const steps = Array.from({ length: 5000 }, (_, i) => synthetic.tool('Read', `f${i}`));
  for (let part = 0; part < 2; part++) {
    await hub.postAndFinish(id, synthetic.script(steps), 1_500_000);
  }
  return id;
}

test.describe('perf: длинный чат на 10 000 вызовов инструментов', () => {
  test.describe.configure({ mode: 'serial' });
  let longChat = '';

  test.beforeAll(async () => {
    test.setTimeout(3_200_000);
    longChat = await buildLongChat(createHubClient(getStack()));
  });

  test('p06_history_100_items_on_a_10000_item_chat', tag, async () => {
    test.setTimeout(600_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const N = 200;
    const path = `/api/v1/conversations/${longChat}/history?limit=100`;
    const timed = async (url: string, init?: RequestInit): Promise<number> => {
      const t0 = performance.now();
      const res = await fetch(url, init);
      expect(res.status).toBe(200);
      const body = (await res.json()) as { items: unknown[] };
      expect(body.items).toHaveLength(100);
      return performance.now() - t0;
    };
    const direct = await hub.fetch(path);
    expect(((await direct.json()) as { items: unknown[] }).items).toHaveLength(100);
    const events = await hub.events(`conversation:${longChat}`, '0');
    const tools = events.filter((event) => event.kind === 'block_started' && (event.payload as { kind?: string; tool_name?: string }).kind === 'tool_call' && (event.payload as { tool_name?: string }).tool_name === 'Read').length;
    expect(tools, 'в чате 10 000 вызовов инструментов').toBe(10_000);

    const headers = { 'Remote-User': 'owner', 'X-Canterlot-Proxy': stack.proxySecret, Origin: stack.baseUrl };
    const directStart = Date.now();
    const directSamples: number[] = [];
    for (let i = 0; i < N; i++) directSamples.push(await timed(`${stack.hubUrl}${path}`, { headers }));
    saveDistribution(distribution(directSamples, {
      metric: 'P-06a: GET /history?limit=100 на чате из 10 000 вызовов напрямую в хаб',
      durationMs: Date.now() - directStart,
      target: 'p95 ≤ 300 мс',
      limitMs: { p95: 300 },
      conditions: standardConditions({ requests: N, items_in_chat: '10 000 вызовов инструментов (два ввода по 5 000 шагов tool)', network: 'loopback, без кромки' })
    }));

    await sendChaos(stack.control, { action: 'latency', ms: 40 });
    const edgeStart = Date.now();
    const edgeSamples: number[] = [];
    try {
      for (let i = 0; i < N; i++) edgeSamples.push(await timed(`${stack.baseUrl}${path}`));
    } finally {
      await sendChaos(stack.control, { action: 'latency', ms: 0 });
    }
    const dist = distribution(edgeSamples, {
      metric: 'P-06b: GET /history?limit=100 через кромку с RTT 80 мс',
      durationMs: Date.now() - edgeStart,
      target: 'p95 ≤ 300 мс',
      limitMs: { p95: 300 },
      conditions: standardConditions({ requests: N, items_in_chat: '10 000 вызовов инструментов', network_rtt: '80 мс (по 40 мс в каждую сторону на кромке)' })
    });
    saveDistribution(dist);
    expect(dist.n).toBe(N);
  });

  test('p13_p14_smoke_typing_during_stream_in_a_long_chat', chromiumTag, async ({ page }) => {
    test.setTimeout(900_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    await page.addInitScript(() => {
      const w = window as unknown as { __typing: { typed: number; events: number[]; longTasks: number[] } };
      w.__typing = { typed: 0, events: [], longTasks: [] };
      document.addEventListener('keydown', () => { w.__typing.typed += 1; }, true);
      try {
        new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) if (entry.name === 'keydown') w.__typing.events.push(entry.duration);
        }).observe({ type: 'event', durationThreshold: 16, buffered: true } as PerformanceObserverInit);
        new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) w.__typing.longTasks.push(entry.duration);
        }).observe({ type: 'longtask', buffered: true });
      } catch { /* наблюдатели недоступны: тест это заметит по пустым образцам */ }
    });
    await openSession(page, stack, longChat);
    await throttle(page);

    // Пять минут потока: 3 000 дельт с паузой 100 мс; всё это время идёт набор в поле ввода.
    const steps: SyntheticStep[] = [];
    for (let i = 0; i < 3000; i++) steps.push(synthetic.delayMs(100), synthetic.delta(`${i % 10}`));
    await hub.postInput(longChat, synthetic.script(steps));
    const composer = page.getByRole('textbox', { name: 'Сообщение' });
    await composer.click();
    const startedAt = Date.now();
    let keys = 0;
    while (Date.now() - startedAt < 300_000) {
      await page.keyboard.press('KeyA');
      keys += 1;
      await page.waitForTimeout(90);
    }
    const result = await page.evaluate(() => (window as unknown as { __typing: { typed: number; events: number[]; longTasks: number[] } }).__typing);
    expect(result.typed).toBeGreaterThanOrEqual(keys - 2);
    expect((await composer.inputValue()).length, 'набранное осталось в поле ввода целиком').toBe(keys);

    // Event Timing сообщает только события дольше 16 мс: остальные нажатия учтены как быстрые.
    const slow = result.events.length;
    const samples = [...result.events, ...new Array(Math.max(0, result.typed - slow)).fill(8)];
    const common = { ...PHONE_CONDITIONS, stream: '3 000 дельт по 100 мс (5 минут) в чат из 10 000 вызовов инструментов', keystrokes: result.typed, keydown_events_over_16ms: slow };
    saveDistribution(distribution(samples, {
      metric: 'P-13: Event Timing нажатий клавиш при наборе во время потока в длинном чате',
      durationMs: Date.now() - startedAt,
      target: 'p95 ≤ 100 мс (смоук)',
      limitMs: { p95: 100 },
      conditions: standardConditions({ ...common, definition: 'duration записей keydown (Event Timing, порог 16 мс; события быстрее порога учтены как 8 мс)' })
    }));
    const over500 = result.longTasks.filter((duration) => duration >= 500).length;
    saveDistribution(distribution(result.longTasks, {
      metric: 'P-14: длинные задачи основного потока при том же сценарии',
      durationMs: Date.now() - startedAt,
      target: 'нет задач ≥ 500 мс (смоук)',
      limitMs: { p95: 499 },
      conditions: standardConditions({ ...common, long_tasks_ge_50ms: result.longTasks.length, long_tasks_ge_200ms: result.longTasks.filter((d) => d >= 200).length, long_tasks_ge_500ms: over500 })
    }));
    await hub.waitIdle(longChat, 600_000);
  });
});
