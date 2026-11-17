// MCP-сервер meeting-pipeline: поиск по встречам, саммари, задачи, расшифровка.
import { fmt } from '#lib/format/ru.js';
import { isObject, jsonItems, jsonSequence, list, num, str, type JsonObject } from '../json';
import { clip, hubError } from '../copy';
import type { CardCall, CardLogic } from '../types';

export interface MeetingPreview {
  /** Тайминг, дата или спикер — короткая подпись слева. */
  lead: string;
  text: string;
}

export interface MeetingData {
  op: string;
  /** Короткий id прогона: первая часть uuid. */
  run: string;
  query: string;
  title: string;
  count: number | null;
  /** Строк превью не больше трёх: расшифровка целиком в свёрнутой карточке не нужна. */
  preview: MeetingPreview[];
  /** Длина текстового ответа в знаках: саммари, задачи, расшифровка. */
  size: number;
  note: string;
  error: string;
}

const PREVIEW = 3;
const FORMS = {
  fragments: ['фрагмент', 'фрагмента', 'фрагментов'],
  meetings: ['встреча', 'встречи', 'встреч'],
  tasks: ['задача', 'задачи', 'задач'],
  speakers: ['спикер', 'спикера', 'спикеров'],
  spaces: ['пространство', 'пространства', 'пространств'],
  chars: ['знак', 'знака', 'знаков']
} as const;

function counted(count: number, forms: readonly [string, string, string]): string {
  return `${fmt.number(count)} ${fmt.plural(count, forms as [string, string, string])}`;
}

/** `01a1e8fc-7528-…` → `01a1e8fc`. */
export function shortRun(id: string): string {
  const clean = id.trim();
  return /^[0-9a-f]{8}-/i.test(clean) ? clean.slice(0, 8) : clean.length > 16 ? `${clean.slice(0, 12)}…` : clean;
}

/** Секунды записи как `1:02:03` или `12:34`. */
export function timecode(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return '';
  const whole = Math.floor(seconds);
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = String(whole % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`;
}

/** Первая строка markdown без разметки заголовка и списка. */
function firstLine(text: string): string {
  const line = text.split('\n').map((item) => item.trim()).find((item) => item && !/^[-=*_]{3,}$/.test(item)) ?? '';
  return line.replace(/^#{1,6}\s+/, '').replace(/^[-*+]\s+(?:\[[ xX]\]\s+)?/, '').replace(/\*\*/g, '');
}

const TASK_LINE = /^\s*(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?(.+)$/;

/** Задачи из markdown: пункты списка верхнего уровня. */
export function taskLines(text: string): string[] {
  return text
    .split('\n')
    .filter((line) => !/^\s{2,}/.test(line))
    .map((line) => TASK_LINE.exec(line)?.[1]?.replace(/\*\*/g, '').trim() ?? '')
    .filter(Boolean);
}

function date(value: unknown): string {
  const text = typeof value === 'string' ? value : '';
  const match = /^(\d{4})-(\d{2})-(\d{2})/.exec(text);
  return match ? `${match[3]}.${match[2]}.${match[1]}` : '';
}

function analysisCounts(answer: JsonObject): string[] {
  const parts: [string, readonly [string, string, string]][] = [
    ['topics', ['тема', 'темы', 'тем']],
    ['decisions', ['решение', 'решения', 'решений']],
    ['action_items', ['задача', 'задачи', 'задач']],
    ['tasks', ['задача', 'задачи', 'задач']],
    ['risks', ['риск', 'риска', 'рисков']],
    ['questions', ['вопрос', 'вопроса', 'вопросов']],
    ['open_questions', ['вопрос', 'вопроса', 'вопросов']]
  ];
  return parts.flatMap(([key, forms]) => (Array.isArray(answer[key]) ? [counted((answer[key] as unknown[]).length, forms)] : []));
}

function itemText(item: JsonObject): string {
  return str(item.text) || str(item.snippet) || str(item.title) || str(item.name) || str(item.summary) || str(item.description);
}

export const meeting: CardLogic<MeetingData> = {
  label: 'Встречи',
  parse(call: CardCall) {
    const op = call.id.slice('meeting.'.length);
    const input = call.input;
    const output = call.output;
    const data: MeetingData = {
      op,
      run: shortRun(str(input.run_id)),
      query: str(input.query).trim(),
      title: '',
      count: null,
      preview: [],
      size: 0,
      note: '',
      error: call.state === 'error' ? hubError(output) : ''
    };
    if (!output || data.error) return data;
    switch (op) {
      case 'search_within_run': {
        const items = jsonItems(output);
        data.count = items.length;
        data.preview = items.slice(0, PREVIEW).map((item) => ({
          lead: [timecode(num(item.start_seconds)), (Array.isArray(item.speaker_labels) ? item.speaker_labels : list(item.speakers)).filter((name) => typeof name === 'string').join(', ')].filter(Boolean).join(' · '),
          text: clip(itemText(item), 160)
        }));
        break;
      }
      case 'search_meetings':
      case 'list_runs': {
        const items = jsonItems(output, ['items']);
        data.count = items.length;
        data.preview = items.slice(0, PREVIEW).map((item) => ({
          lead: date(item.meeting_date) || date(item.created_at),
          text: clip([str(item.title) || str(item.source_filename), op === 'search_meetings' ? str(item.snippet).replace(/<\/?[a-z]+>/gi, '') : ''].filter(Boolean).join(' — '), 160)
        }));
        const whole = jsonSequence(output)[0];
        if (isObject(whole) && str(whole.note)) data.note = 'поиск без смысловой части';
        break;
      }
      case 'get_run': {
        const answer = jsonItems(output)[0] ?? {};
        data.title = str(answer.title) || str(answer.source_filename);
        const lead = [date(answer.meeting_date) || date(answer.created_at), str(answer.stage)].filter(Boolean).join(' · ');
        if (lead) data.preview = [{ lead, text: list(answer.tags).filter((tag) => typeof tag === 'string').join(', ') }];
        break;
      }
      case 'get_tasks': {
        const tasks = taskLines(output);
        data.count = tasks.length;
        data.preview = tasks.slice(0, PREVIEW).map((text) => ({ lead: '', text: clip(text, 160) }));
        data.size = output.length;
        break;
      }
      case 'get_summary':
      case 'get_transcript': {
        data.size = output.length;
        // Расшифровка в свёрнутой карточке не нужна: только её размер. У саммари — первая строка.
        if (op === 'get_summary') {
          const head = firstLine(output);
          if (head) data.preview = [{ lead: '', text: clip(head, 160) }];
        }
        break;
      }
      case 'get_analysis': {
        const answer = jsonItems(output)[0];
        if (answer) {
          const parts = analysisCounts(answer);
          if (parts.length > 0) data.preview = [{ lead: '', text: parts.join(', ') }];
        }
        break;
      }
      case 'list_speakers':
      case 'list_workspaces': {
        const items = jsonItems(output);
        data.count = items.length;
        const names = items.map((item) => str(item.name) || str(item.display_name) || str(item.label)).filter(Boolean);
        if (names.length > 0) data.preview = [{ lead: '', text: clip(names.join(', '), 160) }];
        break;
      }
      default:
        break;
    }
    return data;
  },
  summary(_call, data) {
    const run = data.run ? ` ${data.run}` : '';
    const query = data.query ? ` «${clip(data.query, 60)}»` : '';
    const count = (forms: readonly [string, string, string]) => (data.count !== null ? ` · ${counted(data.count, forms)}` : '');
    switch (data.op) {
      case 'search_within_run':
        return `Встреча${run}: поиск${query}${count(FORMS.fragments)}`;
      case 'search_meetings':
        return `Встречи: поиск${query}${count(FORMS.meetings)}`;
      case 'list_runs':
        return `Встречи${data.count !== null ? `: ${data.count}` : ''}`;
      case 'get_run':
        return data.title ? `Встреча «${clip(data.title, 70)}»` : `Встреча${run}`;
      case 'get_summary':
        return `Саммари встречи${run}`;
      case 'get_tasks':
        return `Задачи встречи${run}${count(FORMS.tasks)}`;
      case 'get_transcript':
        return `Расшифровка встречи${run}${data.size > 0 ? ` · ${counted(data.size, FORMS.chars)}` : ''}`;
      case 'get_analysis':
        return `Анализ встречи${run}`;
      case 'list_speakers':
        return `Спикеры${count(FORMS.speakers)}`;
      case 'list_workspaces':
        return `Пространства встреч${count(FORMS.spaces)}`;
      default:
        return `Встречи · ${data.op.replace(/_/g, ' ')}${run}`;
    }
  }
};
