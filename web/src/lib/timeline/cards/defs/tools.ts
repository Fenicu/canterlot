// Навыки, Taiga, Context7, вопрос владельцу, субагент и служебные вызовы.
import { isObject, list, num, str, type JsonObject } from '../json';
import { clip, say } from '../copy';
import type { CardCall, CardLogic } from '../types';

/* ------------------------------------------------------------------ навык */

export const skill: CardLogic<{ name: string; plugin: string }> = {
  label: 'Навык',
  parse(call) {
    const raw = str(call.input.skill) || str(call.input.name) || call.argument;
    if (!raw) return null;
    const colon = raw.lastIndexOf(':');
    return { name: raw.slice(colon + 1).trim(), plugin: colon > 0 ? raw.slice(0, colon) : '' };
  },
  summary: (_call, data) => `Навык: ${data.name}`
};

/* ------------------------------------------------------------------ Taiga */

const ENTITIES: Record<string, [string, string, string, string, string]> = {
  // винительный, родительный, именительный, родительный множественного, дательный
  userstory: ['историю', 'истории', 'история', 'историй', 'истории'],
  story: ['историю', 'истории', 'история', 'историй', 'истории'],
  task: ['задачу', 'задачи', 'задача', 'задач', 'задаче'],
  issue: ['запрос', 'запроса', 'запрос', 'запросов', 'запросу'],
  epic: ['эпик', 'эпика', 'эпик', 'эпиков', 'эпику'],
  sprint: ['спринт', 'спринта', 'спринт', 'спринтов', 'спринту'],
  milestone: ['спринт', 'спринта', 'спринт', 'спринтов', 'спринту'],
  project: ['проект', 'проекта', 'проект', 'проектов', 'проекту'],
  wiki: ['страницу вики', 'страницы вики', 'страница вики', 'страниц вики', 'странице вики'],
  comment: ['комментарий', 'комментария', 'комментарий', 'комментариев', 'комментарию']
};

export interface TaigaData {
  entity: string;
  verb: string;
  ref: string;
  subject: string;
  target: string;
  url: string;
  query: string;
}

function refOf(...values: unknown[]): string {
  for (const value of values) {
    if (typeof value === 'number' && Number.isFinite(value)) return String(value);
    if (typeof value === 'string' && /^#?\d+$/.test(value.trim())) return value.trim().replace(/^#/, '');
  }
  return '';
}

/** Адрес из ответа Taiga: поле `permalink`/`url` или первая ссылка в тексте. */
export function taigaUrl(answer: JsonObject | null, text: string): string {
  const field = str(answer?.permalink) || str(answer?.url) || str(answer?.link);
  if (/^https?:\/\//.test(field)) return field;
  const found = text.match(/https?:\/\/[^\s"'<>)\]]+/g) ?? [];
  return found.find((url) => /\/(us|task|issue|epic|wiki)\//.test(url)) ?? found[0] ?? '';
}

export const taiga: CardLogic<TaigaData> = {
  label: 'Taiga',
  parse(call) {
    const op = call.id.slice('taiga.'.length);
    const match = /^(userstory|story|task|issue|epic|sprint|milestone|project|wiki|comment)s?_(\w+)$/.exec(op);
    const entity = match?.[1] ?? '';
    const verb = match?.[2] ?? op;
    const input = call.input;
    const answer = call.answer;
    const target = str(input.object_type) || str(input.type) || str(input.entity) || str(input.content_type);
    return {
      entity,
      verb,
      ref: refOf(input.ref, input.object_ref, answer?.ref, input.number),
      subject: str(input.subject) || str(answer?.subject) || str(input.title) || str(input.name),
      target: target.replace(/^userstories$/, 'userstory').replace(/s$/, ''),
      url: taigaUrl(answer, call.output),
      query: str(input.query) || str(input.text) || str(input.q)
    };
  },
  summary(call, data) {
    const forms = ENTITIES[data.entity];
    const ref = data.ref ? ` #${data.ref}` : '';
    const subject = data.subject ? ` «${clip(data.subject, 60)}»` : '';
    const she = call.voice === 'she';
    let text: string;
    if (data.entity === 'comment') {
      const target = ENTITIES[data.target]?.[4];
      const about = target ? `${target}${ref}` : ref.trim() || 'записи';
      text = data.verb === 'list' ? `комментарии к ${about}` : `комментарий к ${about}`;
    } else if (forms) {
      switch (data.verb) {
        case 'list':
          text = `список ${forms[3]}`;
          break;
        case 'get':
          text = she ? `открыла ${forms[0]}${ref}${subject}` : `${forms[2]}${ref}${subject}`;
          break;
        case 'create':
        case 'add':
          text = she ? `создала ${forms[0]}${subject}` : `новая ${forms[2]}${subject}`.replace(/^новая (эпик|спринт|проект|запрос|комментарий)/, 'новый $1');
          break;
        case 'update':
        case 'edit':
        case 'patch':
          text = she ? `обновила ${forms[0]}${ref}${subject}` : `обновление ${forms[1]}${ref}${subject}`;
          break;
        case 'delete':
          text = she ? `удалила ${forms[0]}${ref}` : `удаление ${forms[1]}${ref}`;
          break;
        case 'schema':
          text = `схема ${forms[1]}`;
          break;
        default:
          text = `${forms[2]}${ref}${subject}`;
      }
    } else if (data.verb === 'search') text = data.query ? `поиск: ${clip(data.query, 60)}` : 'поиск';
    else if (data.verb === 'stats') text = 'статистика';
    else if (data.verb === 'whoami') text = 'профиль';
    else text = data.verb.replace(/_/g, ' ');
    return `Taiga · ${text}`;
  }
};

/* ------------------------------------------------------------------ Context7 */

export interface DocsData {
  library: string;
  topic: string;
  resolving: boolean;
}

function libraryName(id: string): string {
  const parts = id.split('/').filter(Boolean);
  return parts.at(-1) ?? id;
}

export const context7: CardLogic<DocsData> = {
  label: 'Документация',
  parse(call) {
    const input = call.input;
    const resolving = /resolve/.test(call.id);
    const id = str(input.context7CompatibleLibraryID) || str(input.libraryId) || str(input.library_id);
    const library = resolving ? str(input.libraryName) || str(input.query) : libraryName(id) || str(input.libraryName);
    const topic = str(input.topic) || (resolving ? '' : str(input.query));
    if (!library && !topic) return null;
    return { library, topic, resolving };
  },
  summary(_call, data) {
    if (data.resolving) return `Документация: поиск библиотеки ${data.library}`;
    return `Документация: ${data.library}${data.topic ? ` — ${clip(data.topic, 60)}` : ''}`;
  }
};

/* ------------------------------------------------------------------ вопрос */

export interface QuestionData {
  questions: { question: string; answer: string }[];
}

/** Ответ CLI на вопрос: `"Вопрос"="Ответ", "Вопрос"="Ответ"`. */
export function questionAnswers(text: string): Map<string, string> {
  const answers = new Map<string, string>();
  for (const match of text.matchAll(/"([^"]+)"="([^"]*)"/g)) answers.set(match[1]!, match[2]!);
  return answers;
}

export const question: CardLogic<QuestionData> = {
  stays: true,
  yieldsToRequest: true,
  label: 'Вопрос',
  parse(call) {
    const answers = questionAnswers(call.output);
    const asked = list(call.input.questions)
      .filter(isObject)
      .map((item) => str(item.question))
      .filter(Boolean);
    const names = asked.length > 0 ? asked : [...answers.keys()];
    if (names.length === 0) return null;
    return { questions: names.map((text) => ({ question: text, answer: answers.get(text) ?? '' })) };
  },
  summary(call, data) {
    const first = data.questions[0]!;
    const more = data.questions.length > 1 ? ` и ещё ${data.questions.length - 1}` : '';
    const head = say(call.voice, 'Спросила', 'Вопрос');
    return `${head}: ${clip(first.question, 80)}${more}`;
  }
};

/* ------------------------------------------------------------------ субагент */

/** Строку субагента рисует его ветка: данных сверх вызова не нужно. */
export const subagent: CardLogic<Record<string, never>> = {
  label: 'Субагент',
  parse: () => ({}),
  summary: (call) => `Субагент${call.argument ? `: ${call.argument}` : ''}`
};

/* ------------------------------------------------------------------ служебные */

const QUIET_NAMES: Record<string, string> = {
  'meta.tool_search': 'Поиск инструментов',
  'meta.monitor': 'Наблюдение',
  'meta.task_stop': 'Остановка фоновой задачи',
  'meta.task_output': 'Вывод фоновой задачи',
  'meta.list_agents': 'Список агентов',
  'meta.send_message': 'Сообщение агенту',
  'meta.push_notification': 'Уведомление владельцу',
  'todo.read': 'Чтение списка задач'
};

export function quietName(id: string): string {
  return QUIET_NAMES[id] ?? 'Служебный вызов';
}

function quietDetail(call: CardCall): string {
  const input = call.input;
  switch (call.id) {
    case 'meta.tool_search':
      return str(input.query).replace(/^select:/, '');
    case 'meta.monitor':
      return str(input.description) || str(input.command);
    case 'meta.send_message':
      return str(input.to) || str(input.agent);
    case 'meta.push_notification':
      return str(input.message) || str(input.title);
    case 'meta.task_stop':
    case 'meta.task_output':
      return str(input.task_id) || str(input.shell_id) || str(input.bash_id);
    default:
      return num(input.limit) !== null ? '' : call.argument;
  }
}

export const quiet: CardLogic<{ name: string; detail: string }> = {
  quiet: true,
  parse(call) {
    return { name: quietName(call.id), detail: clip(quietDetail(call) || '', 80) };
  },
  summary: (_call, data) => (data.detail ? `${data.name}: ${data.detail}` : data.name)
};

