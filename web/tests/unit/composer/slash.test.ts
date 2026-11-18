import { describe, expect, it } from 'vitest';
import { filterSlashCatalog, insertSlash, slashQuery } from '../../../src/lib/composer/slash.js';

const item = (name: string, kind = 'command'): {
  name: string;
  description: string;
  kind: string;
  argument_hint: string;
} => ({ name, description: `about ${name}`, kind, argument_hint: '' });

describe('slashQuery', () => {
  it('opens only while the first character is a slash and no space was typed', () => {
    expect(slashQuery('/')).toBe('');
    expect(slashQuery('/rev')).toBe('rev');
    expect(slashQuery('/REV')).toBe('REV');
    expect(slashQuery('rev')).toBeNull();
    expect(slashQuery(' /rev')).toBeNull();
    expect(slashQuery('/rev iew')).toBeNull();
    expect(slashQuery('/rev\n')).toBeNull();
    expect(slashQuery('')).toBeNull();
  });
});

describe('filterSlashCatalog', () => {
  it('puts name-prefix matches before substring matches, case-insensitively', () => {
    const items = [
      item('explain'),
      item('preview'),
      item('review'),
      item('review-copy'),
      item('re-review')
    ];
    const found = filterSlashCatalog(items, 'rev');
    expect(found.map((entry) => entry.name)).toEqual([
      'review',
      'review-copy',
      'preview',
      're-review'
    ]);
  });

  it('keeps every item for an empty query and drops duplicate names', () => {
    const items = [item('init'), item('init', 'skill'), item('review')];
    expect(filterSlashCatalog(items, '').map((entry) => entry.name)).toEqual(['init', 'review']);
  });

  it('returns nothing when no name matches', () => {
    expect(filterSlashCatalog([item('review')], 'zzz')).toEqual([]);
  });

  it('caps the list so the popover stays small', () => {
    const items = Array.from({ length: 30 }, (_, index) => item(`cmd-${index}`));
    expect(filterSlashCatalog(items, '')).toHaveLength(12);
  });
});

describe('insertSlash', () => {
  it('inserts the bare name with a leading slash and a trailing space', () => {
    expect(insertSlash('review')).toBe('/review ');
  });
});
