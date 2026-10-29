import { test, expect } from '@playwright/test';
import { createHash, randomUUID } from 'node:crypto';
import { open, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { getStack } from './global-setup';
import { synthetic } from './synthetic';
import { answerText, composerOf, createHubClient, createSessionUI, sendButtonOf, sendChaos, sendScript, type HubClient } from './hub';
import { writeSyntheticLimits } from './worker';

const tag = { tag: '@stack' };

/** Резерв недели общий для провайдера и переживает тест: переопределённая сессия снимает его наблюдением ниже порога. */
async function clearReserve(hub: HubClient, overriddenSession: string): Promise<void> {
  const active = async () => (await hub.json<{ provider: string; weekly_reserve_active: boolean }[]>('/api/v1/limits')).filter((item) => item.weekly_reserve_active);
  await hub.postAndFinish(overriddenSession, synthetic.script([synthetic.limitObserved('weekly', 100, 7 * 86_400_000)]), 60_000);
  await expect.poll(async () => (await active()).length, { timeout: 30_000 }).toBe(0);
}

test.describe('stack: limits and attachments', () => {
  test('crunch_five_hour_banner_and_wake', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const resume = `продолжаю после сброса ${randomUUID().slice(0, 8)}`;
    const id = await createSessionUI(page, stack, { agent: 'Синтетический Codex', crunch: true, resumeText: resume });

    await sendScript(page, synthetic.script([synthetic.delta('до лимита'), synthetic.limitReached('five_hour', 7000)]));
    const banner = page.getByText(/Пауза до \d\d:\d\d, затем продолжу автоматически/);
    await expect(banner).toBeVisible({ timeout: 30_000 });
    await expect(page.getByText('Вкладку можно закрыть: продолжение отправит хаб.')).toBeVisible();

    // Лимит «сбросился»: окно новое, использовано мало, сброс позже ожидаемого пробуждения.
    writeSyntheticLimits(stack.workerDataDir!, 'codex', { five_hour: { used_permille: 40, resets_at_ms: Date.now() + 5 * 3_600_000 } });

    await expect(banner).toBeHidden({ timeout: 60_000 });
    await expect(page.locator('article.bubble').filter({ hasText: resume })).toBeVisible({ timeout: 30_000 });
    await expect.poll(async () => (await hub.inputLog(id)).filter((item) => item.text === resume).length, { timeout: 30_000 }).toBe(1);
    await expect.poll(async () => (await hub.textOf(id)).length, { timeout: 30_000 }).toBeGreaterThan('до лимита'.length);
    await hub.waitIdle(id, 60_000);
    const events = await hub.events(`conversation:${id}`, '0');
    expect(events.filter((event) => event.kind === 'crunch_held')).toHaveLength(1);
    const woken = events.filter((event) => event.kind === 'crunch_woken');
    expect(woken).toHaveLength(1);
    expect((woken[0]!.payload as { skipped: unknown }).skipped).toBeNull();
  });

  test('weekly_reserve_banner_and_override', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const idA = await createSessionUI(page, stack, { agent: 'Синтетический Codex' });
    const idB = await hub.createConversation({ harness: 'synthetic-codex' });

    // 96 % недели: резерв включается и держит обе сессии провайдера.
    await sendScript(page, synthetic.script([synthetic.delta('наблюдение'), synthetic.limitObserved('weekly', 960, 3_600_000)]));
    const override = page.getByRole('button', { name: 'Продолжить, тратя резерв', exact: true });
    await expect(override).toBeVisible({ timeout: 30_000 });
    await expect(page.getByText('Оставшийся резерв сохранён для ваших задач.')).toBeVisible();

    // Сообщение в удержанной сессии A встаёт в очередь и не отвечается.
    await sendScript(page, synthetic.script([synthetic.delta('A-после-резерва')]));
    await hub.postInput(idB, synthetic.script([synthetic.delta('B-держится')]));
    await expect.poll(async () => (await hub.detail(idB)).queue.queued).toBe(1);

    await override.click();
    await expect(page.locator('article.card [data-block]').filter({ hasText: 'A-после-резерва' })).toBeVisible({ timeout: 40_000 });
    await expect(override).toBeHidden({ timeout: 15_000 });
    expect((await hub.detail(idB)).queue.queued, 'переопределение действует только на свою сессию').toBe(1);
    expect(await hub.textOf(idB)).not.toContain('B-держится');
    void idA;
    await clearReserve(hub, idA);
  });

  test('attachments_100mib_upload_resume_and_agent_reads_the_checksum', tag, async ({ page }) => {
    test.setTimeout(240_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);

    // Разреженный файл ровно 100 МиБ: сумма считается потоком по тому же содержимому из нулей.
    const path = join(stack.workRoot, 'upload-100mib.bin');
    const size = 100 * 1024 * 1024;
    const handle = await open(path, 'w');
    await handle.truncate(size);
    await handle.close();
    const digest = createHash('sha256');
    const zeros = Buffer.alloc(1024 * 1024);
    for (let i = 0; i < 100; i++) digest.update(zeros);
    const expected = digest.digest('hex');

    // Обрыв посреди передачи: кромка рвёт четвёртый PATCH на 3 МиБ тела, загрузка продолжается с принятого смещения.
    const patches: string[] = [];
    page.on('request', (request) => {
      if (request.method() === 'PATCH' && request.url().endsWith('/content')) patches.push(request.headers()['upload-offset'] ?? '');
    });
    await sendChaos(stack.control, { action: 'cut_during', method: 'PATCH', pathRegex: '/attachments/[^/]+/content$', skip: 3, afterBytes: 3 * 1024 * 1024 });

    await page.locator('[data-composer] input[type="file"]').setInputFiles(path);
    const bar = page.getByRole('progressbar', { name: 'upload-100mib.bin' });
    await expect(bar).toBeVisible({ timeout: 30_000 });
    await expect.poll(async () => Number(await bar.getAttribute('aria-valuenow')), { timeout: 180_000, intervals: [250] }).toBe(size);
    // Порции идут по 8 МиБ; после обрыва четвёртой хаб оставил принятую часть, и следующая начинается с неровного смещения.
    const chunk = 8 * 1024 * 1024;
    const resumed = patches.find((offset) => Number(offset) % chunk !== 0);
    expect(resumed, 'загрузка продолжилась с принятого смещения').toBeDefined();
    expect(Number(resumed)).toBeGreaterThan(3 * chunk);
    expect(Number(resumed)).toBeLessThan(4 * chunk);
    expect(new Set(patches).size, 'ни одно смещение не отправлено дважды').toBe(patches.length);

    await composerOf(page).fill(synthetic.script([synthetic.readAttachment(0)]));
    await expect(sendButtonOf(page)).toHaveAttribute('aria-disabled', 'false', { timeout: 60_000 });
    await sendButtonOf(page).click();

    await expect.poll(() => answerText(page), { timeout: 90_000 }).toBe(`sha256:${expected}`);
    await hub.waitIdle(id);
    expect(await hub.textOf(id)).toBe(`sha256:${expected}`);
    await rm(path, { force: true });
  });

  test('attachment_limits_100mib_plus_one_and_the_eleventh_file', tag, async ({ page }) => {
    test.setTimeout(120_000);
    const stack = getStack();
    const hub = createHubClient(stack);
    const id = await createSessionUI(page, stack);
    const input = page.locator('[data-composer] input[type="file"]');
    const reason = page.locator('[data-composer] .reason');

    // 100 МиБ + 1 байт отклоняется клиентом, в лоток ничего не попадает.
    const big = join(stack.workRoot, 'over-100mib.bin');
    const handle = await open(big, 'w');
    await handle.truncate(100 * 1024 * 1024 + 1);
    await handle.close();
    await input.setInputFiles(big);
    await expect(reason).toContainText('Файл больше 100 МиБ');
    await expect(page.getByRole('progressbar')).toHaveCount(0);
    await rm(big, { force: true });

    // Хаб держит тот же предел независимо от клиента.
    const refused = await hub.fetch('/api/v1/attachments', {
      method: 'POST',
      body: JSON.stringify({ conversation_id: id, name: 'over.bin', size: 100 * 1024 * 1024 + 1, sha256: '0'.repeat(64) })
    });
    expect(refused.status).toBeGreaterThanOrEqual(400);
    expect(refused.status).toBeLessThan(500);

    // Десять файлов принимаются, одиннадцатый нет.
    const files: string[] = [];
    for (let i = 0; i < 11; i++) {
      const file = join(stack.workRoot, `small-${String(i).padStart(2, '0')}.txt`);
      await writeFile(file, `файл ${i}`);
      files.push(file);
    }
    await input.setInputFiles(files.slice(0, 10));
    await expect(page.getByRole('progressbar')).toHaveCount(10);
    await input.setInputFiles(files[10]!);
    await expect(reason).toContainText('Не более 10 файлов в одном сообщении');
    await expect(page.getByRole('progressbar')).toHaveCount(10);
    await Promise.all(files.map((file) => rm(file, { force: true })));
  });
});
