import type { Page } from '@playwright/test';
import { expect, test } from './fixtures';
import {
  area,
  bootDictation,
  micButton,
  panel,
  panelState,
  startDictation
} from './dictation-fixture';

async function watchInputs(page: Page) {
  const hits: string[] = [];
  await page.route('**/api/v1/conversations/*/inputs', async (route) => {
    hits.push(route.request().method());
    await route.fallback();
  });
  return hits;
}

test('mic_button_appears_only_when_the_service_is_available', async ({ page }) => {
  await page.addInitScript(() => localStorage.removeItem('canterlot.dictation'));
  const { hub } = await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    }
  });
  await expect(micButton(page)).toBeVisible();
  expect(hub.dictation.status.available).toBe(true);
});

for (const reason of ['disabled', 'no_service', 'unhealthy'] as const) {
  test(`mic_button_is_hidden_when_the_service_reports_${reason}`, async ({ page }) => {
    await bootDictation(page, {
      configure: (dictation) => {
        dictation.status = { available: false, reason, mode: null, engine: null };
      }
    });
    await expect(micButton(page)).toHaveCount(0);
  });
}

test('mic_button_is_hidden_when_the_local_setting_is_off', async ({ page }) => {
  await bootDictation(page, {
    pref: 'off',
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    }
  });
  await expect(micButton(page)).toHaveCount(0);
});

test('panel_names_loading_then_listening', async ({ page }) => {
  const { hub } = await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.state = 'loading';
      dictation.holdLoading = true;
    }
  });
  await startDictation(page);
  await expect(panelState(page)).toHaveText(/Подключаю микрофон|Загружаю модель/);
  hub.dictation.releaseLoading();
  await expect(panelState(page)).toHaveText('Слушаю');
  await expect(page.locator('[data-dictation-timer]')).toHaveText(/^\d{2}:\d{2}$/);
});

test('cpu_mode_is_named_slow_and_says_text_comes_after_pauses', async ({ page }) => {
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'cpu', engine: 'warm' };
      dictation.mode = 'cpu';
    }
  });
  await startDictation(page);
  await expect(panelState(page)).toHaveText('Медленный режим');
  await expect(page.locator('[data-dictation-slow-mode]')).toHaveText('Текст появляется после пауз');
});

test('live_text_keeps_committed_normal_and_tentative_muted', async ({ page }) => {
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.script = {
        texts: [{ afterMs: 20, committed: 'Привет,', tentative: 'это проверка' }]
      };
    }
  });
  await startDictation(page);
  const committed = page.locator('[data-dictation-committed]');
  const tentative = page.locator('[data-dictation-tentative]');
  await expect(committed).toHaveText('Привет,');
  await expect(tentative).toHaveText('это проверка');
  const { committedColor, tentativeColor, textColor } = await page.evaluate(() => {
    const read = (selector: string) => getComputedStyle(document.querySelector(selector)!).color;
    return {
      committedColor: read('[data-dictation-committed]'),
      tentativeColor: read('[data-dictation-tentative]'),
      textColor: getComputedStyle(document.documentElement).getPropertyValue('--text').trim()
    };
  });
  expect(committedColor).not.toBe(tentativeColor);
  expect(textColor.length).toBeGreaterThan(0);
});

test('done_inserts_at_the_caret_and_the_draft_survives_reload', async ({ page }) => {
  const { api, id } = await bootDictation(page, {
    draft: 'Начало текста',
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.finalText = 'вставка';
    }
  });
  const hits = await watchInputs(page);
  const field = area(page);
  await expect(field).toHaveValue('Начало текста');
  await field.evaluate((element) => {
    const node = element as HTMLTextAreaElement;
    node.focus();
    node.setSelectionRange(6, 6);
  });
  await startDictation(page);
  await expect(panelState(page)).toHaveText('Слушаю');
  await page.getByRole('button', { name: 'Готово' }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect(field).toHaveValue('Начало вставка текста');
  const caret = await field.evaluate((element) => (element as HTMLTextAreaElement).selectionStart);
  expect(caret).toBe('Начало вставка'.length);
  expect(hits).toHaveLength(0);

  await expect.poll(() => api.draft(id).text).toBe('Начало вставка текста');
  await page.reload();
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(area(page)).toHaveValue('Начало вставка текста');
});

test('cancel_discards_the_recognition_and_keeps_the_draft', async ({ page }) => {
  await bootDictation(page, {
    draft: 'черновик',
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.script = { texts: [{ afterMs: 20, committed: 'привет', tentative: '' }] };
    }
  });
  await startDictation(page);
  await expect(page.locator('[data-dictation-committed]')).toHaveText('привет');
  await page.getByRole('button', { name: 'Отмена' }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect(area(page)).toHaveValue('черновик');
});

test('connection_loss_offers_the_partial_text_without_inserting_it', async ({ page }) => {
  await bootDictation(page, {
    draft: 'черновик',
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.script = {
        texts: [{ afterMs: 30, committed: 'Привет', tentative: ' мир' }],
        closeAfterMs: 120
      };
    }
  });
  await startDictation(page);
  await expect(panelState(page)).toHaveText('Связь прервалась');
  await expect(page.getByRole('button', { name: 'Вставить что есть (неполно)' })).toBeVisible();
  await expect(area(page)).toHaveCount(0);
  await page.getByRole('button', { name: 'Вставить что есть (неполно)' }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect(area(page)).toHaveValue('черновик Привет мир');
});

test('finish_waits_for_the_final_and_offers_the_partial_on_timeout', async ({ page }) => {
  test.setTimeout(30_000);
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.script = { texts: [{ afterMs: 20, committed: 'Привет', tentative: '' }] };
      dictation.finish = 'silent';
    }
  });
  await startDictation(page);
  await expect(page.locator('[data-dictation-committed]')).toHaveText('Привет');
  await page.getByRole('button', { name: 'Готово' }).click();
  await expect(panelState(page)).toHaveText('Распознаю…');
  await expect(page.getByRole('button', { name: 'Вставить что есть (неполно)' })).toBeVisible({
    timeout: 8_000
  });
  await expect(page.locator('[data-dictation-state]')).toHaveText('Не дождались итога распознавания');
  await page.getByRole('button', { name: 'Вставить что есть (неполно)' }).click();
  await expect(area(page)).toHaveValue('Привет');
});

test('denied_microphone_explains_how_to_allow_it', async ({ page }) => {
  await bootDictation(page, {
    mic: 'NotAllowedError',
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    }
  });
  await startDictation(page);
  await expect(panelState(page)).toHaveText(/Разрешите микрофон в настройках/);
  await page.getByRole('button', { name: 'Отмена' }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect(micButton(page)).toBeVisible();
});

test('missing_microphone_hides_the_button_for_the_session', async ({ page }) => {
  await bootDictation(page, {
    mic: 'NotFoundError',
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    }
  });
  await startDictation(page);
  await expect(panelState(page)).toHaveText('Микрофон не найден');
  await page.getByRole('button', { name: 'Отмена' }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect(micButton(page)).toHaveCount(0);

  // Микрофон подключили: проверка устройств возвращает кнопку без перезагрузки.
  await page.evaluate(() => {
    Object.defineProperty(navigator.mediaDevices, 'enumerateDevices', {
      configurable: true,
      value: async () => [{ kind: 'audioinput', deviceId: 'mic', groupId: 'group', label: 'Микрофон' }]
    });
    navigator.mediaDevices.dispatchEvent(new Event('devicechange'));
  });
  await expect(micButton(page)).toBeVisible();
});

test('too_long_final_inserts_the_text_and_shows_the_notice', async ({ page }) => {
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.finalText = 'Долгая речь';
      dictation.script = { tooLongAfterMs: 120 };
    }
  });
  await startDictation(page);
  await expect(page.locator('[data-dictation-notice]')).toHaveText('Достигнут предел 10 минут');
  await expect(panel(page)).toHaveCount(0);
  await expect(area(page)).toHaveValue('Долгая речь');
});

test('dictation_panel_follows_the_visual_language', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await bootDictation(page, {
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
      dictation.script = {
        texts: [{ afterMs: 20, committed: 'Привет,', tentative: 'это проверка' }]
      };
    }
  });
  await startDictation(page);
  await expect(page.locator('[data-dictation-tentative]')).toBeVisible();
  const audit = await page.locator('[data-dictation]').evaluate((root) => {
    const borders: string[] = [];
    const durations: string[] = [];
    for (const element of [root, ...root.querySelectorAll('*')]) {
      const style = getComputedStyle(element);
      for (const side of ['Top', 'Right', 'Bottom', 'Left'] as const) {
        const width = style.getPropertyValue(`border-${side.toLowerCase()}-width`);
        if (width !== '0px') borders.push(`${element.tagName}.${element.className}:${width}`);
      }
      durations.push(style.transitionDuration);
    }
    return { borders, durations };
  });
  expect(audit.borders).toEqual([]);
  expect(audit.durations.every((value) => value === '0s')).toBe(true);
  const text = await page.locator('[data-dictation]').innerText();
  expect(text).not.toMatch(/\p{Extended_Pictographic}/u);
  const overflow = await page.evaluate(() => ({
    scrollWidth: document.documentElement.scrollWidth,
    clientWidth: document.documentElement.clientWidth
  }));
  expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth);
});
