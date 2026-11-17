import { describe, expect, it } from 'vitest';
import { resolveCard, cardFold, cardLabel, cardQuiet } from '../../../../src/lib/timeline/cards/registry';
import { exploreLine, foundCount, type FindData, type ReadData } from '../../../../src/lib/timeline/cards/defs/explore';
import { made } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');

function card<D>(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: D }) | null;
}

describe('read card', () => {
  it('claude_read_names_the_file_and_its_line_window', () => {
    const resolved = card<ReadData>(made('Read', { file_path: '/w/web/src/lib/timeline/steps.ts', offset: 120, limit: 61 }, '120\tconst x = 1;'))!;
    expect(resolved.line).toBe('Чтение steps.ts · стр. 120–180');
    expect(resolved.def.quiet).toBe(true);
    expect(cardFold({ kind: 'tool_call', toolName: 'Read' })).toBe('explore');
    expect(cardLabel({ kind: 'tool_call', toolName: 'Read' })).toBe('Чтение');
  });

  it('opencode_offset_is_zero_based', () => {
    expect(card<ReadData>(made('read', { filePath: '/w/a.py', offset: 0, limit: 50 }, 'x'))!.data.range).toBe('стр. 1–50');
    expect(card<ReadData>(made('read', { filePath: '/w/a.py', offset: 99 }, 'x'))!.data.range).toBe('с стр. 100');
    expect(card<ReadData>(made('Read', { file_path: '/w/a.py' }, 'x'))!.data.range).toBe('');
  });

  it('codex_view_image_keeps_the_media_block', () => {
    expect(resolveCard(made('view_image', { path: '/w/shot.png' }, ''), NOW)).toBeNull();
  });
});

describe('search card', () => {
  it('claude_grep_counts_files_and_keeps_pattern_and_scope', () => {
    const resolved = card<FindData>(made('Grep', { pattern: 'cardDef\\(', path: 'web/src', glob: '*.ts' }, 'Found 4 files\nweb/src/a.ts\nweb/src/b.ts'))!;
    expect(resolved.data).toMatchObject({ kind: 'grep', pattern: 'cardDef\\(', where: 'web/src · *.ts', count: 4, unit: 'files' });
    expect(resolved.line).toBe('Поиск cardDef\\( в web/src · *.ts · 4 файла');
  });

  it('opencode_grep_counts_matches_and_glob_counts_paths', () => {
    expect(card<FindData>(made('grep', { pattern: 'TODO', include: '*.rs' }, 'Found 12 matches\n/w/a.rs:\n  Line 3: TODO'))!.data).toMatchObject({ count: 12, unit: 'matches', where: '*.rs' });
    expect(card<FindData>(made('glob', { pattern: '**/*.svelte' }, '/w/a.svelte\n/w/b.svelte\n/w/c.svelte'))!.data).toMatchObject({ count: 3, unit: 'files' });
    expect(card<FindData>(made('Glob', { pattern: '*.nope' }, 'No files found'))!.line).toBe('Поиск *.nope · ничего');
  });

  it('found_count_is_unknown_for_plain_grep_content', () => {
    expect(foundCount('src/a.ts:3:const x', 'grep')).toEqual({ count: null, unit: 'matches' });
    expect(foundCount('', 'glob')).toEqual({ count: null, unit: 'matches' });
  });

  it('opencode_list_names_the_directory', () => {
    expect(card<FindData>(made('list', { path: '/w/web/src' }, 'a\nb'))!.line).toBe('Список /w/web/src · 2 файла');
  });

  it('a_search_without_pattern_falls_back', () => {
    expect(resolveCard(made('Grep', {}, 'x'), NOW)).toBeNull();
    expect(cardQuiet({ kind: 'tool_call', toolName: 'Grep' })).toBe(true);
  });
});

describe('explore line', () => {
  it('uses_russian_plurals_and_voice', () => {
    expect(exploreLine(6, 2, { running: true, voice: 'she' })).toBe('Изучает код: 6 файлов, 2 поиска');
    expect(exploreLine(1, 0, { running: false, voice: 'she' })).toBe('Изучила код: 1 файл');
    expect(exploreLine(3, 5, { running: false, voice: 'neutral' })).toBe('Изучение кода: 3 файла, 5 поисков');
    expect(exploreLine(0, 1, { running: false, voice: 'neutral' })).toBe('Изучение кода: 1 поиск');
  });
});

describe('explore counts', () => {
  it('counts_distinct_files_and_every_search', async () => {
    const { exploreCounts } = await import('../../../../src/lib/timeline/cards/defs/explore');
    const read = (path: string) => ({ toolName: 'Read', text: JSON.stringify({ file_path: path }) });
    expect(exploreCounts([read('/a'), read('/a'), read('/b'), { toolName: 'Grep', text: '{}' }, { toolName: 'glob', text: '{}' }])).toEqual({ files: 2, searches: 2 });
    expect(exploreCounts([{ toolName: 'read', text: '' }])).toEqual({ files: 1, searches: 0 });
  });
});
