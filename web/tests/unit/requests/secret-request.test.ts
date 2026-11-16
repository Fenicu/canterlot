import { describe, expect, it } from 'vitest';
import { fmt } from '#lib/format/ru.js';
import {
  SECRET_REQUEST_VALUE_LIMIT,
  isSecretRequestDocked,
  isSecretValueTooLarge,
  secretRequestCanRevoke,
  secretRequestLine,
  secretRequestShowsField,
  type SecretRequestState,
  type SecretRequestView
} from '#lib/requests/secretRequest.js';


function view(patch: Partial<SecretRequestView> = {}): SecretRequestView {
  return {
    id: 'sr-1',
    conversation_id: 'conv-1',
    name: 'Токен деплоя',
    purpose: 'Загрузить сборку на сервер',
    state: 'open',
    created_at: '2026-11-16T11:55:00.000Z',
    updated_at: '2026-11-16T11:55:00.000Z',
    revision: 0,
    ...patch
  };
}

describe('requests/secretRequest', () => {
  it('maps_every_state_to_the_owner_facing_line', () => {
    const expires = '2026-11-16T11:30:00.000Z';
    const lines: Record<SecretRequestState, string> = {
      open: '',
      sending: 'Передаю на машину…',
      delivered: `Передан · файл до ${fmt.clock(Date.parse(expires))}`,
      declined: 'Отказано',
      revoking: 'Отзываю…',
      revoked: 'Отозван',
      expired: 'Истёк',
      lost: 'Значение не дошло до машины, введите снова',
      cancelled: 'Отменён'
    };
    for (const [state, line] of Object.entries(lines) as [SecretRequestState, string][]) {
      expect(secretRequestLine(view({ state, expires_at: expires }))).toBe(line);
    }
  });

  it('shows_the_field_only_where_the_value_can_be_typed_again', () => {
    expect(secretRequestShowsField(view({ state: 'open' }))).toBe(true);
    expect(secretRequestShowsField(view({ state: 'lost' }))).toBe(true);
    expect(secretRequestShowsField(view({ state: 'sending' }))).toBe(false);
    expect(secretRequestShowsField(view({ state: 'delivered' }))).toBe(false);
    expect(secretRequestShowsField(view({ state: 'declined' }))).toBe(false);
  });

  it('docks_open_and_lost_requests_at_the_composer_and_leaves_the_rest_to_the_feed', () => {
    expect(isSecretRequestDocked(view({ state: 'open' }))).toBe(true);
    expect(isSecretRequestDocked(view({ state: 'lost' }))).toBe(true);
    for (const state of ['sending', 'delivered', 'declined', 'revoking', 'revoked', 'expired', 'cancelled'] as SecretRequestState[]) {
      expect(isSecretRequestDocked(view({ state }))).toBe(false);
    }
  });

  it('offers_revoke_only_for_a_delivered_value', () => {
    expect(secretRequestCanRevoke(view({ state: 'delivered' }))).toBe(true);
    expect(secretRequestCanRevoke(view({ state: 'open' }))).toBe(false);
    expect(secretRequestCanRevoke(view({ state: 'revoked' }))).toBe(false);
  });

  it('checks_the_value_in_utf8_bytes_not_characters', () => {
    expect(SECRET_REQUEST_VALUE_LIMIT).toBe(16 * 1024);
    expect(isSecretValueTooLarge('a'.repeat(SECRET_REQUEST_VALUE_LIMIT))).toBe(false);
    expect(isSecretValueTooLarge('a'.repeat(SECRET_REQUEST_VALUE_LIMIT + 1))).toBe(true);
    // «ё» — два байта: предел наступает на половине длины.
    expect(isSecretValueTooLarge('ё'.repeat(SECRET_REQUEST_VALUE_LIMIT / 2))).toBe(false);
    expect(isSecretValueTooLarge('ё'.repeat(SECRET_REQUEST_VALUE_LIMIT / 2 + 1))).toBe(true);
    expect(isSecretValueTooLarge('€'.repeat(Math.ceil((SECRET_REQUEST_VALUE_LIMIT + 1) / 3)))).toBe(true);
  });

  it('falls_back_to_a_short_line_without_an_expiry', () => {
    expect(secretRequestLine(view({ state: 'delivered' }))).toBe('Передан');
  });
});
