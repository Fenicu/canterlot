// Файл для владельца: вложение блока вызова, его заголовок и заметка.
import type { BlockFile } from '#lib/stores/blocks.js';
import { num, str } from '../json';
import { hubError, quoted, say } from '../copy';
import type { CardLogic } from '../types';

export interface ShareData {
  files: BlockFile[];
  title: string;
  note: string;
  /** Путь из вызова: имя файла, пока вложение не пришло. */
  path: string;
  error: string;
}

function baseName(path: string): string {
  const slash = path.lastIndexOf('/');
  return slash >= 0 ? path.slice(slash + 1) : path;
}

export const share: CardLogic<ShareData> = {
  stays: true,
  label: 'Файл',
  parse(call) {
    const files = call.block.files;
    const answer = call.answer;
    // Вложение у блока приходит от хаба; без него — то, что ответил воркер.
    const fallback: BlockFile[] =
      files.length === 0 && answer && str(answer.attachment_id)
        ? [{ attachment_id: str(answer.attachment_id), name: str(answer.name), mime: str(answer.mime), size: num(answer.size) ?? 0, title: str(answer.title), note: str(answer.note) }]
        : [];
    const path = str(call.input.path);
    if (files.length === 0 && fallback.length === 0 && !path && call.state !== 'error' && !call.argument) return null;
    const shown = files.length > 0 ? files : fallback;
    return {
      files: shown,
      title: str(call.input.title) || shown[0]?.title || '',
      note: str(call.input.note) || shown[0]?.note || '',
      path,
      error: call.state === 'error' ? hubError(call.output) : ''
    };
  },
  summary(call, data) {
    const name = data.title || data.files[0]?.name || baseName(data.path) || call.argument;
    if (data.error) return say(call.voice, `Не передала файл ${quoted(name)}`, `Файл ${quoted(name)} не передан`);
    return say(call.voice, `Передала файл ${quoted(name)}`, `Файл для владельца ${quoted(name)}`);
  }
};
