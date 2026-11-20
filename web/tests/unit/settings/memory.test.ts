import { describe, expect, it } from 'vitest';
import { ApiError } from '#lib/api/http.js';
import {
  RESTORE_NOTE,
  revisionSnapshot,
  actorLabel,
  changedLine,
  collectProjects,
  kindOptions,
  projectOptions,
  DOMAINS,
  conflictVersion,
  dateToValidUntil,
  entryStatus,
  kindLabel,
  kindWord,
  KINDS,
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

  it('keeps_one_kind_dictionary_for_the_screen_and_the_cards', () => {
    const kinds = ['user', 'feedback', 'project', 'reference', 'decision', 'gotcha', 'environment', 'incident'];
    expect(kinds.map(kindWord)).toEqual(['о владельце', 'урок', 'о проекте', 'справка', 'решение', 'грабли', 'окружение', 'инцидент']);
    // Экран говорит теми же словами с заглавной буквы.
    expect(kinds.map(kindLabel)).toEqual(['О владельце', 'Урок', 'О проекте', 'Справка', 'Решение', 'Грабли', 'Окружение', 'Инцидент']);
    expect(KINDS.map((entry) => entry.label)).toEqual(kinds.map(kindLabel));
    expect(kindWord('weird')).toBe('weird');
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

describe('who changed an entry', () => {
  it('names the actor in Russian', () => {
    expect(actorLabel({ kind: 'owner' })).toBe('владельцем');
    expect(actorLabel({ kind: 'agent', harness: 'claude_code' })).toBe('агентом (claude_code)');
    expect(actorLabel({ kind: 'agent' })).toBe('агентом');
    expect(actorLabel({ kind: 'import', bundle_id: 'b1' })).toBe('импортом');
    expect(actorLabel(null)).toBe('неизвестно кем');
  });

  it('puts the actor and a time in one compact line', () => {
    const line = changedLine({ updated_by: { kind: 'owner' }, updated_at: '2026-11-01T09:05:00Z' });
    expect(line.startsWith('Изменено владельцем, ')).toBe(true);
    expect(line.length).toBeLessThan(60);
  });
});

describe('revision snapshot', () => {
  it('carries every editable field of a project-level revision', () => {
    expect(revisionSnapshot({ kind: 'gotcha', subject: 'Тема', body: 'Текст', project: '/h/work/alpha', valid_until: '2027-01-01T23:59:59Z' })).toEqual({
      kind: 'gotcha', subject: 'Тема', body: 'Текст', level: 'project', project: '/h/work/alpha', valid_until: '2027-01-01T23:59:59Z'
    });
  });

  it('turns a domain-level revision without expiry into level domain and no expiry', () => {
    expect(revisionSnapshot({ kind: 'user', subject: 'Т', body: 'Б', project: null })).toEqual({
      kind: 'user', subject: 'Т', body: 'Б', level: 'domain', project: undefined, valid_until: null
    });
  });

  it('names what comes back', () => {
    expect(RESTORE_NOTE(3)).toContain('тема, вид, уровень, срок и текст версии 3');
  });
});

describe('project filter options', () => {
  it('keeps every project seen, once, labelled by its last segment', () => {
    const seen = new Map<string, string>();
    collectProjects(seen, [{ project: '/h/work/alpha', project_label: 'alpha' }, { project: null, project_label: null }]);
    collectProjects(seen, [{ project: '/h/work/beta', project_label: 'beta' }, { project: '/h/work/alpha', project_label: 'alpha' }]);
    expect(projectOptions(seen)).toEqual([
      { value: '', label: 'Все проекты' },
      { value: '/h/work/alpha', label: 'alpha' },
      { value: '/h/work/beta', label: 'beta' }
    ]);
  });

  it('offers every kind plus "all kinds"', () => {
    const options = kindOptions();
    expect(options[0]).toEqual({ value: '', label: 'Все виды' });
    expect(options).toHaveLength(9);
  });
});

describe('settings navigation', () => {
  it('has a memory section', () => {
    expect(SETTINGS_CATEGORIES.find((entry) => entry.id === 'memory')?.title).toBe('Память');
  });
});
