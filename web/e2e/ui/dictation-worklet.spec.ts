import { expect, test } from './fixtures';
import { bootDictation, panel, panelState, startDictation } from './dictation-fixture';

// Проект dictation-media запускает Chromium с поддельным микрофоном и тоном из файла:
// здесь проверяется настоящий AudioWorklet, а не заглушка getUserMedia.
test('real_worklet_streams_frames_and_flushes_the_last_partial_frame', async ({ page }) => {
  test.setTimeout(30_000);
  const { hub } = await bootDictation(page, {
    mic: 'device',
    configure: (dictation) => {
      dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    }
  });
  await startDictation(page);
  await expect(panelState(page)).toHaveText('Слушаю');

  await expect.poll(() => hub.dictation.connections.length).toBe(1);
  await expect.poll(() => hub.dictation.connections[0]!.frames.length, { timeout: 8_000 }).toBeGreaterThan(2);
  // Полтора кадра: последний кусок гарантированно окажется неполным после «Готово».
  await page.waitForTimeout(150);
  await page.getByRole('button', { name: 'Готово' }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect.poll(() => hub.dictation.connections[0]!.finish).toBe(true);

  const connection = hub.dictation.connections[0]!;
  expect(connection.start).toMatchObject({ type: 'start', sample_rate: 16_000, format: 's16le' });
  expect(String(connection.start!.client)).toMatch(/^pwa\/\d+\.\d+\.\d+/);

  const frames = connection.frames;
  expect(frames.length).toBeGreaterThan(2);
  for (const frame of frames) {
    expect(frame.length).toBeGreaterThan(0);
    expect(frame.length % 2).toBe(0);
    expect(frame.length).toBeLessThanOrEqual(8_192);
  }
  const full = frames.slice(0, -1);
  expect(full.every((frame) => frame.length === 3_200)).toBe(true);
  const tail = frames.at(-1)!;
  expect(tail.length).toBeLessThan(3_200);
  expect(tail.length % 2).toBe(0);

  // Тон дошёл до хаба: worklet не просто шлёт тишину.
  const peak = Math.max(
    ...connection.frames.map((frame) => {
      let max = 0;
      for (let offset = 0; offset + 1 < frame.length; offset += 2) {
        max = Math.max(max, Math.abs(new DataView(frame.buffer, frame.byteOffset).getInt16(offset, true)));
      }
      return max;
    })
  );
  expect(peak).toBeGreaterThan(1_000);
});
