/**
 * Реестр карточек ленты: канонический id инструмента → логика и разметка.
 * Новая карточка — файл с логикой (или компонент) и одна строка здесь.
 * Чего нет в реестре, лента показывает обычным блоком инструмента.
 */
import type { Component } from 'svelte';
import type { BlockModel } from '#lib/stores/blocks.js';
import AcceptCard from './AcceptCard.svelte';
import AlarmListCard from './AlarmListCard.svelte';
import LimitsCard from './LimitsCard.svelte';
import QuestionCard from './QuestionCard.svelte';
import SendCard from './SendCard.svelte';
import SessionsCard from './SessionsCard.svelte';
import ShareCard from './ShareCard.svelte';
import SpawnCard from './SpawnCard.svelte';
import SubagentCard from './SubagentCard.svelte';
import TodoCard from './TodoCard.svelte';
import { alarmCancel, alarmList, alarmSet } from './defs/alarms';
import { limits } from './defs/limits';
import * as sessions from './defs/sessions';
import { share } from './defs/share';
import { todo } from './defs/todo';
import { context7, question, quiet, skill, subagent, taiga } from './defs/tools';
import { fetchPage, search } from './defs/web';
import { sessionLink } from './links';
import { toolFamily, toolId } from './toolId';
import type { CardCall, CardDef, CardLogic, CardProps, StateWord } from './types';

function card<D>(logic: CardLogic<D>, component?: Component<CardProps<D>>): CardDef<D> {
  return { ...logic, component } as CardDef<D>;
}

/** Ссылка на сессию по полю `session_id` входа. */
function bySession<D>(logic: CardLogic<D>, pick: (call: CardCall, data: D) => string = (call) => String(call.input.session_id ?? '')): CardLogic<D> {
  return { ...logic, link: logic.link ?? ((call, data) => sessionLink(call, pick(call, data))) };
}

/** Однострочная запись управления: своё слово и тон, если они есть. */
function lined(logic: CardLogic<sessions.LineData>): CardLogic<sessions.LineData> {
  return {
    ...bySession(logic, (_call, data) => data.sessionId),
    word: (call, data) => (data.word ? { word: data.word, tone: data.tone } : defaultWord(call))
  };
}

const REGISTRY: Record<string, CardDef<never>> = {
  'canterlot.session_spawn': card({ ...sessions.spawn, grows: true, link: (call, data) => sessionLink(call, data.sessionId, data.title) } as CardLogic<sessions.SpawnData>, SpawnCard),
  'canterlot.session_accept': card(bySession(sessions.accept), AcceptCard),
  'canterlot.session_send': card(bySession(sessions.send), SendCard),
  'canterlot.session_inspect': card(bySession(sessions.inspect, (_call, data) => data.sessionId)),
  'canterlot.sessions_list': card(sessions.sessionsList, SessionsCard),
  'canterlot.session_wait': card(bySession(sessions.wait)),
  'canterlot.session_stop': card(lined(sessions.stop)),
  'canterlot.session_interrupt': card(lined(sessions.interrupt)),
  'canterlot.session_subscribe': card(lined(sessions.subscribe)),
  'canterlot.session_unsubscribe': card(lined(sessions.unsubscribe)),
  'canterlot.nudge_set': card(lined(sessions.nudgeSet)),
  'canterlot.nudge_clear': card(lined(sessions.nudgeClear)),
  'canterlot.spawn_cancel': card(lined(sessions.spawnCancel)),
  'canterlot.route_preview': card(lined(sessions.routePreview)),
  'canterlot.presets_list': card(lined(sessions.presetsList)),
  'canterlot.preset_save': card(lined(sessions.presetSave)),
  'canterlot.preset_apply': card(lined(sessions.presetApply)),
  'canterlot.alarm_set': card({ ...alarmSet, word: (call, data) => (data.satisfied ? { word: 'уже выполнено', tone: 'success' } : defaultWord(call)) }),
  'canterlot.alarm_cancel': card(alarmCancel),
  'canterlot.alarm_list': card(alarmList, AlarmListCard),
  'canterlot.share_file': card(share, ShareCard),
  'canterlot.limits': card(limits, LimitsCard),
  'todo.write': card(todo, TodoCard),
  'agent.task': card(subagent, SubagentCard),
  'question.ask': card(question, QuestionCard),
  'web.search': card(search),
  'web.fetch': card({ ...fetchPage, link: (_call, data) => ({ text: data.domain, href: data.url, external: true }) }),
  skill: card(skill),
  'meta.tool_search': card(quiet),
  'meta.monitor': card(quiet),
  'meta.task_stop': card(quiet),
  'meta.task_output': card(quiet),
  'meta.list_agents': card(quiet),
  'meta.send_message': card(quiet),
  'meta.push_notification': card(quiet),
  'todo.read': card(quiet)
} as Record<string, CardDef<never>>;

/** Семейства, где карточка одна на все операции сервера. */
const FAMILIES: Record<string, CardDef<never>> = {
  taiga: card({ ...taiga, link: (_call, data) => (data.url ? { text: data.ref ? `#${data.ref}` : 'Taiga', href: data.url, external: true } : null) }),
  context7: card(context7)
} as Record<string, CardDef<never>>;

export function cardDef(id: string): CardDef<unknown> | null {
  return (REGISTRY[id] ?? FAMILIES[toolFamily(id)] ?? null) as CardDef<unknown> | null;
}

/** Все ids с карточкой: для тестов реестра. */
export function cardIds(): string[] {
  return Object.keys(REGISTRY);
}

/** Слово состояния по умолчанию: готовый вызов молчит, остальные называют своё состояние. */
export function defaultWord(call: Pick<CardCall, 'state'>): StateWord | null {
  switch (call.state) {
    case 'running':
      return { word: 'идёт', tone: 'accent', live: true };
    case 'error':
      return { word: 'ошибка', tone: 'danger' };
    case 'interrupted':
      return { word: 'прервано', tone: 'warning' };
    default:
      return null;
  }
}

export interface Resolved {
  def: CardDef<unknown>;
  data: unknown;
  line: string;
  link: ReturnType<NonNullable<CardDef['link']>>;
  word: StateWord | null;
}

/**
 * Карточка вызова, если она есть и вход ей по форме. Любая ошибка разбора — `null`:
 * лента покажет обычный блок, а не пустое место.
 */
export function resolveCard(call: CardCall, now: number): Resolved | null {
  const def = cardDef(call.id);
  if (!def) return null;
  try {
    const data = def.parse(call);
    if (data === null || data === undefined) return null;
    const line = def.summary(call, data);
    if (!line) return null;
    return {
      def,
      data,
      line,
      link: def.link?.(call, data) ?? null,
      word: def.word ? def.word(call, data, now) : defaultWord(call)
    };
  } catch {
    return null;
  }
}

function defOf(block: Pick<BlockModel, 'toolName' | 'kind'>): CardDef<unknown> | null {
  return block.kind === 'tool_call' ? cardDef(toolId(block.toolName)) : null;
}

/** Строка держится на виду: свёрнутые шаги её не прячут. */
export function cardStays(block: Pick<BlockModel, 'toolName' | 'kind'>): boolean {
  return defOf(block)?.stays === true;
}

/** Служебный вызов: бледная строка, подряд идущие собираются в одну. */
export function cardQuiet(block: Pick<BlockModel, 'toolName' | 'kind'>): boolean {
  return defOf(block)?.quiet === true;
}

/** Вызов уступает место карточке запроса, если она идёт сразу за ним. */
export function cardYieldsToRequest(block: Pick<BlockModel, 'toolName' | 'kind'>): boolean {
  return defOf(block)?.yieldsToRequest === true;
}

/** Короткое имя для подсчёта в свёрнутых шагах; без карточки — имя инструмента. */
export function cardLabel(block: Pick<BlockModel, 'toolName' | 'kind'>): string | null {
  return defOf(block)?.label ?? null;
}
