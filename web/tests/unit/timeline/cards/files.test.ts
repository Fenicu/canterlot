import { describe, expect, it } from 'vitest';
import { resolveCard } from '../../../../src/lib/timeline/cards/registry';
import { cardCall } from '../../../../src/lib/timeline/cards/call';
import { fileDir, fileName, type EditData } from '../../../../src/lib/timeline/cards/defs/files';
import { callBlock, made } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');
const PATH = '/home/owner/canterlot/web/src/lib/timeline/cards/registry.ts';

function edit(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: EditData }) | null;
}

describe('paths', () => {
  it('basename_and_a_short_directory', () => {
    expect(fileName(PATH)).toBe('registry.ts');
    expect(fileDir(PATH)).toBe('…/timeline/cards');
    expect(fileDir('web/src/a.ts')).toBe('web/src');
    expect(fileDir('a.ts')).toBe('');
  });
});

describe('edit card', () => {
  it('claude_edit_counts_lines_from_old_and_new_strings', () => {
    const card = edit(made('Edit', { file_path: PATH, old_string: 'a\nb\nc', new_string: 'a\nB\nB2\nc' }, 'The file has been updated.'))!;
    expect(card.def.label).toBe('Правка');
    expect(card.data.kind).toBe('edit');
    expect([card.data.added, card.data.removed]).toEqual([2, 1]);
    expect(card.line).toBe('Правка registry.ts · +2 −1');
    expect(card.word).toBeNull();
  });

  it('claude_multiedit_labels_each_edit', () => {
    const card = edit(made('MultiEdit', { file_path: PATH, edits: [{ old_string: 'x', new_string: 'y' }, { old_string: 'p', new_string: 'p\nq' }] }, 'ok'))!;
    expect([card.data.added, card.data.removed]).toEqual([2, 1]);
    expect(card.data.files[0]!.lines.filter((line) => line.kind === 'hunk').map((line) => line.text)).toEqual(['правка 1 из 2', 'правка 2 из 2']);
  });

  it('opencode_edit_uses_camel_case_fields', () => {
    const card = edit(made('edit', { filePath: '/w/src/app.py', oldString: 'x = 1', newString: 'x = 2' }, 'Edit applied successfully.'))!;
    expect(card.line).toBe('Правка app.py · +1 −1');
  });

  it('claude_write_tells_a_new_file_from_an_overwrite', () => {
    const created = edit(made('Write', { file_path: '/w/docs/new.md', content: '# T\n\ntext\n' }, 'File created successfully at: /w/docs/new.md'))!;
    expect(created.data.kind).toBe('add');
    expect(created.line).toBe('Новый файл new.md · +3 −0');
    const overwritten = edit(made('Write', { file_path: '/w/a.ts', content: 'x' }, 'The file /w/a.ts has been updated successfully.'))!;
    expect(overwritten.data.kind).toBe('overwrite');
    const opencode = edit(made('write', { filePath: '/w/b.ts', content: 'x\ny' }, 'Wrote file successfully.'))!;
    expect(opencode.data.kind).toBe('write');
    expect(opencode.line).toBe('Запись b.ts · +2 −0');
    const fresh = edit(made('write', { filePath: '/w/new.txt', content: 'x\n' }, 'Created file successfully: /w/new.txt'))!;
    expect(fresh.data.kind).toBe('add');
    expect(fresh.line).toBe('Новый файл new.txt · +1 −0');
  });

  it('codex_file_change_takes_paths_from_the_summary', () => {
    const update = cardCall(callBlock('apply_patch', '@@ -1,2 +1,2 @@\n keep\n-old\n+new\n', { summary: 'web/src/a.ts, completed' }), null, { root: true });
    expect(edit(update)!.line).toBe('Правка a.ts · +1 −1');
    const added = cardCall(callBlock('apply_patch', 'hello\nworld\n', { summary: 'docs/hello.md, completed' }), null, { root: true });
    expect(edit(added)!.data.kind).toBe('add');
    expect(edit(added)!.line).toBe('Новый файл hello.md · +2 −0');
    const failed = cardCall(callBlock('apply_patch', '@@ -1 +1 @@\n-a\n+b\n', { summary: 'a.ts, failed' }), null, { root: true });
    expect(edit(failed)!.word).toEqual({ word: 'ошибка', tone: 'danger' });
    const declined = cardCall(callBlock('apply_patch', '@@ -1 +1 @@\n-a\n+b\n', { summary: 'a.ts, declined' }), null, { root: true });
    expect(edit(declined)!.word).toEqual({ word: 'отклонено', tone: 'warning' });
  });

  it('codex_file_change_over_several_files_keeps_one_shared_diff', () => {
    const call = cardCall(callBlock('apply_patch', '@@ -1 +1 @@\n-a\n+b\n@@ -3 +3,2 @@\n c\n+d\n', { summary: 'a.ts, b.ts, completed' }), null, { root: true });
    const card = edit(call)!;
    expect(card.data.files.map((file) => file.path)).toEqual(['a.ts', 'b.ts']);
    expect(card.data.shared).toHaveLength(6);
    expect(card.line).toBe('Правка a.ts и ещё 1 · +2 −1');
  });

  it('a_running_codex_change_without_text_names_its_files_without_counts', () => {
    const call = cardCall(callBlock('apply_patch', '', { state: 'streaming', summary: 'a.ts' }), null, { root: true, working: true });
    const card = edit(call)!;
    expect(card.data.counted).toBe(false);
    expect(card.line).toBe('Правка a.ts');
    expect(card.word).toMatchObject({ word: 'идёт' });
  });

  it('apply_patch_envelope_reads_every_file', () => {
    const text = ['*** Begin Patch', '*** Add File: docs/a.md', '+one', '*** Update File: src/b.ts', '@@', '-x', '+y', '*** End Patch'].join('\n');
    const card = edit(made('apply_patch', { input: text }, 'Done!'))!;
    expect(card.data.files.map((file) => [file.path, file.kind])).toEqual([['docs/a.md', 'add'], ['src/b.ts', 'edit']]);
    expect(card.line).toBe('Правка a.md и ещё 1 · +2 −1');
  });

  it('malformed_input_falls_back_to_the_generic_block', () => {
    expect(resolveCard(made('Edit', { file_path: PATH }, 'ok'), NOW)).toBeNull();
    expect(resolveCard(made('Write', { file_path: PATH }, 'ok'), NOW)).toBeNull();
    expect(resolveCard(made('NotebookEdit', { notebook_path: '/w/a.ipynb', new_source: 'x' }, 'ok'), NOW)).toBeNull();
    expect(resolveCard(made('apply_patch', '', null), NOW)).toBeNull();
  });
});
