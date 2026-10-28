import { describe, it, expect, vi } from 'vitest';
import { SvelteMap } from 'svelte/reactivity';
import { cancelInput, retryAsNew, returnToDraft } from '$lib/timeline/QueueActions';
import { freshInput, type InputView } from '$lib/stores/inputs';
import type { OpJournal, Op } from '$lib/ops/journal';
import type { ChatStore } from '$lib/stores/chat.svelte';
import type { DraftStore } from '$lib/drafts/store';

describe('timeline/QueueActions', () => {
	it('cancel_outcomes_requested_and_lost_follow_delivery_d3', async () => {
		const input: InputView = freshInput('k1', 'Текст для отмены', Date.now());
		input.id = 'inp-uuid-1';
		input.state = 'queued';

		// 1. Hub returns "cancelled" -> outcome 'cancelled', state becomes 'cancelled'
		const resCancelled = await cancelInput(input, {
			clientCancel: async () => ({ data: 'cancelled' })
		});
		expect(resCancelled).toBe('cancelled');
		expect(input.state).toBe('cancelled');
		expect(input.placement).toBe('none');

		// 2. Hub returns { cancel_requested: { cancel_command_id: 'cmd-1' } } -> outcome 'requested', flag set
		const input2: InputView = freshInput('k2', 'Второй ввод', Date.now());
		input2.id = 'inp-uuid-2';
		input2.state = 'on_machine';
		const resRequested = await cancelInput(input2, {
			clientCancel: async () => ({ data: { cancel_requested: { cancel_command_id: 'cmd-1' } } })
		});
		expect(resRequested).toBe('requested');
		expect(input2.cancelRequested).toBe(true);

		// 3. Hub returns { too_late: { state: 'transmitting' } } -> outcome 'lost'
		const input3: InputView = freshInput('k3', 'Третий ввод', Date.now());
		input3.id = 'inp-uuid-3';
		input3.state = 'transmitting';
		const resLost = await cancelInput(input3, {
			clientCancel: async () => ({ data: { too_late: { state: 'transmitting' } } })
		});
		expect(resLost).toBe('lost');
	});

	it('retry_as_new_uses_a_new_key_and_sends_retry_of_after_confirm', async () => {
		const unconfirmedInput: InputView = freshInput('orig-key', 'Неподтверждённый текст', Date.now());
		unconfirmedInput.id = 'inp-unconfirmed-id';
		unconfirmedInput.state = 'acceptance_unconfirmed';

		interface SubmitPayload {
			content?: Array<{ text?: string }>;
			retry_of?: string;
			attachment_ids?: string[];
		}

		const captured = {
			body: null as SubmitPayload | null,
			key: undefined as string | undefined
		};

		const opsMock = {
			begin: vi.fn(async (kind: string, body: unknown, opts: unknown) => {
				captured.body = body as SubmitPayload;
				captured.key = (opts as { key?: string })?.key;
				return { key: captured.key ?? crypto.randomUUID() } as Op;
			}),
			run: vi.fn(async () => undefined)
		} as unknown as OpJournal;

		const chatMock = {
			inputs: new SvelteMap<string, InputView>()
		};

		// 1. User cancels confirm -> does not send
		const confirmDeclined = vi.fn(async () => false);
		await retryAsNew(unconfirmedInput, {
			ops: opsMock,
			chat: chatMock as unknown as ChatStore,
			conversationId: 'conv-1',
			confirm: confirmDeclined
		});

		expect(confirmDeclined).toHaveBeenCalled();
		expect(opsMock.begin).not.toHaveBeenCalled();

		// 2. User confirms -> sends with new key and retry_of
		const confirmAccepted = vi.fn(async () => true);
		await retryAsNew(unconfirmedInput, {
			ops: opsMock,
			chat: chatMock as unknown as ChatStore,
			conversationId: 'conv-1',
			confirm: confirmAccepted
		});

		expect(confirmAccepted).toHaveBeenCalled();
		expect(opsMock.begin).toHaveBeenCalled();
		expect(captured.key).toBeDefined();
		expect(captured.key).not.toBe('orig-key'); // New idempotency key!
		expect(captured.body?.retry_of).toBe('inp-unconfirmed-id'); // Points to unconfirmed input
		expect(captured.body?.content?.[0]?.text).toBe('Неподтверждённый текст');

		// Local item placed in chat.inputs
		expect(chatMock.inputs.has(captured.key!)).toBe(true);
		expect(chatMock.inputs.get(captured.key!)?.retryOf).toBe('inp-unconfirmed-id');
	});

	it('return_to_draft_restores_text_to_draft_and_cleans_input', () => {
		const rejectedInput: InputView = freshInput('rej-key', 'Отклонённый текст', Date.now());
		const chatMock = {
			inputs: new SvelteMap<string, InputView>()
		};
		chatMock.inputs.set('rej-key', rejectedInput);

		let returnedDraftText = '';
		const draftsMock = {
			set: vi.fn((convId: string, patch: { text?: string }) => {
				returnedDraftText = patch.text ?? '';
			})
		} as unknown as DraftStore;

		returnToDraft(rejectedInput, {
			drafts: draftsMock,
			conversationId: 'conv-1',
			chat: chatMock as unknown as ChatStore
		});

		expect(returnedDraftText).toBe('Отклонённый текст');
		expect(chatMock.inputs.has('rej-key')).toBe(false);
	});
});
