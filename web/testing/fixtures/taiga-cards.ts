// Лента хода агента с карточками Taiga: e2e, экран visual-language и видео на ревью.
// Данные синтетические: проект «Склад», люди и записи выдуманы.
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';
import { Feed, hubText } from './feed-cards';

const MINUTE = 60_000;
const MODEL = 'claude-opus-5-5';
const US = 'https://taiga.example.com/project/sklad/us/123';
const TASK = 'https://taiga.example.com/project/sklad/task/201';
const WIKI = 'https://taiga.example.com/project/sklad/wiki/home';

export interface TaigaCardsSeed {
  id: string;
  run: string;
  /** id блоков по имени сцены. */
  blocks: Record<string, string>;
}

const STORY = {
  ref: 123,
  subject: 'Фильтр товаров по складу',
  status: 'В работе',
  assigned_to: 'Анна Смирнова',
  assigned_users: ['Анна Смирнова', 'Илья Котов'],
  sprint: 'Спринт 8',
  points: 5,
  tags: ['склад', 'витрина'],
  is_blocked: true,
  is_closed: false,
  total_comments: 3,
  permalink: US
};

const STORY_ROWS = [
  ['124', 'Экспорт остатков в CSV', 'Новая', 'Илья Котов'],
  ['125', 'Уведомление о низком остатке', 'Готово к тестированию', 'Анна Смирнова'],
  ['126', 'Поиск по артикулу', 'Готово', 'Илья Котов'],
  ['127', 'Массовое списание', 'Новая', 'Анна Смирнова'],
  ['128', 'История движения товара', 'В работе', 'Илья Котов'],
  ['129', 'Печать этикеток', 'Новая', 'Анна Смирнова'],
  ['130', 'Импорт из Excel', 'Готово', 'Илья Котов'],
  ['131', 'Роли кладовщиков', 'Новая', 'Анна Смирнова'],
  ['132', 'Отчёт по остаткам', 'Готово к тестированию', 'Илья Котов'],
  ['133', 'Сканер штрихкодов', 'В работе', 'Анна Смирнова'],
  ['134', 'Архив перемещений', 'Готово', 'Илья Котов']
].map(([ref, subject, status, assigned_to]) => ({ ref: Number(ref), subject, status, assigned_to, sprint: 'Спринт 8', points: 3 }));

const LONG_COMMENT = [
  'Проверила фильтр на телефоне: при выборе склада список обновляется без перезагрузки.',
  '',
  'Осталось три вещи:',
  '- пустой склад показывает подсказку;',
  '- счётчик выбранных товаров не прыгает;',
  '- сброс фильтра возвращает весь список.',
  '',
  'Проверка на 320 px прошла, горизонтальной прокрутки нет.'
].join('\n');

export function seedTaigaCards(api: FakeHubApi, opts: { now?: number } = {}): TaigaCardsSeed {
  const now = opts.now ?? Date.now();
  const id = seedConversation(api, {
    title: 'Склад',
    model: MODEL,
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' }
  });
  // Ход ещё идёт: последний вызов без ответа показывает живое «идёт».
  api.details.get(id)!.activity = { state: 'working', evidence: { phase: 'running_tool' }, since: new Date(now).toISOString() };
  const feed = new Feed(api, now - 30 * MINUTE);
  const run = api.id();

  feed.input('Проверь спринт «Спринт 8» и приведи задачи в порядок');
  feed.text(run, 'Сначала найду историю по складу и посмотрю, что в ней уже есть.');

  feed.call(run, 'search', 'mcp__plugin_taiga_taiga__taiga_search', { project: 'sklad', text: 'склад' }, hubText({
    count: 7,
    userstories: [
      { ref: 123, subject: 'Фильтр товаров по складу', status: 'В работе' },
      { ref: 124, subject: 'Экспорт остатков в CSV', status: 'Новая' },
      { ref: 125, subject: 'Уведомление о низком остатке', status: 'Готово к тестированию' }
    ],
    tasks: [
      { ref: 201, subject: 'Настроить выгрузку остатков', status: 'Новая' },
      { ref: 202, subject: 'Сверить колонки CSV', status: 'Новая' },
      { ref: 203, subject: 'Добавить прогресс выгрузки', status: 'В работе' }
    ],
    issues: [],
    epics: [],
    wikipages: [{ id: 9, slug: 'home', modified_date: '2026-11-01' }]
  }));

  feed.call(run, 'get', 'mcp__plugin_taiga_taiga__taiga_userstory_get', { project: 'sklad', ref: 123 }, hubText(STORY));

  feed.text(run, 'Проверяю доступ и схему проекта.');
  feed.call(run, 'whoami', 'mcp__plugin_taiga_taiga__taiga_whoami', {}, hubText({ id: 11, username: 'anna', full_name: 'Анна Смирнова', email: 'anna@example.com', default_project: 'sklad', projects: [{ id: 1, slug: 'sklad', name: 'Склад' }] }));
  feed.call(run, 'schema', 'mcp__plugin_taiga_taiga__taiga_project_schema', { project: 'sklad' }, hubText({ statuses: [{ name: 'Новая' }, { name: 'В работе' }, { name: 'Готово к тестированию' }, { name: 'Готово' }], members: [{ full_name: 'Анна Смирнова' }, { full_name: 'Илья Котов' }] }));
  feed.call(run, 'stats', 'mcp__plugin_taiga_taiga__taiga_stats', { project: 'sklad', sprint: 'Спринт 8' }, hubText({ scope: 'sprint', name: 'Спринт 8', total_points: 34, completed_points: 12 }));

  feed.text(run, 'Читаю комментарии и отвечаю по фильтру.');
  feed.call(run, 'comments', 'mcp__plugin_taiga_taiga__taiga_comment_list', { project: 'sklad', resource: 'userstory', ref: 123 }, hubText({
    total: 3,
    items: [
      { author: 'Илья Котов', created_at: '2026-11-05T09:10:00Z', comment: 'Проверил на большом складе: фильтр отвечает быстро.' },
      { author: 'Анна Смирнова', created_at: '2026-11-06T12:30:00Z', comment: 'Добавила сценарий с пустым складом.' },
      { author: 'Илья Котов', created_at: '2026-11-07T08:15:00Z', comment: 'Осталось проверить сброс фильтра на телефоне.' }
    ]
  }));
  feed.call(run, 'comment', 'mcp__plugin_taiga_taiga__taiga_comment_add', { project: 'sklad', resource: 'userstory', ref: 123, comment: LONG_COMMENT }, hubText({ added: true, resource: 'userstory', id: 9123 }));

  feed.text(run, 'Меняю статус, исполнителя и снимаю блокировку.');
  feed.call(run, 'update', 'mcp__plugin_taiga_taiga__taiga_userstory_update', {
    project: 'sklad',
    ref: 123,
    subject: 'Фильтр товаров по складу',
    status: 'Готово к тестированию',
    assigned_to: 'Илья Котов',
    add_tags: ['срочно'],
    is_blocked: false
  }, hubText({ ...STORY, status: 'Готово к тестированию', assigned_to: 'Илья Котов', tags: ['склад', 'витрина', 'срочно'], is_blocked: false }));

  feed.text(run, 'Создаю задачи на выгрузку: одну вручную и пачку по плану.');
  feed.call(run, 'create', 'mcp__plugin_taiga_taiga__taiga_task_create', { project: 'sklad', subject: 'Проверить выгрузку остатков', user_story: 123, status: 'Новая', assigned_to: 'Анна Смирнова' }, hubText({ ref: 207, subject: 'Проверить выгрузку остатков', status: 'Новая', assigned_to: 'Анна Смирнова', user_story: 123, tags: [], is_closed: false, permalink: TASK }));
  feed.call(run, 'bulk', 'mcp__plugin_taiga_taiga__taiga_bulk_create', {
    project: 'sklad',
    resource: 'task',
    items: [
      { subject: 'Настроить выгрузку остатков', user_story: 123 },
      { subject: 'Сверить колонки CSV', user_story: 123 },
      { subject: 'Добавить прогресс выгрузки', user_story: 123 },
      { subject: 'Проверить пустой склад', user_story: 123 },
      { subject: 'Обновить подсказку в интерфейсе', user_story: 123 },
      { subject: 'Написать тесты выгрузки', user_story: 123 }
    ]
  }, hubText({
    created: [
      { ref: 201, subject: 'Настроить выгрузку остатков', status: 'Новая' },
      { ref: 202, subject: 'Сверить колонки CSV', status: 'Новая' },
      { ref: 203, subject: 'Добавить прогресс выгрузки', status: 'Новая' },
      { ref: 204, subject: 'Проверить пустой склад', status: 'Новая' },
      { ref: 205, subject: 'Обновить подсказку в интерфейсе', status: 'Новая' },
      { ref: 206, subject: 'Написать тесты выгрузки', status: 'Новая' }
    ],
    failed: []
  }));

  feed.call(run, 'link', 'mcp__plugin_taiga_taiga__taiga_link', { project: 'sklad', from: 201, to: 123, type: 'relates' }, hubText({
    from: { ref: 201, kind: 'task', subject: 'Настроить выгрузку остатков' },
    to: { ref: 123, kind: 'userstory', subject: 'Фильтр товаров по складу' },
    changed: ['from:Связано с', 'to:Связано с']
  }));

  feed.text(run, 'Сверяю список историй спринта и страницу вики.');
  feed.call(run, 'list', 'mcp__plugin_taiga_taiga__taiga_userstory_list', { project: 'sklad', sprint: 'Спринт 8' }, hubText({
    total: 12,
    page: 1,
    has_more: false,
    items: [
      { ref: 123, subject: 'Фильтр товаров по складу', status: 'Готово к тестированию', assigned_to: 'Илья Котов' },
      ...STORY_ROWS
    ]
  }));
  feed.call(run, 'sprints', 'mcp__plugin_taiga_taiga__taiga_sprint_list', { project: 'sklad' }, hubText({
    total: 3,
    items: [
      { id: 1, name: 'Спринт 7', slug: 'sprint-7', closed: true },
      { id: 2, name: 'Спринт 8', slug: 'sprint-8', closed: false },
      { id: 3, name: 'Спринт 9', slug: 'sprint-9', closed: false }
    ]
  }));
  feed.call(run, 'wikiGet', 'mcp__plugin_taiga_taiga__taiga_wiki_get', { project: 'sklad', slug: 'home' }, hubText({ id: 9, slug: 'home', modified_date: '2026-11-01', last_modifier: 'Анна Смирнова', permalink: WIKI }));
  feed.call(run, 'wikiCreate', 'mcp__plugin_taiga_taiga__taiga_wiki_create', { project: 'sklad', slug: 'release-notes', content: 'Что вошло в релиз склада.' }, hubText({ id: 10, slug: 'release-notes', permalink: 'https://taiga.example.com/project/sklad/wiki/release-notes' }));
  feed.call(run, 'wikiUpdate', 'mcp__plugin_taiga_taiga__taiga_wiki_update', { project: 'sklad', id: 10, content: 'Что вошло в релиз склада: фильтр, выгрузка, этикетки.' }, hubText({ id: 10, slug: 'release-notes', permalink: 'https://taiga.example.com/project/sklad/wiki/release-notes' }));

  feed.text(run, 'Удаляю устаревшую задачу и смотрю, что ещё идёт.');
  feed.call(run, 'delete', 'mcp__plugin_taiga_taiga__taiga_task_delete', { project: 'sklad', ref: 198, confirm: true }, hubText({ deleted: true, resource: 'task', id: 1980 }));
  feed.call(run, 'running', 'mcp__plugin_taiga_taiga__taiga_userstory_get', { project: 'sklad', ref: 124 }, '', {}, { state: 'streaming' });

  feed.text(run, 'Проверяю спринт, которого ещё нет.');
  feed.call(run, 'error', 'mcp__plugin_taiga_taiga__taiga_userstory_update', { project: 'sklad', ref: 123, sprint: 'Спринт 9' }, '"Спринт 9" is not a sprint in this project.', {}, { summary: 'error' });
  feed.text(run, 'Готово: история и задачи приведены в порядок.');

  api.histories.set(id, feed.items);
  api.bindConversation(id);
  api.hub.reach(`conversation:${id}`, String(feed.seq));
  return { id, run, blocks: feed.blocks };
}
