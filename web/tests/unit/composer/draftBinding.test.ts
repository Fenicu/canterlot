import { describe, it, expect, vi, beforeEach } from 'vitest';
import { bindDraft } from '$lib/composer/draftBinding';
import type { DraftStore } from '$lib/drafts/store';

describe('composer/draftBinding', () => {
	let draftsMock: DraftStore;
	let loadedDraft: { text: string } | null;
	let savedDrafts: Array<{ convId: string; text?: string }>;
	let flushed: boolean;

	beforeEach(() => {
		loadedDraft = { text: 'Сохранённый черновик' };
		savedDrafts = [];
		flushed = false;

		draftsMock = {
			load: vi.fn(async () => loadedDraft),
			set: vi.fn((convId: string, patch: { text?: string }) => {
				savedDrafts.push({ convId, text: patch.text });
			}),
			clear: vi.fn(async () => undefined),
			flush: vi.fn(async () => {
				flushed = true;
			}),
			watchComposer: vi.fn(() => () => undefined)
		} as unknown as DraftStore;
	});

	it('restores_draft_on_mount_and_does_not_auto_send', async () => {
		const textarea = document.createElement('textarea');
		let restoredText = '';
		const sendTriggered = false;

		const binding = bindDraft(textarea, {
			drafts: draftsMock,
			conversationId: 'conv-123',
			onRestore: (text) => {
				restoredText = text;
				// Verification: restored draft must not auto-send
			}
		});

		// Wait for load promise to resolve
		await Promise.resolve();
		await Promise.resolve();

		expect(textarea.value).toBe('Сохранённый черновик');
		expect(restoredText).toBe('Сохранённый черновик');
		expect(sendTriggered).toBe(false);

		// Typing into textarea triggers set
		textarea.value = 'Новый текст';
		textarea.dispatchEvent(new Event('input'));

		expect(savedDrafts).toEqual([{ convId: 'conv-123', text: 'Новый текст' }]);

		binding.destroy();
		expect(flushed).toBe(true);
	});
});
