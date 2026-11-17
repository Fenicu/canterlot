import { mkdirSync } from 'node:fs';
import type { Locator, Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { streamCall } from '../../testing/fixtures/feed-cards';
import { LONG_COMMAND, seedToolCards, type ToolCardsSeed } from '../../testing/fixtures/tool-cards';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000c1';
const SHOTS = '/tmp/bubbles-shots';

async function boot(page: Page, opts: { working?: boolean } = {}): Promise<{ hub: FakeHub; api: FakeHubApi; seed: ToolCardsSeed }> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const seed = seedToolCards(api, opts);
  if (opts.working) api.details.get(seed.id)!.activity = { state: 'working', evidence: {}, since: new Date().toISOString() };
  // Буфер обмена подменяется: проверка смотрит, что именно ушло в него, в любом браузере.
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async (text: string) => { (window as unknown as { __copied?: string }).__copied = text; } } });
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${seed.id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.feed [data-row]').first()).toBeVisible({ timeout: 15_000 });
  return { hub, api, seed };
}

const feed = (page: Page) => page.locator('.feed');
const row = (page: Page, id: string) => page.locator(`[data-key="block:${id}"]`);

/** Лента виртуальная: строка далеко от низа не смонтирована, пока до неё не докрутили. */
async function reveal(page: Page, target: Locator): Promise<Locator> {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await feed(page).evaluate((node) => {
    node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 }));
    node.scrollTop = 0;
  });
  await settle();
  for (let step = 0; step < 60; step++) {
    if ((await target.count()) > 0) {
      try {
        await target.first().scrollIntoViewIfNeeded({ timeout: 2_000 });
        await settle();
        if ((await target.count()) > 0) return target.first();
      } catch {
        // Строку сняли с окна, пока до неё крутили: ищем дальше.
      }
    }
    await feed(page).evaluate((node) => {
      node.scrollTop += node.clientHeight * 0.6;
    });
    await settle();
  }
  await expect(target.first()).toBeAttached();
  return target.first();
}

async function cardOf(page: Page, id: string): Promise<Locator> {
  return reveal(page, row(page, id).locator('.feed-card'));
}

async function shot(page: Page, target: Locator, name: string, opts: { moving?: boolean } = {}): Promise<void> {
  if (test.info().project.name !== 'phone') return;
  mkdirSync(SHOTS, { recursive: true });
  await page.mouse.move(0, 0);
  await target.screenshot({ path: `${SHOTS}/${name}.png`, animations: opts.moving ? 'allow' : 'disabled' });
}

test.describe('shell card', () => {
  test('shell_card_shows_the_formatted_highlighted_command_and_copies_the_original', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.shellLong!);
    await expect(card).toHaveAttribute('data-card', 'shell.exec');
    await expect(card.locator('[data-role="shell-title"]')).toHaveText('Проверки веба: токены и юнит-тесты');
    const rows = card.locator('pre.code .row');
    await expect(rows).toHaveText(['cd web', '  && npm ci --no-audit --no-fund', '  && npm run lint:tokens', '  && npm run test:unit -- --reporter=dot 2>&1', '    | tail -20']);
    // Подсветка приходит ленивым модулем highlight.js: имена команд красятся токеном.
    await expect(card.locator('pre.code [class*="hljs-"]').first()).toBeAttached();
    await card.getByRole('button', { name: 'Скопировать команду' }).click();
    await expect(card.locator('.status[role="status"]')).toHaveText('Скопировано');
    expect(await page.evaluate(() => (window as unknown as { __copied?: string }).__copied)).toBe(LONG_COMMAND);
    await shot(page, card, 'shell-phone');
  });

  test('shell_output_is_collapsed_and_expands_to_its_tail', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.shellLong!);
    await expect(card.locator('[data-role="card-fold"]')).toHaveCount(0);
    await expect(card.locator('[data-role="shell-meta"]')).toContainText('вывод · 34 строки');
    const head = card.locator('.head .hit');
    await expect(head).toHaveAttribute('aria-label', 'Проверки веба: токены и юнит-тесты. Показать вывод');
    await head.click();
    const fold = card.locator('[data-role="card-fold"]');
    await expect(fold.locator('pre')).toContainText('Duration  4.21s');
    await expect(fold.locator('pre')).not.toContainText('case-1.test.ts');
    await expect(fold.getByRole('button', { name: /Показать полностью/ })).toBeVisible();
    await shot(page, card, 'shell-expanded-phone');
    await head.click();
    await expect(fold).toHaveCount(0);
  });

  test('exit_codes_denials_and_cwd_read_in_their_tones', async ({ page }) => {
    const { seed } = await boot(page);
    const failed = await cardOf(page, seed.blocks.shellFailed!);
    await expect(failed.locator('[data-role="card-state"]')).toHaveText('код 1');
    await expect(failed.locator('[data-role="card-state"]')).toHaveAttribute('data-tone', 'danger');
    await expect(failed.locator('[data-role="shell-title"]')).toHaveText('Команда');
    await shot(page, failed, 'shell-failed-phone');
    const pipeline = await cardOf(page, seed.blocks.shellPipeline!);
    await expect(pipeline.locator('[data-role="card-state"]')).toHaveCount(0);
    await expect(pipeline.locator('[data-role="shell-meta"]')).toContainText('код 0');
    await expect(pipeline.locator('pre.code .row')).toHaveCount(6);
    const denied = await cardOf(page, seed.blocks.shellDenied!);
    await expect(denied.locator('[data-role="card-state"]')).toHaveText('отклонено');
    await expect(denied.locator('[data-role="card-state"]')).toHaveAttribute('data-tone', 'warning');
    const cwd = await cardOf(page, seed.blocks.shellCwd!);
    await expect(cwd.locator('[data-role="shell-cwd"]')).toHaveText('/home/owner/canterlot');
  });

  test('a_running_command_reads_as_running_and_settles_with_its_output', async ({ page }) => {
    const { hub, api, seed } = await boot(page, { working: true });
    const fresh = streamCall(hub, api, seed.id, seed.run, 'Bash', { command: 'npm run build', description: 'Сборка веба' });
    const card = row(page, fresh.call).locator('.feed-card');
    await expect(card.locator('[data-role="card-state"]')).toHaveText('идёт');
    await expect(card).toHaveAttribute('data-state', 'running');
    streamCall(hub, api, seed.id, seed.run, 'Bash', null, { blockId: fresh.call, resultId: fresh.result, result: 'built in 3.1s' });
    await expect(card).toHaveAttribute('data-state', 'done');
    await expect(card.locator('[data-role="card-state"]')).toHaveCount(0);
  });

  test('a_long_command_scrolls_inside_its_block_not_the_page_at_360px', async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 760 });
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.shellLong!);
    const code = card.locator('pre.code');
    const sizes = await code.evaluate((node) => ({ scroll: node.scrollWidth, client: node.clientWidth }));
    expect(sizes.scroll).toBeGreaterThan(sizes.client);
    const page360 = await page.evaluate(() => ({ scroll: document.documentElement.scrollWidth, client: document.documentElement.clientWidth }));
    expect(page360.scroll).toBeLessThanOrEqual(page360.client);
    const feedWidth = await feed(page).evaluate((node) => node.scrollWidth - node.clientWidth);
    expect(feedWidth).toBeLessThanOrEqual(0);
  });
});

/** Имена анимаций на блике навыка: пусто — имя стоит спокойно. */
async function glints(card: Locator): Promise<string[]> {
  return card.locator('[data-role="skill-name"]').evaluate((node) => node.getAnimations({ subtree: true }).map((item) => (item as CSSAnimation).animationName));
}

test.describe('skill card', () => {
  test('a_finished_skill_shows_its_name_plugin_and_args_and_stands_still', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.skillDone!);
    await expect(card).toHaveAttribute('data-card', 'skill');
    await expect(card.locator('[data-role="skill-name"]')).toHaveText('test-driven-development');
    await expect(card.locator('[data-role="skill-plugin"]')).toHaveText('superpowers');
    await expect(card.locator('[data-role="skill-args"]')).toHaveText('карточки ленты: формат команды и счёт строк');
    await expect(card.locator('[data-role="card-state"]')).toHaveCount(0);
    expect(await glints(card)).toEqual([]);
    const opencode = await cardOf(page, seed.blocks.skillOpencode!);
    await expect(opencode.locator('[data-role="skill-name"]')).toHaveText('frontend-design');
    await shot(page, card, 'skill-phone');
  });

  for (const motion of ['full', 'reduced'] as const) {
    test(`a_running_skill_glints_until_it_loads_${motion}`, async ({ page }) => {
      if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
      const { hub, api, seed } = await boot(page, { working: true });
      const fresh = streamCall(hub, api, seed.id, seed.run, 'Skill', { skill: 'antislop:antislop-ui' });
      const card = row(page, fresh.call).locator('.feed-card');
      await expect(card.locator('[data-role="card-state"]')).toHaveText('идёт');
      await expect(card.locator('[data-role="skill-name"]')).toHaveAttribute('data-glint', '');
      if (motion === 'full') {
        await expect.poll(async () => (await glints(card)).map((name) => name.replace(/^svelte-\w+-/, ''))).toEqual(['skill-glint']);
        // Снимок посреди прохода блика: полоса акцента на имени.
        await page.waitForTimeout(700);
        await shot(page, card, 'skill-running-phone', { moving: true });
      } else {
        expect(await glints(card)).toEqual([]);
      }
      streamCall(hub, api, seed.id, seed.run, 'Skill', null, { blockId: fresh.call, resultId: fresh.result, result: 'Launching skill: antislop:antislop-ui' });
      await expect(card.locator('[data-role="card-state"]')).toHaveCount(0);
      await expect(card.locator('[data-role="skill-name"]')).not.toHaveAttribute('data-glint');
      expect(await glints(card)).toEqual([]);
    });
  }
});

test.describe('edit and write cards', () => {
  test('an_edit_is_compact_by_default_and_expands_to_a_highlighted_diff', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.editClaude!);
    await expect(card).toHaveAttribute('data-card', 'file.edit');
    await expect(card.locator('[data-role="edit-kind"]')).toHaveText('Правка');
    await expect(card.locator('[data-role="edit-name"]')).toHaveText('registry.ts');
    await expect(card.locator('[data-role="edit-dir"]')).toHaveText('…/timeline/cards');
    await expect(card.locator('[data-role="edit-counts"]')).toHaveText('+3−1');
    await expect(card.locator('pre.code')).toHaveCount(0);
    await shot(page, card, 'edit-phone');
    const head = card.locator('.head .hit');
    await expect(head).toHaveAttribute('aria-label', 'Правка registry.ts · +3 −1. Показать изменения');
    await head.click();
    const code = card.locator('[data-role="card-fold"] pre.code');
    await expect(code.locator('.row[data-kind="add"]')).toHaveCount(3);
    await expect(code.locator('.row[data-kind="del"]')).toHaveCount(1);
    await expect(code.locator('.row[data-kind="del"]')).toContainText('skill: card(skill),');
    await expect(code.locator('[class*="hljs-"]').first()).toBeAttached();
    const backgrounds = await code.evaluate((node) => {
      const probe = document.createElement('span');
      node.append(probe);
      probe.style.color = 'var(--success-soft)';
      const success = getComputedStyle(probe).color;
      probe.style.color = 'var(--danger-soft)';
      const danger = getComputedStyle(probe).color;
      probe.remove();
      return {
        add: getComputedStyle(node.querySelector('.row[data-kind="add"]')!).backgroundColor === success,
        del: getComputedStyle(node.querySelector('.row[data-kind="del"]')!).backgroundColor === danger
      };
    });
    expect(backgrounds).toEqual({ add: true, del: true });
    await shot(page, card, 'edit-expanded-phone');
    await head.click();
    await expect(card.locator('pre.code')).toHaveCount(0);
  });

  test('a_new_file_reads_as_added_and_its_long_content_is_cut_with_show_all', async ({ page }) => {
    const { seed } = await boot(page);
    const card = await cardOf(page, seed.blocks.writeClaude!);
    await expect(card).toHaveAttribute('data-card', 'file.write');
    await expect(card.locator('[data-role="edit-kind"]')).toHaveText('Новый файл');
    await expect(card.locator('[data-role="edit-kind"]')).toHaveAttribute('data-kind', 'add');
    await expect(card.locator('[data-role="edit-counts"]')).toHaveText('+72');
    await shot(page, card, 'write-phone');
    await card.locator('.head .hit').click();
    const code = card.locator('[data-role="card-fold"] pre.code');
    await expect(code.locator('.row')).toHaveCount(60);
    await expect(code.locator('.sign')).toHaveCount(0);
    const more = card.getByRole('button', { name: 'Показать всё · 72 строки' });
    await more.click();
    await expect(code.locator('.row')).toHaveCount(72);
    await expect(card.getByRole('button', { name: 'Свернуть' })).toBeVisible();
  });

  test('opencode_and_codex_shapes_render_the_same_card', async ({ page }) => {
    const { seed } = await boot(page);
    const opencode = await cardOf(page, seed.blocks.editOpencode!);
    await expect(opencode.locator('[data-role="edit-name"]')).toHaveText('limit.py');
    await expect(opencode.locator('[data-role="edit-counts"]')).toHaveText('+3−2');
    const codex = await cardOf(page, seed.blocks.patchCodex!);
    await expect(codex).toHaveAttribute('data-card', 'file.patch');
    await expect(codex.locator('[data-role="edit-name"]')).toHaveText('events.rs');
    await expect(codex.locator('[data-role="edit-counts"]')).toHaveText('+2−1');
    await codex.locator('.head .hit').click();
    await expect(codex.locator('pre.code .row[data-kind="hunk"]')).toContainText('@@ -12,6 +12,7 @@');
    await shot(page, codex, 'patch-codex-expanded-phone');
    const added = await cardOf(page, seed.blocks.patchCodexAdd!);
    await expect(added.locator('[data-role="edit-kind"]')).toHaveText('Новый файл');
    await expect(added.locator('[data-role="edit-counts"]')).toHaveText('+3');
    const envelope = await cardOf(page, seed.blocks.patchEnvelope!);
    await expect(envelope.locator('[data-role="edit-name"]')).toHaveText('copy.ts');
    await expect(envelope).toContainText('и ещё 1');
    await expect(envelope.locator('[data-role="edit-counts"]')).toHaveText('+4−1');
    await envelope.locator('.head .hit').click();
    await expect(envelope.locator('[data-role="edit-file"]')).toHaveCount(2);
    await expect(envelope.locator('[data-role="edit-file"]').first()).toContainText('+2−1');
    await expect(envelope.locator('[data-role="card-fold"] pre.code')).toHaveCount(2);
    await shot(page, envelope, 'patch-envelope-expanded-phone');
  });

  test('a_running_edit_awaiting_permission_keeps_its_card_and_the_request_card', async ({ page }) => {
    const { hub, api, seed } = await boot(page, { working: true });
    const fresh = streamCall(hub, api, seed.id, seed.run, 'Edit', { file_path: '/w/src/a.ts', old_string: 'a', new_string: 'b' });
    const card = row(page, fresh.call).locator('.feed-card');
    await expect(card.locator('[data-role="card-state"]')).toHaveText('идёт');
    await expect(card.locator('[data-role="edit-counts"]')).toHaveText('+1−1');
    hub.append(`conversation:${seed.id}`, { kind: 'request_opened', conversation_id: seed.id, payload: { request_id: api.id(), kind: 'permission', payload: { tool_name: 'Edit', input: { file_path: '/w/src/a.ts', old_string: 'a', new_string: 'b' } } } });
    hub.flush();
    await expect(page.locator('.permission-card')).toBeVisible();
    await expect(card.locator('[data-role="card-state"]')).toHaveText('идёт');
    streamCall(hub, api, seed.id, seed.run, 'Edit', null, { blockId: fresh.call, resultId: fresh.result, result: 'The file /w/src/a.ts has been updated.' });
    await expect(card.locator('[data-role="card-state"]')).toHaveCount(0);
  });
});

/** Свёрнутая строка «Изучает код», где первым стоит этот вызов. */
async function exploreRow(page: Page, firstCall: string): Promise<Locator> {
  return reveal(page, page.locator(`[data-key="quiet:block:${firstCall}"] [data-role="quiet-group"][data-family="explore"]`));
}

test.describe('reads and searches', () => {
  test('consecutive_reads_and_searches_fold_into_one_row_and_expand_to_compact_rows', async ({ page }) => {
    const { seed } = await boot(page);
    const group = await exploreRow(page, seed.blocks.readSteps!);
    const head = group.locator(':scope > .head');
    await expect(head).toHaveText('Изучила код: 3 файла, 2 поиска');
    await expect(head).toHaveAttribute('aria-expanded', 'false');
    await shot(page, group, 'explore-phone');
    await head.click();
    await expect(group.locator('.feed-card')).toHaveCount(6);
    const read = group.locator(`[data-key="block:${seed.blocks.readSteps}"] .feed-card`);
    await expect(read).toHaveAttribute('data-quiet', '');
    await expect(read.locator('[data-role="read-name"]')).toHaveText('steps.ts');
    await expect(read.locator('[data-role="read-dir"]')).toHaveText('…/lib/timeline');
    await expect(read.locator('[data-role="read-range"]')).toHaveText('стр. 120–180');
    const grep = group.locator(`[data-key="block:${seed.blocks.grepClaude}"] .feed-card`);
    await expect(grep.locator('[data-role="find-pattern"]')).toHaveText('foldSteps\\(');
    await expect(grep.locator('[data-role="find-where"]')).toHaveText('в web/src · *.svelte');
    await expect(grep.locator('[data-role="find-count"]')).toHaveText('2 файла');
    const glob = group.locator(`[data-key="block:${seed.blocks.globOpencode}"] .feed-card`);
    await expect(glob.locator('[data-role="find-count"]')).toHaveText('24 файла');
    await shot(page, group, 'explore-expanded-phone');
    await head.click();
    await expect(group.locator('.feed-card')).toHaveCount(0);
  });

  test('a_lone_read_is_a_quiet_row_and_a_failed_read_marks_its_fold', async ({ page }) => {
    const { seed } = await boot(page);
    const single = await cardOf(page, seed.blocks.readSingle!);
    await expect(single).toHaveAttribute('data-card', 'file.read');
    await expect(single.locator('[data-role="read-range"]')).toHaveText('стр. 80–99');
    const failed = await exploreRow(page, seed.blocks.readOk!);
    await expect(failed.locator(':scope > .head')).toContainText('Изучила код: 2 файла');
    await expect(failed.locator('[data-role="quiet-state"]')).toHaveText('ошибка 1');
    await expect(failed.locator('[data-role="quiet-state"]')).toHaveAttribute('data-tone', 'danger');
  });

  test('a_running_read_awaiting_permission_stays_outside_the_fold', async ({ page }) => {
    const { hub, api, seed } = await boot(page, { working: true });
    const first = streamCall(hub, api, seed.id, seed.run, 'Read', { file_path: '/w/src/a.ts' }, { result: '1\ta' });
    streamCall(hub, api, seed.id, seed.run, 'Grep', { pattern: 'needle', path: '/w/src' }, { result: 'Found 1 file\n/w/src/a.ts' });
    const waiting = streamCall(hub, api, seed.id, seed.run, 'Read', { file_path: '/home/owner/.ssh/config' });
    hub.append(`conversation:${seed.id}`, { kind: 'request_opened', conversation_id: seed.id, payload: { request_id: api.id(), kind: 'permission', payload: { tool_name: 'Read', input: { file_path: '/home/owner/.ssh/config' } } } });
    hub.flush();
    await expect(page.locator('.permission-card')).toBeVisible();
    const group = page.locator(`[data-key="quiet:block:${first.call}"] [data-role="quiet-group"]`);
    await expect(group.locator(':scope > .head')).toContainText('Изучает код: 1 файл, 1 поиск');
    await expect(group.locator('[data-role="quiet-state"]')).toHaveText('идёт');
    const row = page.locator(`[data-key="block:${waiting.call}"] .feed-card`);
    await expect(row).toBeVisible();
    await expect(row.locator('[data-role="card-state"]')).toHaveText('идёт');
    await expect(page.locator(`[data-role="quiet-group"] [data-key="block:${waiting.call}"]`)).toHaveCount(0);
    await shot(page, page.locator('.feed article.card').last(), 'explore-running-permission-phone');
  });
});
