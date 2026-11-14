import { describe, expect, it } from 'vitest';
import { composerPlaceholder, sendActionLabel } from '#lib/composer/copy.js';

describe('composer/copy', () => {
  it('promises_a_queue_while_a_turn_runs', () => {
    expect(composerPlaceholder(true, null)).toBe('Ход идёт: сообщение встанет в очередь');
    expect(composerPlaceholder(true, 'Подсказка')).toBe('Ход идёт: сообщение встанет в очередь');
  });

  it('keeps_the_usual_placeholder_and_suggestion_when_idle', () => {
    expect(composerPlaceholder(false, null)).toBe('Сообщение агенту…');
    expect(composerPlaceholder(false, 'Подсказка')).toBe('Подсказка');
  });

  it('names_the_send_button_by_what_it_does', () => {
    expect(sendActionLabel(false)).toBe('Отправить');
    expect(sendActionLabel(true)).toBe('В очередь');
  });
});
