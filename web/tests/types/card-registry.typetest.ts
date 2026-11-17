/**
 * Проверка типов реестра карточек: файл не запускается, его проверяет `npm run check`
 * (svelte-check по tsconfig, куда входит каталог tests). Каждая `@ts-expect-error` — пара,
 * которая не должна собираться; если реестр снова начнёт стирать тип данных, директива
 * окажется лишней, и проверка упадёт с «Unused '@ts-expect-error' directive».
 */
import ShellCard from '../../src/lib/timeline/cards/ShellCard.svelte';
import TodoCard from '../../src/lib/timeline/cards/TodoCard.svelte';
import { shell, type ShellData } from '../../src/lib/timeline/cards/defs/shell';
import { todo, type TodoData } from '../../src/lib/timeline/cards/defs/todo';
import { card } from '../../src/lib/timeline/cards/registry';
import type { AnyCard, CardCall, CardView } from '../../src/lib/timeline/cards/types';

declare const call: CardCall;

// Верные пары собираются: логика и разметка про одни данные, строка без разметки — тоже.
export const fine: AnyCard[] = [card(todo, TodoCard), card(shell, ShellCard), card(shell)];

// @ts-expect-error разметка команды не умеет рисовать данные списка задач
export const wrongComponent = card(todo, ShellCard);

// @ts-expect-error слово состояния ждёт данные команды, а parse отдаёт задачи
export const wrongWord = card({ ...todo, word: (_call: CardCall, data: ShellData) => ({ word: data.command, tone: 'muted' as const }) }, TodoCard);

// @ts-expect-error ссылка ждёт данные задач, а карточка — команды
export const wrongLink = card({ ...shell, link: (_call: CardCall, data: TodoData) => ({ text: data.items[0]!.text, href: '/' }) }, ShellCard);

// @ts-expect-error в реестр попадает только собранная `card()` карточка, а не голое описание
export const rawEntry: Record<string, AnyCard> = { 'shell.exec': { ...shell, component: ShellCard } };

// Снаружи тип данных не виден: только `unknown`, без приведения его не прочитать как чужой.
const resolved = card(shell, ShellCard).resolve(call, 0);
// @ts-expect-error данные готовой карточки — unknown, а не данные команды
export const leaked: ShellData | undefined = resolved?.data;
// Разметка с данными одного вида встаёт в общий список без приведения типов.
export const views: CardView<unknown>[] = resolved ? [resolved.view] : [];
