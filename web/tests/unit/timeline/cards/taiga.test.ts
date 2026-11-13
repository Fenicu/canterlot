import { describe, expect, it } from 'vitest';
import { cardCall } from '../../../../src/lib/timeline/cards/call';
import { cardQuiet, resolveCard } from '../../../../src/lib/timeline/cards/registry';
import type { TaigaData } from '../../../../src/lib/timeline/cards/defs/tools';
import { callBlock, hub, made, resultBlock } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');
const US_URL = 'https://taiga.example.com/project/sklad/us/123';
const TASK_URL = 'https://taiga.example.com/project/sklad/task/201';

const STORY = {
  ref: 123,
  subject: 'Фильтр товаров по складу',
  status: 'В работе',
  assigned_to: 'Анна Смирнова',
  assigned_users: ['Анна Смирнова', 'Илья Котов'],
  sprint: 'Спринт 8',
  points: 5,
  tags: ['склад'],
  is_blocked: true,
  is_closed: false,
  total_comments: 3,
  permalink: US_URL
};

function taiga<D = TaigaData>(call: ReturnType<typeof made>) {
  return resolveCard(call, NOW) as (NonNullable<ReturnType<typeof resolveCard>> & { data: D }) | null;
}

describe('taiga get', () => {
  it('opens_a_story_with_status_assignee_sprint_points_and_the_blocked_word', () => {
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_userstory_get', { project: 'sklad', ref: 123 }, hub(STORY)))!;
    expect(resolved.line).toBe('Taiga · открыла историю #123 «Фильтр товаров по складу»');
    expect(resolved.link).toEqual({ text: '#123', href: US_URL, external: true });
    expect(resolved.data).toMatchObject({
      kind: 'get',
      entity: 'userstory',
      status: 'В работе',
      assigned: 'Анна Смирнова',
      sprint: 'Спринт 8',
      points: '5 очков',
      blocked: true
    });
  });

  it('a_task_gets_its_own_word_and_the_masculine_blocked_form_for_an_issue', () => {
    const answer = { ref: 45, subject: 'Проверить выгрузку остатков', status: 'Новая', assigned_to: 'Илья Котов', tags: [], is_closed: false, permalink: TASK_URL };
    const resolved = taiga(made('taiga.taiga_task_get', { project: 'sklad', ref: 45 }, hub(answer), { root: false }))!;
    expect(resolved.line).toBe('Taiga · задача #45 «Проверить выгрузку остатков»');
    expect(resolved.data.entity).toBe('task');
    expect(resolved.data.blockedWord).toBe('заблокирована');
    expect(taiga(made('taiga_taiga_issue_get', { project: 'sklad', ref: 7 }, hub({ ref: 7, subject: 'Сбой выгрузки', status: 'Новая' })))!.data.blockedWord).toBe('заблокирован');
  });

  it('falls_back_to_the_assigned_users_list_when_there_is_no_single_assignee', () => {
    const resolved = taiga(made('taiga_userstory_get', { project: 'sklad', ref: 123 }, hub({ ref: 123, subject: 'Фильтр товаров по складу', assigned_users: ['Анна Смирнова', 'Илья Котов'] })))!;
    expect(resolved.data.assigned).toBe('Анна Смирнова, Илья Котов');
  });

  it('a_missing_answer_keeps_the_ref_line_and_does_not_throw', () => {
    const resolved = taiga(made('taiga_taiga_userstory_get', { project: 'sklad', ref: 123 }, null, { working: true }))!;
    expect(resolved.line).toBe('Taiga · открыла историю #123');
    expect(resolved.data.rows).toEqual([]);
    expect(resolved.link).toBeNull();
    expect(resolved.word).toMatchObject({ word: 'идёт' });
  });
});

describe('taiga update', () => {
  const input = { project: 'sklad', ref: 123, status: 'Готово к тестированию', assigned_to: 'Илья Котов', add_tags: ['срочно'], subject: 'Фильтр товаров по складу' };
  const answer = { ...STORY, status: 'Готово к тестированию', assigned_to: 'Илья Котов', tags: ['склад', 'срочно'] };

  it('names_every_change_from_the_call_input', () => {
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_userstory_update', input, hub(answer)))!;
    expect(resolved.line).toBe('Taiga · обновила историю #123 «Фильтр товаров по складу»');
    expect(resolved.data.changes).toEqual([
      'статус → Готово к тестированию',
      'исполнитель → Илья Котов',
      'тема → «Фильтр товаров по складу»',
      'добавила теги: срочно'
    ]);
  });

  it('uses_names_from_the_answer_when_the_input_has_ids', () => {
    const resolved = taiga(made('taiga_taiga_task_update', { project: 'sklad', ref: 45, status: 7, assigned_to: 42 }, hub({ ref: 45, subject: 'Сверить остатки', status: 'В работе', assigned_to: 'Илья Котов' })))!;
    expect(resolved.data.changes).toEqual(['статус → В работе', 'исполнитель → Илья Котов']);
  });

  it('clearing_fields_reads_as_removal', () => {
    const resolved = taiga(made('taiga_userstory_update', { project: 'sklad', ref: 123, assigned_to: '', sprint: '', tags: [], epic: '' }, hub(answer)))!;
    expect(resolved.data.changes).toEqual([
      'сняла исполнителя',
      'убрала из спринта',
      'сняла теги',
      'убрала из эпика'
    ]);
  });

  it('tag_replacement_and_other_fields_are_named_in_russian', () => {
    const resolved = taiga(made('taiga_userstory_update', {
      project: 'sklad',
      ref: 123,
      tags: ['backend', 'склад'],
      description: 'Новый текст',
      append_description: 'Ещё строка',
      points: { Front: '5', Back: '3' },
      due_date: '2026-12-01',
      is_blocked: true,
      user_story: 34
    }, hub(answer)))!;
    expect(resolved.data.changes).toEqual([
      'описание изменено',
      'описание дополнено',
      'теги → backend, склад',
      'очки → Front 5, Back 3',
      'срок → 2026-12-01',
      'заблокировала',
      'история → #34'
    ]);
  });

  it('an_unexpected_shape_still_has_the_one_line_summary', () => {
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_issue_update', 'not json at all', 'still not json'))!;
    expect(resolved.line).toContain('Taiga ·');
    expect(resolved.data.changes).toEqual([]);
  });

  it('an_error_shows_its_text', () => {
    const call = cardCall(
      callBlock('mcp__plugin_taiga_taiga__taiga_userstory_update', { project: 'sklad', ref: 123, sprint: 'Спринт 9' }),
      resultBlock('Error: "Спринт 9" is not a sprint in this project.', { summary: 'error' }),
      { root: true }
    );
    const resolved = taiga(call)!;
    expect(resolved.word).toMatchObject({ word: 'ошибка', tone: 'danger' });
    expect(resolved.data.error).toBe('"Спринт 9" is not a sprint in this project.');
  });
});

describe('taiga create', () => {
  it('shows_the_number_and_subject_of_the_new_record', () => {
    const resolved = taiga(made('taiga_taiga_task_create', { project: 'sklad', subject: 'Проверить выгрузку остатков', user_story: 123 }, hub({ ref: 207, subject: 'Проверить выгрузку остатков', status: 'Новая', assigned_to: 'Анна Смирнова', user_story: 123, tags: [], is_closed: false, permalink: TASK_URL })))!;
    expect(resolved.line).toBe('Taiga · создала задачу #207 «Проверить выгрузку остатков»');
    expect(resolved.link).toEqual({ text: '#207', href: TASK_URL, external: true });
    expect(resolved.data.rows[0]).toMatchObject({ ref: '207', subject: 'Проверить выгрузку остатков', status: 'Новая' });
  });

  it('bulk_create_counts_the_batch_and_keeps_the_created_rows', () => {
    const created = [
      { ref: 201, subject: 'Настроить выгрузку остатков', status: 'Новая' },
      { ref: 202, subject: 'Сверить колонки CSV', status: 'Новая' }
    ];
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_bulk_create', { project: 'sklad', resource: 'task', items: [{ subject: 'a' }, { subject: 'b' }] }, hub({ created, failed: [] })))!;
    expect(resolved.line).toBe('Taiga · создала 2 задачи');
    expect(resolved.data.created.map((row) => row.ref)).toEqual(['201', '202']);
    expect(resolved.data.failed).toBe(0);
  });
});

describe('taiga comments', () => {
  it('comment_add_keeps_the_comment_text', () => {
    const text = 'Первая строка\nВторая строка\nТретья строка';
    const resolved = taiga(made('taiga_taiga_comment_add', { project: 'sklad', resource: 'userstory', ref: 123, comment: text }, hub({ added: true, resource: 'userstory', id: 9123 })))!;
    expect(resolved.line).toBe('Taiga · комментарий к истории #123');
    expect(resolved.data.comment).toBe(text);
    expect(resolved.data.target).toBe('userstory');
  });

  it('comment_list_counts_and_shows_the_last_comment', () => {
    const items = [
      { author: 'Анна Смирнова', created_at: '2026-11-06T10:00:00Z', comment: 'Старый комментарий' },
      { author: 'Илья Котов', created_at: '2026-11-07T10:00:00Z', comment: 'Последний комментарий' }
    ];
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_comment_list', { project: 'sklad', resource: 'task', ref: 45 }, hub({ total: 2, items })))!;
    expect(resolved.line).toBe('Taiga · комментарии к задаче #45: 2');
    expect(resolved.data.totalComments).toBe(2);
    expect(resolved.data.lastComment).toEqual({ author: 'Илья Котов', text: 'Последний комментарий' });
  });
});

describe('taiga lists and search', () => {
  const items = Array.from({ length: 12 }, (_, index) => ({ ref: 100 + index, subject: `История ${index + 1}`, status: index % 2 === 0 ? 'Новая' : 'В работе', assigned_to: 'Анна Смирнова' }));

  it('list_names_the_filter_and_counts_the_answer', () => {
    const resolved = taiga(made('taiga_taiga_userstory_list', { project: 'sklad', sprint: 'Спринт 8' }, hub({ total: 12, page: 1, has_more: false, items })))!;
    expect(resolved.line).toBe('Taiga · истории спринта «Спринт 8»: 12');
    expect(resolved.data.rows).toHaveLength(12);
    expect(resolved.data.rows[0]).toMatchObject({ ref: '100', subject: 'История 1', status: 'Новая' });
  });

  it('list_without_a_filter_reads_as_a_plain_count', () => {
    const resolved = taiga(made('taiga_task_list', { project: 'sklad' }, hub({ total: 2, items: items.slice(0, 2) })))!;
    expect(resolved.line).toBe('Taiga · задачи: 2');
  });

  it('sprint_and_wiki_lists_use_their_own_titles', () => {
    const sprints = taiga(made('taiga_taiga_sprint_list', { project: 'sklad' }, hub({ total: 2, items: [{ id: 1, name: 'Спринт 7', closed: true }, { id: 2, name: 'Спринт 8', closed: false }] })))!;
    expect(sprints.line).toBe('Taiga · спринты: 2');
    expect(sprints.data.rows[1]).toMatchObject({ subject: 'Спринт 8', status: 'открыт' });
    const wiki = taiga(made('taiga_wiki_list', { project: 'sklad' }, hub({ total: 1, items: [{ id: 9, slug: 'home', modified_date: '2026-11-01' }] })))!;
    expect(wiki.line).toBe('Taiga · страницы вики: 1');
    expect(wiki.data.rows[0]).toMatchObject({ subject: 'home' });
  });

  it('search_flattens_every_bucket_and_uses_the_query', () => {
    const answer = {
      count: 3,
      userstories: [{ ref: 123, subject: 'Фильтр товаров по складу', status: 'В работе' }],
      tasks: [{ ref: 201, subject: 'Настроить выгрузку остатков', status: 'Новая' }],
      issues: [],
      epics: [],
      wikipages: [{ id: 9, slug: 'home', modified_date: '2026-11-01' }]
    };
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_search', { project: 'sklad', text: 'склад' }, hub(answer)))!;
    expect(resolved.line).toBe('Taiga · поиск «склад»: 3');
    expect(resolved.data.rows.map((row) => row.kind)).toEqual(['userstory', 'task', 'wiki']);
  });
});

describe('taiga link and delete', () => {
  const answer = {
    from: { ref: 201, kind: 'task', subject: 'Настроить выгрузку остатков' },
    to: { ref: 123, kind: 'userstory', subject: 'Фильтр товаров по складу' },
    changed: ['to:is_blocked']
  };

  it('link_names_both_sides', () => {
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_link', { project: 'sklad', from: 201, to: 123, type: 'blocks' }, hub(answer)))!;
    expect(resolved.line).toBe('Taiga · связала задачу #201 с историей #123');
  });

  it('link_removal_reads_as_unlinking', () => {
    const resolved = taiga(made('taiga_taiga_link', { project: 'sklad', from: 201, to: 123, type: 'blocks', remove: true }, hub(answer)))!;
    expect(resolved.line).toBe('Taiga · сняла связь задачи #201 с историей #123');
  });

  it('delete_stays_one_line', () => {
    const resolved = taiga(made('mcp__plugin_taiga_taiga__taiga_task_delete', { project: 'sklad', ref: 198, confirm: true }, hub({ deleted: true, resource: 'task', id: 9 })))!;
    expect(resolved.line).toBe('Taiga · удалила задачу #198');
  });
});

describe('taiga service calls', () => {
  const calls: [string, string][] = [
    ['mcp__plugin_taiga_taiga__taiga_whoami', 'Профиль Taiga'],
    ['mcp__plugin_taiga_taiga__taiga_project_schema', 'Схема проекта'],
    ['mcp__plugin_taiga_taiga__taiga_project_list', 'Проекты Taiga'],
    ['mcp__plugin_taiga_taiga__taiga_stats', 'Статистика Taiga: проект']
  ];

  it('whoami_schema_and_project_list_are_quiet_rows', () => {
    for (const [name, line] of calls) {
      const resolved = taiga(made(name, { project: 'sklad' }, hub({ id: 1 })))!;
      expect(resolved.line, name).toBe(line);
      expect(resolved.def.quiet, name).toBe(true);
      expect(cardQuiet({ kind: 'tool_call', toolName: name }), name).toBe(true);
    }
  });

  it('project_get_and_stats_name_what_they_read', () => {
    expect(taiga(made('mcp__plugin_taiga_taiga__taiga_project_get', { project: 'sklad' }, hub({ id: 1, slug: 'sklad', name: 'Склад' })))!.line).toBe('Проект Taiga: sklad');
    expect(taiga(made('mcp__plugin_taiga_taiga__taiga_stats', { project: 'sklad', sprint: 'Спринт 8' }, hub({ scope: 'sprint' })))!.line).toBe('Статистика Taiga: Спринт 8');
  });
});

describe('taiga fallbacks and name forms', () => {
  it('every_harness_name_form_reaches_the_same_card', () => {
    for (const name of ['mcp__plugin_taiga_taiga__taiga_userstory_get', 'taiga_taiga_userstory_get', 'taiga.userstory_get']) {
      const resolved = taiga(made(name, { project: 'sklad', ref: 123 }, hub(STORY)))!;
      expect(resolved.line, name).toBe('Taiga · открыла историю #123 «Фильтр товаров по складу»');
      expect(resolved.data.kind, name).toBe('get');
    }
  });

  it('unknown_taiga_operations_and_shapes_keep_a_line', () => {
    for (const name of ['taiga_userstory_frobnicate', 'taiga_project_ping']) {
      const resolved = taiga(made(name, {}, ''))!;
      expect(resolved.line, name).toContain('Taiga ·');
      expect(resolved.line.length, name).toBeGreaterThan('Taiga · '.length);
    }
  });

  it('wiki_get_create_and_update_read_as_one_line_with_the_title', () => {
    expect(taiga(made('taiga_taiga_wiki_get', { project: 'sklad', slug: 'home' }, hub({ id: 9, slug: 'home', modified_date: '2026-11-01', last_modifier: 'Анна Смирнова', permalink: 'https://taiga.example.com/project/sklad/wiki/home' })))!.line).toBe('Taiga · открыла страницу вики «home»');
    expect(taiga(made('taiga_taiga_wiki_create', { project: 'sklad', slug: 'release-notes', content: 'Текст' }, hub({ id: 10, slug: 'release-notes' })))!.line).toBe('Taiga · создала страницу вики «release-notes»');
    expect(taiga(made('taiga_taiga_wiki_update', { project: 'sklad', id: 10, content: 'Новый текст' }, hub({ id: 10, slug: 'release-notes' })))!.line).toBe('Taiga · обновила страницу вики «release-notes»');
  });
});
