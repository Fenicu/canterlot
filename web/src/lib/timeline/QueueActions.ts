import type { InputView } from '$lib/stores/inputs';
import type { OpJournal } from '$lib/ops/journal';
import type { ChatStore } from '$lib/stores/chat.svelte';
import type { DraftStore } from '$lib/drafts/store';
import { cancelInput as apiCancelInput } from '$lib/api/client';
import { freshInput } from '$lib/stores/inputs';

export interface CancelDeps {
	clientCancel?: (id: string, key: string) => Promise<{ data?: unknown } | unknown>;
}

/**
 * Отмена сообщения в очереди или на машине.
 * Результаты соответствуют Delivery D3:
 * - 'cancelled': отмена подтверждена сервером
 * - 'requested': команда отмены отправлена исполнителю
 * - 'lost': передача уже началась, обычная отмена невозможна
 */
export async function cancelInput(
	input: InputView,
	deps?: CancelDeps
): Promise<'cancelled' | 'requested' | 'lost'> {
	if (!input.id) {
		input.state = 'cancelled';
		input.placement = 'none';
		return 'cancelled';
	}

	try {
		const key = crypto.randomUUID();
		const response = deps?.clientCancel
			? await deps.clientCancel(input.id, key)
			: await apiCancelInput(input.id, key);

		const data = (response as { data?: unknown })?.data ?? response;

		if (data === 'cancelled') {
			input.state = 'cancelled';
			input.placement = 'none';
			return 'cancelled';
		}

		if (typeof data === 'object' && data !== null) {
			if ('cancel_requested' in data) {
				input.cancelRequested = true;
				return 'requested';
			}
			if ('too_late' in data) {
				return 'lost';
			}
		}

		return 'lost';
	} catch {
		return 'lost';
	}
}

export interface RetryDeps {
	ops: OpJournal;
	chat: ChatStore;
	conversationId: string;
	confirm?: (message: string) => boolean | Promise<boolean>;
}

/**
 * Повторная отправка неподтверждённого сообщения как нового.
 * Создаёт новый ключ идемпотентности и передаёт retry_of, указывающий на предыдущий ввод (A4 G10).
 */
export async function retryAsNew(
	input: InputView,
	deps?: RetryDeps
): Promise<void> {
	const warning = 'Действие может быть выполнено повторно. Отправить как новое сообщение?';
	const confirmed = deps?.confirm
		? await deps.confirm(warning)
		: typeof window !== 'undefined' && typeof window.confirm === 'function'
			? window.confirm(warning)
			: true;

	if (!confirmed) return;
	if (!deps) return;

	const newKey = crypto.randomUUID();
	const retryTarget = input.id ?? input.key;

	const body = {
		content: [{ type: 'text' as const, text: input.text }],
		attachment_ids: input.attachments.map((a) => a.attachment_id),
		retry_of: retryTarget
	};

	const localInput = freshInput(newKey, input.text, Date.now(), input.attachments);
	localInput.retryOf = retryTarget;
	deps.chat.inputs.set(newKey, localInput);

	const op = await deps.ops.begin('input', body, {
		conversationId: deps.conversationId,
		persist: true,
		key: newKey
	});

	void deps.ops.run(op);
}

export interface ReturnDraftDeps {
	drafts?: DraftStore;
	conversationId?: string;
	chat?: ChatStore;
	onReturn?: (text: string) => void;
}

/**
 * Возврат отклонённого сообщения в черновик.
 */
export function returnToDraft(
	input: InputView,
	deps?: ReturnDraftDeps
): void {
	if (deps?.onReturn) {
		deps.onReturn(input.text);
	}
	if (deps?.drafts && deps.conversationId) {
		deps.drafts.set(deps.conversationId, {
			text: input.text,
			attachments: input.attachments.map((a) => ({ id: a.attachment_id, name: a.name }))
		});
	}
	if (deps?.chat) {
		deps.chat.inputs.delete(input.key);
	}
}
