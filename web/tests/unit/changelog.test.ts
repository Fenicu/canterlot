import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import {
  compareVersions,
  entriesSince,
  parseChangelog,
  type ChangelogEntry
} from '../../src/lib/changelog';
import { APP_VERSION } from '../../src/lib/stores/appVersion';

const SAMPLE = `
История изменений
=================

Вводный абзац, который в записи попадать не должен.

2.2.0 — 10.09.2026
------------------

Добавлено
~~~~~~~~~

- Первый пункт.
- Пункт, который не поместился в одну строку и
  продолжается с отступом.

Исправлено
~~~~~~~~~~

- Что-то починили.

2.1.0 — 03.09.2026
------------------

Изменено
~~~~~~~~

- Что-то поменяли.

2.0.0 — 31.08.2026
------------------

Начало истории изменений.
`;

function versions(entries: ChangelogEntry[]): string[] {
  return entries.map((entry) => entry.version);
}

function cargoVersion(): string {
  const text = readFileSync('../Cargo.toml', 'utf8');
  const section = text.match(/\[workspace\.package\]([\s\S]*?)(?=\n\[|$)/);
  const version = section?.[1]?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) throw new Error('в Cargo.toml нет [workspace.package] version');
  return version;
}

describe('разбор истории изменений', () => {
  it('читает версии, даты и разделы', () => {
    const entries = parseChangelog(SAMPLE);

    expect(versions(entries)).toEqual(['2.2.0', '2.1.0', '2.0.0']);
    expect(entries[0]!.date).toBe('10.09.2026');
    expect(entries[0]!.sections.map((section) => section.title)).toEqual(['Добавлено', 'Исправлено']);
  });

  it('склеивает пункт, перенесённый на следующую строку', () => {
    const [latest] = parseChangelog(SAMPLE);

    expect(latest!.sections[0]!.items).toEqual([
      'Первый пункт.',
      'Пункт, который не поместился в одну строку и продолжается с отступом.'
    ]);
  });

  it('запись без разделов сохраняет свой текст', () => {
    const first = parseChangelog(SAMPLE).find((entry) => entry.version === '2.0.0');

    expect(first?.preamble).toEqual(['Начало истории изменений.']);
    expect(first?.sections).toEqual([]);
  });

  it('не тащит вводную часть файла в первую запись', () => {
    for (const entry of parseChangelog(SAMPLE)) {
      expect(entry.preamble.join(' ')).not.toContain('Вводный абзац');
    }
  });

  it('даты в настоящем CHANGES.rst записаны как ДД.ММ.ГГГГ', () => {
    for (const entry of parseChangelog()) {
      expect(entry.date).toMatch(/^\d{2}\.\d{2}\.\d{4}$/);
    }
  });

  it('настоящий CHANGES.rst разбирается и начинается со свежей версии', () => {
    const entries = parseChangelog();

    expect(entries.length).toBeGreaterThan(0);
    expect(entries[0]!.version).toBe(APP_VERSION);
    expect(entries[0]!.version).toMatch(/^\d+\.\d+\.\d+$/);
  });
});

describe('сравнение версий', () => {
  it('считает числа, а не строки', () => {
    expect(compareVersions('2.10.0', '2.9.0')).toBeGreaterThan(0);
    expect(compareVersions('2.1.0', '2.1.0')).toBe(0);
    expect(compareVersions('2.1', '2.1.0')).toBe(0);
  });
});

describe('что показать после обновления', () => {
  const entries = parseChangelog(SAMPLE);

  it('отдаёт всё, что вышло между виденной версией и текущей', () => {
    expect(versions(entriesSince('2.0.0', '2.2.0', entries))).toEqual(['2.2.0', '2.1.0']);
  });

  it('не показывает будущее: версии новее установленной', () => {
    expect(versions(entriesSince('2.0.0', '2.1.0', entries))).toEqual(['2.1.0']);
  });

  it('ничего не показывает, когда человек уже на свежей версии', () => {
    expect(entriesSince('2.2.0', '2.2.0', entries)).toEqual([]);
  });

  it('ничего не показывает тому, кто здесь впервые', () => {
    expect(entriesSince(null, '2.2.0', entries)).toEqual([]);
  });

  it('переживает версию из будущего в хранилище', () => {
    expect(entriesSince('3.0.0', '2.2.0', entries)).toEqual([]);
  });
});

describe('версия и история не разъезжаются', () => {
  it('в CHANGES.rst есть запись под установленную версию', () => {
    expect(parseChangelog().map((entry) => entry.version)).toContain(APP_VERSION);
  });

  it('версия интерфейса совпадает с Cargo.toml и package.json', () => {
    const pkg = JSON.parse(readFileSync('package.json', 'utf8')) as { version: string };

    expect(APP_VERSION).toBe(cargoVersion());
    expect(pkg.version).toBe(cargoVersion());
  });
});
