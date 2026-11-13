// Навыки, Taiga, Context7, вопрос владельцу, субагент и служебные вызовы.
import { bool, isObject, list, num, obj, str, strings, type Json, type JsonObject } from '../json';
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

const ENTITIES: Record<string, [string, string, string, string, string, string]> = {
  // винительный, родительный, именительный, родительный множественного, дательный, творительный
  userstory: ['историю', 'истории', 'история', 'историй', 'истории', 'историей'],
  task: ['задачу', 'задачи', 'задача', 'задач', 'задаче', 'задачей'],
  issue: ['запрос', 'запроса', 'запрос', 'запросов', 'запросу', 'запросом'],
  epic: ['эпик', 'эпика', 'эпик', 'эпиков', 'эпику', 'эпиком'],
  sprint: ['спринт', 'спринта', 'спринт', 'спринтов', 'спринту', 'спринтом'],
  project: ['проект', 'проекта', 'проект', 'проектов', 'проекту', 'проектом'],
  wiki: ['страницу вики', 'страницы вики', 'страница вики', 'страниц вики', 'странице вики', 'страницей вики'],
  comment: ['комментарий', 'комментария', 'комментарий', 'комментариев', 'комментарию', 'комментарием']
};

/** Винительный, родительный и родительный множественного для счёта созданных записей. */
const CREATED_FORMS: Record<string, [string, string, string]> = {
  userstory: ['историю', 'истории', 'историй'],
  task: ['задачу', 'задачи', 'задач'],
  issue: ['запрос', 'запроса', 'запросов'],
  epic: ['эпик', 'эпика', 'эпиков'],
  sprint: ['спринт', 'спринта', 'спринтов'],
  wiki: ['страницу вики', 'страницы вики', 'страниц вики'],
  project: ['проект', 'проекта', 'проектов']
};

const LIST_HEAD: Record<string, string> = {
  userstory: 'истории',
  task: 'задачи',
  issue: 'запросы',
  epic: 'эпики',
  sprint: 'спринты',
  wiki: 'страницы вики',
  project: 'проекты'
};

export type TaigaKind = 'get' | 'create' | 'update' | 'delete' | 'list' | 'search' | 'bulk' | 'comment_add' | 'comment_list' | 'link' | 'other';

export interface TaigaRow {
  ref: string;
  subject: string;
  status: string;
  assigned: string;
  kind: string;
  url: string;
}

export interface TaigaComment {
  author: string;
  text: string;
}

export interface TaigaData {
  kind: TaigaKind;
  entity: string;
  verb: string;
  ref: string;
  subject: string;
  status: string;
  assigned: string;
  sprint: string;
  points: string;
  blocked: boolean;
  blockedWord: string;
  target: string;
  url: string;
  query: string;
  rows: TaigaRow[];
  created: TaigaRow[];
  failed: number;
  total: number | null;
  changes: string[];
  comment: string;
  lastComment: TaigaComment | null;
  totalComments: number;
  hint: string;
  error: string;
  from: TaigaRow | null;
  to: TaigaRow | null;
  remove: boolean;
}

function refOf(...values: unknown[]): string {
  for (const value of values) {
    if (typeof value === 'number' && Number.isFinite(value)) return String(value);
    if (typeof value === 'string' && /^#?\d+$/.test(value.trim())) return value.trim().replace(/^#/, '');
  }
  return '';
}

function textOf(...values: unknown[]): string {
  for (const value of values) if (typeof value === 'string' && value.trim()) return value.trim();
  return '';
}

/** `story` и `milestone` — другие имена тех же записей Taiga. */
export function taigaEntity(value: string): string {
  const word = value.trim().toLowerCase();
  if (word === 'userstory' || word === 'story' || word === 'userstories') return 'userstory';
  if (word === 'milestone' || word === 'milestones') return 'sprint';
  if (word === 'task' || word === 'issue' || word === 'epic' || word === 'sprint' || word === 'project' || word === 'wiki' || word === 'comment') return word;
  return '';
}

/** Адрес из ответа Taiga: поле `permalink`/`url` или первая ссылка в тексте. */
export function taigaUrl(answer: JsonObject | null, text: string): string {
  const field = str(answer?.permalink) || str(answer?.url) || str(answer?.link);
  if (/^https?:\/\//.test(field)) return field;
  const found = text.match(/https?:\/\/[^\s"'<>)\]]+/g) ?? [];
  return found.find((url) => /\/(us|task|issue|epic|wiki)\//.test(url)) ?? found[0] ?? '';
}

function pointsWord(value: number): string {
  const mod10 = Math.abs(value) % 10;
  const mod100 = Math.abs(value) % 100;
  if (mod10 === 1 && mod100 !== 11) return 'очко';
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return 'очка';
  return 'очков';
}

function pointsText(value: Json | undefined): string {
  if (typeof value === 'number' && Number.isFinite(value)) return `${value} ${pointsWord(value)}`;
  if (isObject(value)) {
    return Object.entries(value)
      .map(([role, point]) => {
        const text = typeof point === 'number' ? String(point) : str(point);
        return text ? `${role} ${text}` : '';
      })
      .filter(Boolean)
      .join(', ');
  }
  return typeof value === 'string' ? value.trim() : '';
}

function statusOf(raw: JsonObject, entity: string): string {
  const status = str(raw.status);
  if (status) return status;
  if (entity === 'sprint') {
    const closed = bool(raw.closed);
    if (closed !== null) return closed ? 'закрыт' : 'открыт';
  }
  return '';
}

function assignedOf(raw: JsonObject): string {
  return textOf(raw.assigned_to, strings(raw.assigned_users).join(', '));
}

function blockedWord(entity: string): string {
  return entity === 'issue' || entity === 'epic' ? 'заблокирован' : 'заблокирована';
}

function rowOf(entity: string, raw: JsonObject): TaigaRow {
  return {
    ref: refOf(raw.ref),
    subject: textOf(raw.subject, raw.name, raw.slug, raw.title),
    status: statusOf(raw, entity),
    assigned: assignedOf(raw),
    kind: entity,
    url: taigaUrl(raw, '')
  };
}

function mainRow(entity: string, input: JsonObject, answer: JsonObject | null, url: string): TaigaRow {
  return {
    ref: refOf(input.ref, input.object_ref, answer?.ref, input.number),
    subject: textOf(answer?.subject, input.subject, answer?.name, input.name, answer?.slug, input.slug),
    status: textOf(answer?.status, typeof input.status === 'string' ? input.status : '') || (answer ? statusOf(answer, entity) : ''),
    assigned: assignedOf(answer ?? {}) || assignedOf(input),
    kind: entity,
    url
  };
}

function pluralWord(entity: string, count: number): string {
  const forms = CREATED_FORMS[entity];
  if (!forms) return 'записей';
  const mod10 = count % 10;
  const mod100 = count % 100;
  if (mod10 === 1 && mod100 !== 11) return forms[0];
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 10 || mod100 >= 20)) return forms[1];
  return forms[2];
}

function errorLine(output: string): string {
  const line = output.split('\n').map((part) => part.trim()).find(Boolean) ?? '';
  return line.replace(/^(?:error|taiga_error|mcp_error):\s*/i, '').trim();
}

/** Правки из входа вызова: владелец читает с телефона, что именно изменилось. */
function changesOf(input: JsonObject, answer: JsonObject | null): string[] {
  const changes: string[] = [];
  const has = (key: string) => Object.hasOwn(input, key) && input[key] !== undefined && input[key] !== null;
  const answerName = (key: string) => textOf(answer?.[key]);
  const named = (key: string, fallback = 'изменено') => answerName(key) || textOf(input[key]) || fallback;

  if (has('status')) changes.push(`статус → ${named('status')}`);
  if (has('assigned_to')) {
    const value = input.assigned_to;
    changes.push(value === '' ? 'сняла исполнителя' : `исполнитель → ${named('assigned_to')}`);
  }
  if (has('assigned_users')) {
    const names = strings(input.assigned_users);
    changes.push(names.length > 0 ? `исполнители → ${names.join(', ')}` : 'сняла исполнителей');
  }
  if (has('sprint')) {
    const value = textOf(input.sprint);
    changes.push(value ? `спринт → ${value}` : 'убрала из спринта');
  }
  if (has('subject')) changes.push(`тема → «${textOf(input.subject)}»`);
  if (has('description')) changes.push('описание изменено');
  if (has('append_description')) changes.push('описание дополнено');
  if (has('tags')) {
    const tags = strings(input.tags);
    changes.push(tags.length > 0 ? `теги → ${tags.join(', ')}` : 'сняла теги');
  }
  if (has('add_tags')) {
    const tags = strings(input.add_tags);
    if (tags.length > 0) changes.push(`добавила теги: ${tags.join(', ')}`);
  }
  if (has('points')) {
    const points = pointsText(input.points);
    if (points) changes.push(`очки → ${points}`);
  }
  if (has('estimate')) {
    const estimate = num(input.estimate);
    if (estimate !== null) changes.push(`оценка → ${estimate}`);
  }
  if (has('role')) {
    const role = textOf(input.role);
    if (role) changes.push(`роль → ${role}`);
  }
  if (has('priority')) changes.push(`приоритет → ${named('priority')}`);
  if (has('severity')) changes.push(`важность → ${named('severity')}`);
  if (has('type')) changes.push(`тип → ${named('type')}`);
  if (has('due_date')) {
    const date = textOf(input.due_date);
    changes.push(date ? `срок → ${date}` : 'сняла срок');
  }
  if (has('is_blocked')) changes.push(bool(input.is_blocked) === true ? 'заблокировала' : 'разблокировала');
  if (has('blocked_note')) {
    changes.push(textOf(input.blocked_note) ? 'заметка о блокировке' : 'сняла заметку о блокировке');
  }
  if (has('user_story')) {
    const ref = refOf(input.user_story);
    if (ref) changes.push(`история → #${ref}`);
  }
  if (has('epic')) {
    const epic = textOf(input.epic);
    changes.push(epic ? `эпик → «${epic}»` : 'убрала из эпика');
  }
  if (has('client_requirement')) changes.push(`требование клиента: ${bool(input.client_requirement) ? 'да' : 'нет'}`);
  if (has('team_requirement')) changes.push(`требование команды: ${bool(input.team_requirement) ? 'да' : 'нет'}`);
  if (has('color')) changes.push(`цвет → ${textOf(input.color)}`);
  if (has('name')) changes.push(`название → «${textOf(input.name)}»`);
  if (has('closed')) changes.push(bool(input.closed) === true ? 'закрыла спринт' : 'открыла спринт');
  if (has('estimated_start')) changes.push(`начало → ${textOf(input.estimated_start)}`);
  if (has('estimated_finish')) changes.push(`конец → ${textOf(input.estimated_finish)}`);
  if (has('content')) changes.push('содержимое изменено');
  if (has('backlog_order')) {
    const order = num(input.backlog_order);
    if (order !== null) changes.push(`порядок в бэклоге → ${order}`);
  }
  return changes;
}

function listModifiers(call: CardCall, data: TaigaData): string[] {
  const input = call.input;
  const mods: string[] = [];
  if (data.entity === 'userstory') {
    const sprint = textOf(input.sprint);
    if (sprint) mods.push(`спринта «${clip(sprint, 40)}»`);
    if (bool(input.in_backlog) === true) mods.push('бэклога');
  }
  if (data.entity === 'task') {
    const sprint = textOf(input.sprint);
    if (sprint) mods.push(`спринта «${clip(sprint, 40)}»`);
    const story = refOf(input.user_story);
    if (story) mods.push(`истории #${story}`);
  }
  const status = textOf(input.status);
  if (status) mods.push(`со статусом «${clip(status, 40)}»`);
  const assigned = textOf(input.assigned_to);
  if (assigned) mods.push(`исполнителя «${clip(assigned, 40)}»`);
  const epic = textOf(input.epic);
  if (epic) mods.push(`эпика «${clip(epic, 40)}»`);
  const tags = strings(input.tags);
  if (tags.length > 0) mods.push(`с тегами ${tags.join(', ')}`);
  const priority = textOf(input.priority);
  if (priority) mods.push(`с приоритетом «${clip(priority, 40)}»`);
  const severity = textOf(input.severity);
  if (severity) mods.push(`с важностью «${clip(severity, 40)}»`);
  const type = textOf(input.type);
  if (type) mods.push(`с типом «${clip(type, 40)}»`);
  return mods;
}

function listSummary(call: CardCall, data: TaigaData): string {
  const head = LIST_HEAD[data.entity] ?? 'записи';
  if (data.entity === 'sprint') {
    const closed = bool(call.input.closed);
    if (closed === true) return `закрытые спринты${data.total !== null ? `: ${data.total}` : ''}`;
    if (closed === false) return `открытые спринты${data.total !== null ? `: ${data.total}` : ''}`;
  }
  const mods = listModifiers(call, data);
  const text = mods.length > 0 ? `${head} ${mods.join(' ')}` : head;
  return data.total !== null ? `${text}: ${data.total}` : `список ${ENTITIES[data.entity]?.[3] ?? 'записей'}`;
}

function linkSummary(data: TaigaData, she: boolean): string {
  if (!data.from?.ref || !data.to?.ref) return 'связь записей';
  const fromForms = ENTITIES[data.from.kind];
  const toForms = ENTITIES[data.to.kind];
  const fromRef = `#${data.from.ref}`;
  const toRef = `#${data.to.ref}`;
  if (data.remove) {
    const from = fromForms?.[1] ?? 'записи';
    const to = toForms?.[5] ?? 'записью';
    return she ? `сняла связь ${from} ${fromRef} с ${to} ${toRef}` : `снятие связи ${from} ${fromRef} с ${to} ${toRef}`;
  }
  const from = fromForms?.[0] ?? 'запись';
  const to = toForms?.[5] ?? 'записью';
  return she ? `связала ${from} ${fromRef} с ${to} ${toRef}` : `связь ${fromForms?.[1] ?? 'записи'} ${fromRef} с ${to} ${toRef}`;
}

function linkRow(raw: JsonObject, ref: unknown, kind: string): TaigaRow | null {
  const value = refOf(raw.ref, ref);
  if (!value) return null;
  return {
    ref: value,
    subject: str(raw.subject),
    status: '',
    assigned: '',
    kind: taigaEntity(str(raw.kind)) || kind,
    url: taigaUrl(raw, '')
  };
}

export const taiga: CardLogic<TaigaData> = {
  stays: true,
  label: 'Taiga',
  parse(call) {
    const op = call.id.slice('taiga.'.length);
    const match = /^(userstory|story|task|issue|epic|sprint|milestone|project|wiki)s?_(\w+)$/.exec(op);
    const input = call.input;
    const answer = call.answer;
    const query = str(input.query) || str(input.text) || str(input.q);
    const target = taigaEntity(str(input.resource) || str(input.object_type) || str(input.type) || str(input.entity) || str(input.content_type));
    let entity = taigaEntity(match?.[1] ?? '');
    let verb = match?.[2] ?? op;
    let kind: TaigaKind = 'other';
    if (op === 'search') {
      verb = 'search';
      kind = 'search';
    } else if (op === 'link') {
      verb = 'link';
      kind = 'link';
    } else if (op === 'comment_add' || op === 'comment_list') {
      entity = 'comment';
      verb = op === 'comment_add' ? 'add' : 'list';
      kind = op as TaigaKind;
    } else if (op === 'bulk_create') {
      entity = taigaEntity(str(input.resource)) || 'task';
      verb = 'create';
      kind = 'bulk';
    } else if (verb === 'get') kind = 'get';
    else if (verb === 'list') kind = 'list';
    else if (verb === 'create' || verb === 'add') kind = 'create';
    else if (verb === 'update' || verb === 'edit' || verb === 'patch') kind = 'update';
    else if (verb === 'delete') kind = 'delete';

    const url = taigaUrl(answer, call.output);
    const data: TaigaData = {
      kind,
      entity,
      verb,
      ref: refOf(input.ref, input.object_ref, answer?.ref, input.number),
      subject: textOf(answer?.subject, input.subject, answer?.name, input.name, answer?.slug, input.slug),
      status: textOf(answer?.status, typeof input.status === 'string' ? input.status : '') || (answer ? statusOf(answer, entity) : ''),
      assigned: assignedOf(answer ?? {}) || assignedOf(input),
      sprint: textOf(answer?.sprint, input.sprint),
      points: pointsText(answer?.points ?? input.points),
      blocked: bool(answer?.is_blocked) === true,
      blockedWord: blockedWord(entity),
      target,
      url,
      query,
      rows: [],
      created: [],
      failed: 0,
      total: null,
      changes: [],
      comment: '',
      lastComment: null,
      totalComments: 0,
      hint: str(answer?.hint),
      error: call.state === 'error' ? errorLine(call.output) : '',
      from: null,
      to: null,
      remove: input.remove === true
    };

    if (kind === 'get' && answer && (data.ref || data.subject || data.status)) data.rows = [mainRow(entity, input, answer, url)];
    if (kind === 'create') {
      const created = mainRow(entity, input, answer, url);
      if (created.ref || created.subject) data.rows = [created];
    }
    if (kind === 'update') data.changes = changesOf(input, answer);
    if (kind === 'list') {
      const items = list(answer?.items).filter(isObject);
      data.rows = items.map((item) => rowOf(entity, item));
      data.total = answer ? (num(answer.total) ?? data.rows.length) : null;
    }
    if (kind === 'search') {
      const buckets: [string, string][] = [
        ['userstories', 'userstory'],
        ['tasks', 'task'],
        ['issues', 'issue'],
        ['epics', 'epic'],
        ['wikipages', 'wiki']
      ];
      data.rows = buckets.flatMap(([key, bucket]) => list(answer?.[key]).filter(isObject).map((item) => rowOf(bucket, item)));
      data.total = answer ? (num(answer.count) ?? data.rows.length) : null;
    }
    if (kind === 'bulk') {
      data.created = list(answer?.created).filter(isObject).map((item) => rowOf(entity, item));
      data.failed = list(answer?.failed).length;
      data.total = answer ? data.created.length : null;
    }
    if (kind === 'comment_add') {
      data.comment = str(input.comment) || str(input.text);
      data.totalComments = data.comment ? 1 : 0;
    }
    if (kind === 'comment_list') {
      const items = list(answer?.items).filter(isObject);
      const comments = items
        .map((item) => ({ author: str(item.author), text: str(item.comment) }))
        .filter((item) => item.text !== '');
      data.totalComments = answer ? (num(answer.total) ?? comments.length) : 0;
      data.lastComment = comments.at(-1) ?? null;
    }
    if (kind === 'link') {
      data.from = linkRow(obj(answer?.from), input.from, '');
      data.to = linkRow(obj(answer?.to), input.to, '');
    }
    if (entity === 'comment' && kind === 'comment_add') data.totalComments = 1;
    if (kind === 'get') data.totalComments = num(answer?.total_comments) ?? 0;
    return data;
  },
  summary(call, data) {
    const forms = ENTITIES[data.entity];
    const ref = data.ref ? ` #${data.ref}` : '';
    const subject = data.subject ? ` «${clip(data.subject, 60)}»` : '';
    const she = call.voice === 'she';
    let text: string;
    if (data.kind === 'list') {
      text = listSummary(call, data);
    } else if (data.kind === 'search') {
      text = data.query ? `поиск «${clip(data.query, 60)}»${data.total !== null ? `: ${data.total}` : ''}` : 'поиск';
    } else if (data.kind === 'bulk') {
      const count = data.created.length;
      text = she ? `создала ${count} ${pluralWord(data.entity, count)}` : `создание: ${count} ${pluralWord(data.entity, count)}`;
    } else if (data.kind === 'comment_add' || data.kind === 'comment_list') {
      const about = data.target ? `${ENTITIES[data.target]?.[4] ?? 'записи'}${ref}` : ref.trim() || 'записи';
      text = data.kind === 'comment_list'
        ? `комментарии к ${about}${data.totalComments > 0 ? `: ${data.totalComments}` : ''}`
        : `комментарий к ${about}`;
    } else if (data.kind === 'link') {
      text = linkSummary(data, she);
    } else if (data.kind === 'get') {
      text = she ? `открыла ${forms?.[0] ?? 'запись'}${ref}${subject}` : `${forms?.[2] ?? 'запись'}${ref}${subject}`;
    } else if (data.kind === 'create') {
      text = she
        ? `создала ${forms?.[0] ?? 'запись'}${ref}${subject}`
        : `новая ${forms?.[2] ?? 'запись'}${ref}${subject}`.replace(/^новая (эпик|спринт|проект|запрос|комментарий)/, 'новый $1');
    } else if (data.kind === 'update') {
      text = she ? `обновила ${forms?.[0] ?? 'запись'}${ref}${subject}` : `обновление ${forms?.[1] ?? 'записи'}${ref}${subject}`;
    } else if (data.kind === 'delete') {
      text = she ? `удалила ${forms?.[0] ?? 'запись'}${ref}` : `удаление ${forms?.[1] ?? 'записи'}${ref}`;
    } else if (forms) {
      if (data.verb === 'schema') text = `схема ${forms[1]}`;
      else text = `${forms[2]}${ref}${subject}`;
    } else if (data.verb === 'stats') text = 'статистика';
    else if (data.verb === 'whoami') text = 'профиль';
    else text = data.verb.replace(/_/g, ' ') || 'вызов';
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
  'todo.read': 'Чтение списка задач',
  'taiga.whoami': 'Профиль Taiga',
  'taiga.project_schema': 'Схема проекта',
  'taiga.project_list': 'Проекты Taiga',
  'taiga.project_get': 'Проект Taiga',
  'taiga.stats': 'Статистика Taiga'
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
    case 'taiga.whoami':
    case 'taiga.project_schema':
    case 'taiga.project_list':
      return '';
    case 'taiga.project_get':
      return str(input.project) || str(input.slug);
    case 'taiga.stats':
      return str(input.sprint) || 'проект';
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

