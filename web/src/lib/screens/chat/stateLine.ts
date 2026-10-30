import { activityLine, type Activity, type Tone } from '#lib/screens/sessions/activityText.js';

export interface ChatStateLine {
	text: string;
	tone: Tone;
	live: boolean;
	/** Простой сессии не занимает строку шапки: там остаётся подпись с агентом и машиной. */
	quiet: boolean;
}

export function chatStateLine(
	activity: Activity | null,
	phase: { phase?: string } | null,
	executionState: string | null | undefined,
	now: number
): ChatStateLine {
	const effective = effectiveActivity(activity, phase, executionState);
	const line = activityLine(effective, { now });
	// Вопрос целиком стоит в карточке прямо под шапкой, повторять его негде.
	const text = effective.state === 'waiting_for_user' ? 'Ждёт ответа' : line.text;
	return { ...line, text, quiet: effective.state === 'idle' };
}

function effectiveActivity(
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
