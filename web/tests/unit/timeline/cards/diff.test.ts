import { describe, expect, it } from 'vitest';
import { applyPatchFiles, contentLines, countChanges, lineDiff, splitLines, unifiedLines, unifiedSections } from '../../../../src/lib/timeline/cards/diff';

const kinds = (lines: { kind: string; text: string }[]) => lines.map((line) => `${line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : line.kind === 'hunk' ? '@' : ' '}${line.text}`);

describe('splitLines', () => {
  it('drops_only_the_final_newline', () => {
    expect(splitLines('a\nb\n')).toEqual(['a', 'b']);
    expect(splitLines('a\n\n')).toEqual(['a', '']);
    expect(splitLines('')).toEqual([]);
    expect(splitLines('a\r\nb')).toEqual(['a', 'b']);
  });
});

describe('lineDiff', () => {
  it('marks_a_changed_line_between_unchanged_context', () => {
    const diff = lineDiff('a\nb\nc\n', 'a\nB\nc\n');
    expect(kinds(diff)).toEqual([' a', '-b', '+B', ' c']);
    expect(countChanges(diff)).toEqual({ added: 1, removed: 1 });
  });

  it('counts_pure_insertions_and_deletions', () => {
    expect(countChanges(lineDiff('a\nc', 'a\nb1\nb2\nc'))).toEqual({ added: 2, removed: 0 });
    expect(countChanges(lineDiff('a\nb\nc', 'a'))).toEqual({ added: 0, removed: 2 });
    expect(countChanges(lineDiff('', 'x\ny'))).toEqual({ added: 2, removed: 0 });
    expect(countChanges(lineDiff('same', 'same'))).toEqual({ added: 0, removed: 0 });
  });

  it('keeps_two_lines_of_context_and_folds_the_rest_into_a_gap_row', () => {
    const before = Array.from({ length: 20 }, (_, index) => `line ${index + 1}`).join('\n');
    const after = before.replace('line 3', 'LINE 3').replace('line 17', 'LINE 17');
    const diff = lineDiff(before, after);
    expect(kinds(diff)).toEqual([
      ' line 1', ' line 2', '-line 3', '+LINE 3', ' line 4', ' line 5',
      '@9 строк без изменений',
      ' line 15', ' line 16', '-line 17', '+LINE 17', ' line 18', ' line 19',
      '@1 строка без изменений'
    ]);
  });

  it('interleaves_changes_by_longest_common_subsequence', () => {
    const diff = lineDiff('import a\nimport b\nconst x = 1\nexport x', 'import a\nimport c\nimport b\nconst x = 2\nexport x');
    expect(kinds(diff)).toEqual([' import a', '+import c', ' import b', '-const x = 1', '+const x = 2', ' export x']);
  });

  it('falls_back_to_remove_then_add_when_both_sides_are_huge', () => {
    const before = Array.from({ length: 3000 }, (_, index) => `a${index}`).join('\n');
    const after = Array.from({ length: 3000 }, (_, index) => `b${index}`).join('\n');
    expect(countChanges(lineDiff(before, after))).toEqual({ added: 3000, removed: 3000 });
  });
});

describe('unified diffs', () => {
  const patch = [
    'diff --git a/src/a.ts b/src/a.ts',
    'index 1111111..2222222 100644',
    '--- a/src/a.ts',
    '+++ b/src/a.ts',
    '@@ -1,3 +1,4 @@',
    ' import x',
    '-const a = 1;',
    '+const a = 2;',
    '+const b = 3;',
    ' export { a };',
    '\\ No newline at end of file'
  ].join('\n');

  it('reads_hunks_and_skips_file_headers', () => {
    const lines = unifiedLines(patch);
    expect(kinds(lines)).toEqual(['@@@ -1,3 +1,4 @@', ' import x', '-const a = 1;', '+const a = 2;', '+const b = 3;', ' export { a };']);
    expect(countChanges(lines)).toEqual({ added: 2, removed: 1 });
  });

  it('splits_concatenated_file_diffs_by_their_headers', () => {
    const second = ['--- /dev/null', '+++ b/docs/new.md', '@@ -0,0 +1,2 @@', '+# Title', '+text'].join('\n');
    const sections = unifiedSections(`${patch}\n${second}\n`);
    expect(sections.map((section) => [section.path, section.created, countChanges(section.lines)])).toEqual([
      ['src/a.ts', false, { added: 2, removed: 1 }],
      ['docs/new.md', true, { added: 2, removed: 0 }]
    ]);
  });

  it('a_headerless_codex_diff_is_one_section_without_a_path', () => {
    const sections = unifiedSections('@@ -1 +1 @@\n-a\n+b\n');
    expect(sections).toHaveLength(1);
    expect(sections[0]!.path).toBe('');
    expect(countChanges(sections[0]!.lines)).toEqual({ added: 1, removed: 1 });
  });

  it('plain_text_has_no_sections', () => {
    expect(unifiedSections('# Новый файл\n\nТекст.\n')).toEqual([]);
  });
});

describe('apply_patch envelopes', () => {
  it('reads_add_update_delete_and_move_sections', () => {
    const text = [
      '*** Begin Patch',
      '*** Add File: docs/new.md',
      '+# Title',
      '+body',
      '*** Update File: src/a.ts',
      '*** Move to: src/b.ts',
      '@@ function a()',
      ' keep',
      '-old',
      '+new',
      '*** Delete File: src/gone.ts',
      '*** End Patch'
    ].join('\n');
    const files = applyPatchFiles(text)!;
    expect(files.map((file) => [file.path, file.kind, file.added, file.removed])).toEqual([
      ['docs/new.md', 'add', 2, 0],
      ['src/b.ts', 'edit', 1, 1],
      ['src/gone.ts', 'delete', 0, 0]
    ]);
    expect(kinds(files[1]!.lines)).toEqual(['@function a()', ' keep', '-old', '+new']);
    expect(files[0]!.content).toBe('# Title\nbody');
  });

  it('is_null_for_other_text', () => {
    expect(applyPatchFiles('@@ -1 +1 @@\n-a\n+b')).toBeNull();
  });
});

describe('contentLines', () => {
  it('counts_every_line_of_new_content', () => {
    expect(countChanges(contentLines('a\nb\nc\n'))).toEqual({ added: 3, removed: 0 });
    expect(countChanges(contentLines(''))).toEqual({ added: 0, removed: 0 });
  });
});
