/**
 * Реестр карточек ленты: канонический id инструмента → логика и разметка.
 * Новая карточка — файл с логикой (или компонент) и одна строка здесь.
 * Чего нет в реестре, лента показывает обычным блоком инструмента.
 */
import type { Component } from 'svelte';
import type { BlockModel } from '#lib/stores/blocks.js';
import AcceptCard from './AcceptCard.svelte';
import AlarmListCard from './AlarmListCard.svelte';
import FindCard from './FindCard.svelte';
import ForgejoCard from './ForgejoCard.svelte';
import ReadCard from './ReadCard.svelte';
import EditCard from './EditCard.svelte';
import LimitsCard from './LimitsCard.svelte';
import MeetingCard from './MeetingCard.svelte';
import QuestionCard from './QuestionCard.svelte';
import SendCard from './SendCard.svelte';
import SessionsCard from './SessionsCard.svelte';
import ShellCard from './ShellCard.svelte';
import SkillCard from './SkillCard.svelte';
import ShareCard from './ShareCard.svelte';
import SpawnCard from './SpawnCard.svelte';
import SubagentCard from './SubagentCard.svelte';
import TaigaCard from './TaigaCard.svelte';
import TodoCard from './TodoCard.svelte';
import MemoryGetCard from './MemoryGetCard.svelte';
import MemorySearchCard from './MemorySearchCard.svelte';
import MemoryUpdateCard from './MemoryUpdateCard.svelte';
import MemoryWriteCard from './MemoryWriteCard.svelte';
import PlanAddCard from './plan/PlanAddCard.svelte';
import PlanAdoptCard from './plan/PlanAdoptCard.svelte';
import PlanListCard from './plan/PlanListCard.svelte';
import PlanUpdateCard from './plan/PlanUpdateCard.svelte';
import { planHref } from './plan/livePlan';
import { alarmCancel, alarmList, alarmSet } from './defs/alarms';
import { limits } from './defs/limits';
import { meeting } from './defs/meeting';
import * as memory from './defs/memory';
import { planAdd, planAdopt, planList, planUpdate } from './defs/plan';
import * as sessions from './defs/sessions';
import { find, read } from './defs/explore';
import { edit, patch, write } from './defs/files';
import { forgejo } from './defs/forgejo';
import { share } from './defs/share';
import { shell } from './defs/shell';
import { todo } from './defs/todo';
import { context7, question, quiet, skill, subagent, taiga } from './defs/tools';
import { fetchPage, search } from './defs/web';
import { sessionLink } from './links';
import { outcomeWord, type Outcome } from './outcome';
import { toolFamily, toolId } from './toolId';
import type { IconName } from '#lib/ui/icons.js';
import LineCard from './LineCard.svelte';
import type { AnyCard, CardCall, CardLogic, CardProps, ResolvedCard, StateWord } from './types';

/**
 * Карточка реестра: логика и разметка проверяются на один и тот же тип данных здесь, в точке
 * вызова, и дальше живут вместе. Без своей разметки карточка — одна строка (`LineCard`).
 * Любая ошибка разбора — `null`: лента покажет обычный блок, а не пустое место.
 */
export function card<D>(logic: CardLogic<D>, component: Component<CardProps<D>> = LineCard): AnyCard {
  const self: AnyCard = {
    quiet: logic.quiet === true,
    stays: logic.stays === true,
    grows: logic.grows === true,
    yieldsToRequest: logic.yieldsToRequest === true,
    label: logic.label ?? null,
    fold: logic.fold ?? null,
    icon: logic.icon ?? null,
    resolve(call, now) {
      try {
        const data = logic.parse(call);
        if (data === null || data === undefined) return null;
        const line = logic.summary(call, data);
        if (!line) return null;
        return {
          def: self,
          data,
          line,
          link: logic.link?.(call, data) ?? null,
          word: logic.word ? logic.word(call, data, now) : defaultWord(call),
          icon: self.icon,
          view: { data, render: component }
        };
      } catch {
        return null;
      }
    }
  };
  return self;
}

/**
 * Значки семейств собственных инструментов canterlot. Один значок на семейство, а не на глагол:
 * поиск, чтение, запись и архив памяти — всё один мозг, глагол стоит первым словом строки.
 * У инструментов харнесса значка нет, и они этим помощником не проходят.
 * Задачи на месте (`job_*`) получат `hammer`, когда у них появятся карточки; `history_search`
 * (`search`) — когда у него появится карточка.
 */
const family = {
  memory: 'brain',
  delegates: 'branch',
  sessions: 'chats',
  alarms: 'alarm',
  plan: 'list',
  limits: 'gauge',
  share: 'share'
} as const satisfies Record<string, IconName>;

/** Значок семейства в описании карточки. */
function marked<D>(icon: IconName, logic: CardLogic<D>): CardLogic<D> {
  return { ...logic, icon };
}

/** Ссылка на сессию по полю `session_id` входа. */
function bySession<D>(logic: CardLogic<D>, pick: (call: CardCall, data: D) => string = (call) => String(call.input.session_id ?? '')): CardLogic<D> {
  return { ...logic, link: logic.link ?? ((call, data) => sessionLink(call, pick(call, data))) };
}

/** Слово «План» в строке ведёт в план оркестратора этой ленты. */
function toPlan<D>(logic: CardLogic<D>): CardLogic<D> {
  return {
    ...logic,
    link: (call) => {
      const id = call.conversationId;
      return id ? { text: 'План', href: planHref(id), plan: id } : null;
    }
  };
}

/** Однострочная запись управления: своё слово и тон, если они есть. */
function lined(logic: CardLogic<sessions.LineData>): CardLogic<sessions.LineData> {
  return {
    ...bySession(logic, (_call, data) => data.sessionId),
    word: (call, data) => (data.word ? { word: data.word, tone: data.tone } : defaultWord(call))
  };
}

/** Отказ, ненулевой код и сорванный вызов говорят своим словом; остальное — по состоянию. */
function outcomed<D extends { outcome: Outcome }>(logic: CardLogic<D>): CardLogic<D> {
  return { ...logic, word: (call, data) => outcomeWord(data.outcome) ?? defaultWord(call) };
}

/** Готовый вызов памяти говорит своё слово; ошибка, ход и обрыв — слово состояния. */
function remembered<D>(logic: CardLogic<D>, done: (data: D) => StateWord | null): CardLogic<D> {
  return { ...logic, word: (call, data) => (call.state === 'done' ? done(data) : defaultWord(call)) };
}

const REGISTRY: Record<string, AnyCard> = {
  'canterlot.memory_search': card(marked(family.memory, remembered(memory.memorySearch, (data) => (data.refusal ? null : memory.searchWord(data)))), MemorySearchCard),
  'canterlot.memory_get': card(marked(family.memory, memory.memoryGet), MemoryGetCard),
  'canterlot.memory_write': card(marked(family.memory, memory.memoryWrite), MemoryWriteCard),
  'canterlot.memory_update': card(marked(family.memory, remembered(memory.memoryUpdate, memory.updateWord)), MemoryUpdateCard),
  'canterlot.memory_delete': card(marked(family.memory, memory.memoryDelete)),
  'canterlot.session_spawn': card<sessions.SpawnData>({ ...sessions.spawn, icon: family.delegates, grows: true, link: (call, data) => sessionLink(call, data.sessionId, data.title) }, SpawnCard),
  'canterlot.session_accept': card(marked(family.delegates, bySession(sessions.accept)), AcceptCard),
  'canterlot.session_send': card(marked(family.delegates, bySession(sessions.send)), SendCard),
  'canterlot.session_inspect': card(marked(family.delegates, bySession(sessions.inspect, (_call, data) => data.sessionId))),
  'canterlot.sessions_list': card(marked(family.sessions, sessions.sessionsList), SessionsCard),
  'canterlot.session_wait': card(marked(family.delegates, bySession(sessions.wait))),
  'canterlot.session_stop': card(marked(family.delegates, lined(sessions.stop))),
  'canterlot.session_interrupt': card(marked(family.delegates, lined(sessions.interrupt))),
  'canterlot.session_subscribe': card(marked(family.delegates, lined(sessions.subscribe))),
  'canterlot.session_unsubscribe': card(marked(family.delegates, lined(sessions.unsubscribe))),
  'canterlot.nudge_set': card(marked(family.alarms, lined(sessions.nudgeSet))),
  'canterlot.nudge_clear': card(marked(family.alarms, lined(sessions.nudgeClear))),
  'canterlot.spawn_cancel': card(marked(family.delegates, lined(sessions.spawnCancel))),
  'canterlot.route_preview': card(lined(sessions.routePreview)),
  'canterlot.presets_list': card(lined(sessions.presetsList)),
  'canterlot.preset_save': card(lined(sessions.presetSave)),
  'canterlot.preset_apply': card(lined(sessions.presetApply)),
  'canterlot.alarm_set': card({ ...alarmSet, icon: family.alarms, word: (call, data) => (data.satisfied ? { word: 'уже выполнено', tone: 'success' } : defaultWord(call)) }),
  'canterlot.alarm_cancel': card(marked(family.alarms, alarmCancel)),
  'canterlot.alarm_list': card(marked(family.alarms, alarmList), AlarmListCard),
  'canterlot.share_file': card(marked(family.share, share), ShareCard),
  'canterlot.limits': card(marked(family.limits, limits), LimitsCard),
  'canterlot.plan_add': card(marked(family.plan, toPlan(planAdd)), PlanAddCard),
  'canterlot.plan_update': card(marked(family.plan, planUpdate), PlanUpdateCard),
  'canterlot.plan_list': card(marked(family.plan, toPlan(planList)), PlanListCard),
  'canterlot.plan_adopt': card({ ...planAdopt, icon: family.plan, link: (call, data) => sessionLink(call, data.from) }, PlanAdoptCard),
  'todo.write': card(todo, TodoCard),
  'agent.task': card(subagent, SubagentCard),
  'question.ask': card(question, QuestionCard),
  'web.search': card(search),
  'web.fetch': card({ ...fetchPage, link: (_call, data) => ({ text: data.domain, href: data.url, external: true }) }),
  skill: card(skill, SkillCard),
  'file.read': card(read, ReadCard),
  'file.search': card(find, FindCard),
  'file.edit': card(outcomed(edit), EditCard),
  'file.write': card(outcomed(write), EditCard),
  'file.patch': card(outcomed(patch), EditCard),
  'shell.exec': card(outcomed(shell), ShellCard),
  'meta.tool_search': card(quiet),
  'meta.monitor': card(quiet),
  'meta.task_stop': card(quiet),
  'meta.task_output': card(quiet),
  'meta.list_agents': card(quiet),
  'meta.send_message': card(quiet),
  'meta.push_notification': card(quiet),
  'todo.read': card(quiet),
  'taiga.whoami': card(quiet),
  'taiga.project_schema': card(quiet),
  'taiga.project_list': card(quiet),
  'taiga.project_get': card(quiet),
  'taiga.stats': card(quiet)
};

/** Семейства, где карточка одна на все операции сервера. */
const FAMILIES: Record<string, AnyCard> = {
  taiga: card({ ...taiga, link: (_call, data) => (data.url ? { text: data.ref ? `#${data.ref}` : 'Taiga', href: data.url, external: true } : null) }, TaigaCard),
  context7: card(context7),
  forgejo: card(forgejo, ForgejoCard),
  meeting: card(meeting, MeetingCard)
};

export function cardDef(id: string): AnyCard | null {
  return REGISTRY[id] ?? FAMILIES[toolFamily(id)] ?? null;
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

/** Карточка вызова, если она есть и вход ей по форме. */
export function resolveCard(call: CardCall, now: number): ResolvedCard | null {
  return cardDef(call.id)?.resolve(call, now) ?? null;
}

type FoldBlock = Pick<BlockModel, 'toolName' | 'kind'> & { media?: readonly unknown[] };

function defOf(block: FoldBlock): AnyCard | null {
  // Медиа живёт в обычном блоке инструмента: бледная строка свёртки его не показывает.
  if (block.media && block.media.length > 0) return null;
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

/** Семейство свёртки бледной строки: `explore` у чтений и поисков; у остальных `null`. */
export function cardFold(block: Pick<BlockModel, 'toolName' | 'kind'>): string | null {
  return defOf(block)?.fold ?? null;
}

/** Вызов уступает место карточке запроса, если она идёт сразу за ним. */
export function cardYieldsToRequest(block: Pick<BlockModel, 'toolName' | 'kind'>): boolean {
  return defOf(block)?.yieldsToRequest === true;
}

/** Короткое имя для подсчёта в свёрнутых шагах; без карточки — имя инструмента. */
export function cardLabel(block: Pick<BlockModel, 'toolName' | 'kind'>): string | null {
  return defOf(block)?.label ?? null;
}
