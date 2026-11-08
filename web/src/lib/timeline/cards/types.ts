import type { Component } from 'svelte';
import type { BackgroundFact } from '#lib/stores/background.svelte.js';
import type { BlockModel, BlockPage } from '#lib/stores/blocks.js';
import type { PlanTaskState } from '#lib/stores/plan.svelte.js';
import type { BlockLinks } from '#lib/timeline/rows.js';
import type { HubEvent } from '#lib/timeline/hubEvent.js';
import type { FeedFolds } from '#lib/timeline/folds.js';
import type { JsonObject } from './json';
import type { Tone } from './copy';

/** Состояние вызова словами ленты: идёт, готов, ошибка, оборван. */
export type CardState = 'running' | 'done' | 'error' | 'interrupted';

/**
 * Голос строки: свой оркестратор владельца говорит о себе в женском роде («Запустила»),
 * делегат и субагент — безлично («Запуск делегата»).
 */
export type Voice = 'she' | 'neutral';

/** Вызов инструмента, разобранный для карточки. */
export interface CardCall {
  /** Канонический id, см. `toolId`. */
  id: string;
  /** Имя, как его прислал харнесс. */
  name: string;
  block: BlockModel;
  result: BlockModel | null;
  /** Вход вызова; у старых записей и у оборванного текста может быть пустым или неполным. */
  input: JsonObject;
  /** Текст вызова оборван: история отдаёт только его начало. */
  partial: boolean;
  /** Вход разобран: в тексте вызова был объект JSON. */
  hasInput: boolean;
  /** Аргумент из сводки воркера: у Claude Code это описание задачи, путь или запрос. */
  argument: string;
  /** Текст ответа без обёрток CLI; пусто, пока ответа нет. */
  output: string;
  /** Объект JSON из ответа; хаб отвечает им на вызовы canterlot. */
  answer: JsonObject | null;
  state: CardState;
  voice: Voice;
  /** Провайдер сессии: claude, codex, opencode … */
  harness: string;
  /** Разговор, в чьей ленте стоит вызов: его план правят инструменты плана. */
  conversationId: string;
  /** Строка сессии из списка владельца: заголовок и живое состояние. */
  session: (id: string) => SessionFacts | null;
  /** События хаба о сессии из той же ленты, по порядку: итог приёмки, решение владельца, старт из очереди. */
  events: (sessionId: string) => HubEvent[];
  /** Прошлый вызов того же инструмента в той же дорожке. */
  previous: () => BlockModel | null;
  /** Задача плана с этим номером по прошлым вызовам плана в ленте: какой она была до этого вызова. */
  planBefore: (number: number) => PlanSnapshot | null;
}

/** Задача плана в ответе прошлого вызова той же ленты. */
export interface PlanSnapshot {
  title: string;
  state: PlanTaskState | null;
}

/** Что карточке видно о чужой сессии из списка владельца. */
export interface SessionFacts {
  id: string;
  title: string;
  state: string;
  accepted: boolean;
  harness: string;
  model: string;
}

/** То, что карточке нужно от ленты помимо самого вызова. */
export interface CardEnv {
  open: boolean;
  onToggle: () => void;
  load: (blockId: string, offset: number) => Promise<BlockPage>;
  now: number;
  working: boolean;
  links: BlockLinks;
  background: BackgroundFact | null;
  conversationId: string;
  /** Раскрытое владельцем внутри карточки: бриф, текст сообщения, весь список задач. */
  folds: FeedFolds;
}

export interface CardProps<D> {
  call: CardCall;
  data: D;
  /** Однострочная сводка из `summary`. */
  line: string;
  /** Ссылка внутри строки и слово состояния: их считает реестр по `link` и `word`. */
  link: CardLink | null;
  word: StateWord | null;
  /** Строка пришла на глазах: карточка вырастает, история стоит сразу. */
  grows: boolean;
  quiet: boolean;
  env: CardEnv;
}

export interface CardDef<D = unknown> {
  /** Данные карточки из вызова; `null` — форма не та, лента покажет обычный блок. */
  parse(call: CardCall): D | null;
  /** Одна строка: заголовок карточки, подпись для чтения с экрана и тест. */
  summary(call: CardCall, data: D): string;
  /** Своя разметка; без неё карточка — одна строка с раскрытием сырого вызова. */
  component?: Component<CardProps<D>>;
  /** Служебный вызов: бледная строка, подряд идущие собираются в одну. */
  quiet?: boolean;
  /** Строка не прячется в свёрнутые шаги хода. */
  stays?: boolean;
  /** Карточка, пришедшая на глазах, вырастает из края, как ветка делегирования. */
  grows?: boolean;
  /** Вызов уступает место карточке запроса, если она идёт сразу за ним. */
  yieldsToRequest?: boolean;
  /** Короткое имя для подсчёта в свёрнутых шагах. */
  label?: string;
  /** Часть строки, которая ведёт дальше: сессия, страница, история Taiga. */
  link?(call: CardCall, data: D): CardLink | null;
  /** Слово состояния справа; без него — по состоянию вызова. */
  word?(call: CardCall, data: D, now: number): StateWord | null;
}

export interface CardLink {
  /** Текст внутри строки, который станет ссылкой. */
  text: string;
  href: string;
  external?: boolean;
  /** Ссылка на план этого оркестратора: в своём чате открывает вкладку «План» на месте. */
  plan?: string;
}

export interface StateWord {
  word: string;
  tone: Tone;
  live?: boolean;
}

/** Логика карточки без разметки: так её проверяют юнит-тесты. */
export type CardLogic<D> = Omit<CardDef<D>, 'component'>;
