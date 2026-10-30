export type InputUiState =
	| 'saving'
	| 'queued'
	| 'dispatched'
	| 'on_machine'
	| 'transmitting'
	| 'accepted'
	| 'acceptance_unconfirmed'
	| 'cancelled'
	| 'rejected';

export type InputPlacement = 'strip' | 'feed' | 'none';

export interface AttachmentRef {
	attachment_id: string;
	name: string;
	mime: string;
	size: number;
	sha256: string;
	image: boolean;
}

export interface InputView {
	key: string;
	id: string | null;
	text: string;
	attachments: AttachmentRef[];
	state: InputUiState;
	placement: InputPlacement;
	marks: { saved: boolean; accepted: boolean };
	cancelRequested: boolean;
	attempt: number | null;
	createdSeq: string | null;
	feedSeq: string | null;
	createdAt: number | null;
	retryOf: string | null;
	localOnly: boolean;
	rejection?: string;
	stripUntil: number | null;
}

const FEED: ReadonlySet<InputUiState> = new Set([
	'transmitting',
	'accepted',
	'acceptance_unconfirmed',
	'rejected'
]);

export function placeInput(state: InputUiState): InputPlacement {
	if (state === 'cancelled') return 'none';
	if (FEED.has(state)) return 'feed';
	return 'strip';
}

export function marksFor(state: InputUiState): { saved: boolean; accepted: boolean } {
	return {
		saved: state !== 'saving',
		accepted: state === 'accepted'
	};
}

export const CANCEL_REQUESTED_LABEL = 'отмена запрошена';

export function freshInput(key: string, text: string, createdAt: number, attachments: AttachmentRef[] = []): InputView {
	return {
		key,
		id: null,
		text,
		attachments: [...attachments],
		state: 'saving',
		placement: 'strip',
		marks: marksFor('saving'),
		cancelRequested: false,
		attempt: null,
		createdSeq: null,
		feedSeq: null,
		createdAt,
		retryOf: null,
		localOnly: true,
		stripUntil: null
	};
}

/** Текст и вложения из тела операции журнала: это же тело уходит в POST. */
export function inputOfBody(body: unknown): { text: string; attachmentIds: string[] } {
	const { content, attachment_ids } = (body ?? {}) as {
		content?: { type: string; text?: string }[];
		attachment_ids?: string[];
	};
	return {
		text: (content ?? [])
			.filter((block) => block.type === 'text')
			.map((block) => block.text ?? '')
			.join(''),
		attachmentIds: attachment_ids ?? []
	};
}

export function blankAttachment(attachment_id: string): AttachmentRef {
	return { attachment_id, name: '', mime: '', size: 0, sha256: '', image: false };
}

const FEED_STATES: ReadonlySet<string> = FEED;

export function assignFeedSeq(input: InputView, state: InputUiState, scopeSeq: string): void {
	if (input.feedSeq !== null) return;
	if (!FEED_STATES.has(state)) return;
	input.feedSeq = scopeSeq;
}
