import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import type { ChatStore } from '#lib/stores/chat.svelte.js';
import { FakeComposerModel } from './support/fakeComposerModel.svelte.js';

let model: FakeComposerModel;

vi.mock('#lib/app/runtime.js', () => ({
	getRuntime: () => ({
		journal: {},
		drafts: {},
		deviceId: 'device-1',
		connection: { status: { phase: 'live' } },
		machines: { items: new Map() }
	})
}));
vi.mock('#lib/screens/chat/open.js', () => ({
	sessionUploads: () => ({ list: () => [], retry: vi.fn(), cancel: vi.fn() })
}));
vi.mock('#lib/api/config.js', () => ({ loadConfig: () => new Promise(() => undefined) }));
vi.mock('#lib/dictation/status.svelte.js', () => ({
	dictationStatus: { available: false, missing: false, refresh: () => Promise.resolve(), hideForMissingMicrophone: vi.fn(), noteUnavailable: vi.fn() }
}));
vi.mock('#lib/composer/model.svelte.js', async (original) => ({
	...(await original<typeof import('#lib/composer/model.svelte.js')>()),
	ComposerModel: class extends FakeComposerModel {
		constructor() {
			super();
			// eslint-disable-next-line @typescript-eslint/no-this-alias
			model = this;
		}
	}
}));

const { default: Composer } = await import('#lib/composer/Composer.svelte');

const chat = {
	id: 'conv-1',
	detail: { harness: { id: 'codex', title: 'Codex', provider: 'codex' }, machine: { id: 'm1', name: 'workstation' } },
	activity: { state: 'idle', evidence: {} },
	queue: { pause_reasons: [], hold_reasons: [], run_slot: 'free', queued: 0 },
	promptSuggestion: null,
	context: null,
	draft: null,
	draftSerial: 0
} as unknown as ChatStore;

describe('composer/side command', () => {
	let instance: ReturnType<typeof mount> | undefined;

	beforeEach(() => {
		vi.stubGlobal(
			'ResizeObserver',
			class {
				observe(): void {}
				disconnect(): void {}
			}
		);
	});

	afterEach(() => {
		if (instance) void unmount(instance);
		instance = undefined;
		document.body.textContent = '';
		vi.unstubAllGlobals();
	});

	function enter(onside: (question: string) => boolean): void {
		const target = document.createElement('div');
		document.body.appendChild(target);
		instance = mount(Composer, { target, props: { chat, onside } });
		flushSync();
		const field = target.querySelector<HTMLTextAreaElement>('textarea')!;
		field.value = '/side что делает этот тест?';
		field.dispatchEvent(new Event('input', { bubbles: true }));
		flushSync();
		model.typedTexts.length = 0;
		field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
		flushSync();
	}

	it('hands the question to the side chat and clears the shared draft', () => {
		const onside = vi.fn(() => true);

		enter(onside);

		expect(onside).toHaveBeenCalledWith('что делает этот тест?');
		expect(model.text).toBe('');
		// Без этого сохранённый черновик «/side …» вернулся бы в поле при следующем открытии чата.
		expect(model.typedTexts).toEqual(['']);
		expect(model.calls).toEqual([]);
	});

	it('keeps the text and says so when the side chat is not available', () => {
		enter(() => false);

		expect(model.text).toBe('/side что делает этот тест?');
		expect(model.errorMessage).toBe('Боковой чат здесь недоступен');
		expect(document.querySelector('textarea')!.value).toBe('/side что делает этот тест?');
		// В родительскую сессию команда не уходит.
		expect(model.calls).toEqual([]);
	});
});
