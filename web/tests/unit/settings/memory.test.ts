import { describe, expect, it } from 'vitest';
import { ApiError } from '#lib/api/http.js';
import {
  DOMAINS,
  conflictVersion,
  dateToValidUntil,
  entryStatus,
  kindLabel,
  levelLabel,
  secretClass,
  sessionMemoryLabel,
  validUntilToDate
} from '#lib/screens/settings/memory.js';
import { SETTINGS_CATEGORIES } from '#lib/screens/settings/settingsNav.js';

function failure(code: string, details?: Record<string, unknown>): ApiError {
  return new ApiError(409, code as never, 'отказ', 'browser', 'never', 'c-1', details as never);
}

describe('memory domains and copy', () => {
  it('offers exactly personal and work, in Russian', () => {
    expect(DOMAINS).toEqual([
      { value: 'personal', label: 'Личное' },
      { value: 'work', label: 'Работа' }
    ]);
  });

  it('names every kind in words', () => {
    for (const kind of ['user', 'feedback', 'project', 'reference', 'decision', 'gotcha', 'environment', 'incident']) {
      expect(kindLabel(kind)).toMatch(/^[А-Яа-яЁё ]+$/);
    }
    expect(kindLabel('unknown')).toBe('unknown');
  });

  it('describes the level of an entry', () => {
    expect(levelLabel({ level: 'domain', project_label: null })).toBe('Весь домен');
    expect(levelLabel({ level: 'project', project_label: 'alpha' })).toBe('Проект alpha');
  });
});

describe('entry status', () => {
  it('says archived, expired and active in words with a tone', () => {
    expect(entryStatus({ status: 'archived', expired: false })).toEqual({ word: 'В архиве', tone: 'neutral' });
    expect(entryStatus({ status: 'active', expired: true })).toEqual({ word: 'Срок истёк', tone: 'warn' });
    expect(entryStatus({ status: 'active', expired: false })).toEqual({ word: 'Активна', tone: 'ok' });
  });
});

describe('session memory row', () => {
  it('shows the domain, no memory, or a domain conflict', () => {
    expect(sessionMemoryLabel({ memory_domain: 'work', memory_conflict: false })).toBe('Работа');
    expect(sessionMemoryLabel({ memory_domain: 'personal', memory_conflict: false })).toBe('Личное');
    expect(sessionMemoryLabel({ memory_domain: null, memory_conflict: false })).toBe('нет памяти');
    expect(sessionMemoryLabel({ memory_domain: null, memory_conflict: true })).toBe('конфликт домена');
    expect(sessionMemoryLabel({})).toBe('нет памяти');
  });
});

describe('refusal helpers', () => {
  it('reads the current version out of a version conflict only', () => {
    expect(conflictVersion(failure('version_conflict', { current_version: 4 }))).toBe(4);
    expect(conflictVersion(failure('subject_exists', { existing_version: 4 }))).toBeNull();
    expect(conflictVersion(new Error('x'))).toBeNull();
  });

  it('reads the secret class without any value', () => {
    expect(secretClass(failure('looks_like_secret', { class: 'token_prefix' }))).toBe('token_prefix');
    expect(secretClass(failure('version_conflict', { current_version: 1 }))).toBeNull();
  });
});

describe('expiry date field', () => {
  it('round-trips a date through the hub timestamp', () => {
    const stamp = dateToValidUntil('2027-03-05');
    expect(stamp).toBe('2027-03-05T23:59:59Z');
    expect(validUntilToDate(stamp)).toBe('2027-03-05');
    expect(dateToValidUntil('')).toBeNull();
    expect(validUntilToDate(null)).toBe('');
  });
});

describe('settings navigation', () => {
  it('has a memory section', () => {
    expect(SETTINGS_CATEGORIES.find((entry) => entry.id === 'memory')?.title).toBe('Память');
  });
});
