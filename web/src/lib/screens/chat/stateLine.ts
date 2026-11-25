import { activityLine, type Activity, type Tone } from '#lib/screens/sessions/activityText.js';
import type { WaitFacts } from '#lib/requests/waitCopy.js';

export interface ChatStateLine {
	text: string;
	tone: Tone;
	live: boolean;
	/** Простой сессии не занимает строку шапки: там остаётся подпись с агентом и машиной. Фоновая работа — не простой. */
	quiet: boolean;
	/** Слова состояния без длинного довода для строки шапки: «Выполняет shell». Полный текст — в `text`. */
	short: string;
}

/** Имя инструмента без аргументов: «shell» из «shell: cd /srv/…», иначе имя вызова. */
function toolWord(evidence: Activity['evidence']): string {
	const summary = (evidence?.summary ?? '').trim();
	const head = summary.includes(':') ? summary.slice(0, summary.indexOf(':')).trim() : '';
	return head || evidence?.tool_name || summary || 'инструмент';
}

export function chatStateLine(
	activity: Activity | null,
	phase: { phase?: string } | null,
	executionState: string | null | undefined,
	now: number,
	pending: WaitFacts | null = null
): ChatStateLine {
	const effective = effectiveActivity(activity, phase, executionState);
	const line = activityLine(effective, { now, pending });
	// Вопрос целиком стоит в карточке прямо под шапкой, повторять его негде.
	// Без данных запроса остаётся прежняя короткая строка.
	const text = effective.state === 'waiting_for_user' && !pending ? 'Ждёт ответа' : line.text;
	// Время инструмента ведёт строка хода в ленте: шапке хватает слов.
	const evidence = effective.evidence;
	const tool = effective.state === 'working' && evidence?.phase === 'running_tool' && !evidence.lease_wait;
	const short = tool ? `Выполняет ${toolWord(evidence)}` : text;
	return { ...line, text, short, quiet: effective.state === 'idle' && !line.live };
}

/** Активность с поправкой на фазу и остановку исполнения: её же читает знак состояния шапки. */
export function effectiveActivity(
	activity: Activity | null,
	phase: { phase?: string } | null,
	executionState: string | null | undefined
): Activity {
	if (executionState === 'stopping') return { state: 'working', evidence: { phase: 'stopping' } };
	if (!activity) return { state: 'idle' };
	const value = phase?.phase;
	if (value && (!activity.evidence || !activity.evidence.phase)) {
		return { ...activity, evidence: { ...(activity.evidence ?? {}), phase: value } };
	}
	return activity;
}
