import { bootScreen, SCREENS, type Screen } from '../parity/states';
import { expect, test } from './fixtures';

const screen = (name: string): Screen => SCREENS.find((item) => item.name === name)!;

// F-28: ответ в ветке без кнопок в подвале не должен держать пустую полосу 44 px.
test('F-28: lane answer card without footer actions has equal space above and below its text', async ({ page }) => {
  await bootScreen(page, screen('05-subagent-lane'));
  const cards = page.locator('article.card');
  await expect(cards.first()).toBeVisible();
  const quiet = cards.filter({ has: page.locator('.entry .text'), hasNot: page.locator('.footer-actions > *') });
  const count = await quiet.count();
  expect(count, 'lane has at least one answer card without footer actions').toBeGreaterThan(0);

  for (let index = 0; index < count; index++) {
    const card = quiet.nth(index);
    const box = (await card.boundingBox())!;
    const content = (await card.locator('.entry .text').first().boundingBox())!;
    const byline = (await card.locator('.byline p').boundingBox())!;
    const above = content.y - box.y;
    const below = box.y + box.height - (byline.y + byline.height);
    expect(Math.abs(above - below), `card ${index}: ${above} above, ${below} below`).toBeLessThanOrEqual(2);
  }
});
