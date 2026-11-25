import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, tick, unmount } from 'svelte';
import type { ChatStore } from '#lib/stores/chat.svelte.js';
import { FakeComposerModel } from './support/fakeComposerModel.svelte.js';

let uploadItems: { phase: string }[] = [];

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
	sessionUploads: () => ({ list: () => uploadItems, retry: vi.fn(), cancel: vi.fn() })
}));
vi.mock('#lib/api/config.js', () => ({ loadConfig: () => new Promise(() => undefined) }));
vi.mock('#lib/dictation/status.svelte.js', () => ({
	dictationStatus: { available: true, missing: false, refresh: () => Promise.resolve(), hideForMissingMicrophone: vi.fn(), noteUnavailable: vi.fn() }
}));
vi.mock('#lib/composer/model.svelte.js', async (original) => ({
	...(await original<typeof import('#lib/composer/model.svelte.js')>()),
	ComposerModel: FakeComposerModel
}));

const { default: Composer } = await import('#lib/composer/Composer.svelte');

function chatWith(patch: Record<string, unknown> = {}): ChatStore {
	return {
		id: 'conv-1',
		detail: { harness: { id: 'codex', title: 'Codex', provider: 'codex' }, machine: { id: 'm1', name: 'workstation' } },
		activity: { state: 'idle', evidence: {} },
		queue: { pause_reasons: [], hold_reasons: [], run_slot: 'free', queued: 0 },
		promptSuggestion: null,
		context: { tokens: 9_600, window: 200_000, observed_at: '2026-11-25T10:00:00.000Z' },
		draft: null,
		draftSerial: 0,
		...patch
	} as unknown as ChatStore;
}

const liveTurn = {
	activity: { state: 'working', evidence: { phase: 'running_tool' } },
	queue: { pause_reasons: [], hold_reasons: [], run_slot: { running: { input_id: 'in-1' } }, queued: 0 }
};

describe('composer/idle row', () => {
	let instance: ReturnType<typeof mount> | undefined;

	beforeEach(() => {
		uploadItems = [];
		vi.stubGlobal(
			'ResizeObserver',
			class {
				observe(): void {}
				disconnect(): void {}
			}
		);
		Object.defineProperty(navigator, 'mediaDevices', { configurable: true, value: { getUserMedia: vi.fn() } });
	});

	afterEach(() => {
		if (instance) void unmount(instance);
		instance = undefined;
		document.body.textContent = '';
		vi.unstubAllGlobals();
	});

	function show(chat = chatWith()): HTMLElement {
		const target = document.createElement('div');
		document.body.appendChild(target);
		instance = mount(Composer, { target, props: { chat } });
		flushSync();
		return target;
	}

	const capsule = (target: HTMLElement) => target.querySelector<HTMLElement>('.capsule')!;
	const field = (target: HTMLElement) => target.querySelector<HTMLTextAreaElement>('textarea')!;
	const button = (target: HTMLElement, label: string) => target.querySelector(`[aria-label="${label}"]`);

	function type(target: HTMLElement, text: string): void {
		const node = field(target);
		node.value = text;
		node.dispatchEvent(new Event('input', { bubbles: true }));
		flushSync();
	}

	it('idle_field_is_one_row_with_attach_context_mic_and_send_only', () => {
		const target = show(chatWith(liveTurn));
		expect(capsule(target).hasAttribute('data-idle')).toBe(true);
		expect(button(target, 'Прикрепить файл')).not.toBeNull();
		expect(button(target, 'Диктовка')).not.toBeNull();
		expect(button(target, 'В очередь')).not.toBeNull();
		expect(target.querySelector('.ctx')).not.toBeNull();
		// Заготовки, «Влить в ход», «Отправить позже» и подсказка клавиш ждут раскрытого поля.
		expect(button(target, 'Заготовки')).toBeNull();
		expect(button(target, 'Влить в ход')).toBeNull();
		expect(button(target, 'Отправить позже')).toBeNull();
		expect(target.querySelector('.hint')).toBeNull();
	});

	it('focus_opens_the_full_toolbar_on_the_same_field_and_blur_folds_it', async () => {
		const target = show();
		const node = field(target);
		node.focus();
		flushSync();
		expect(capsule(target).hasAttribute('data-idle')).toBe(false);
		expect(button(target, 'Заготовки')).not.toBeNull();
		expect(button(target, 'Отправить позже')).not.toBeNull();
		// Раскладка меняется, а поле то же самое и в фокусе.
		expect(field(target)).toBe(node);
		expect(document.activeElement).toBe(node);

		// Фокус ушёл на кнопку того же поля: строка не сворачивается.
		(button(target, 'Заготовки') as HTMLElement).focus();
		flushSync();
		expect(capsule(target).hasAttribute('data-idle')).toBe(false);

		(document.activeElement as HTMLElement).blur();
		await tick();
		flushSync();
		expect(capsule(target).hasAttribute('data-idle')).toBe(true);
	});

	it('text_keeps_the_full_toolbar_without_focus', () => {
		const target = show();
		type(target, 'Проверь манифест');
		expect(capsule(target).hasAttribute('data-idle')).toBe(false);
		expect(button(target, 'Заготовки')).not.toBeNull();
	});

	it('steer_is_offered_with_text_during_a_live_turn', () => {
		const target = show(chatWith(liveTurn));
		expect(button(target, 'Влить в ход')).toBeNull();
		type(target, 'Добавь ещё один случай');
		const steer = button(target, 'Влить в ход') as HTMLButtonElement;
		expect(steer).not.toBeNull();
		steer.click();
		flushSync();
		expect(capsule(target).hasAttribute('data-idle')).toBe(false);
	});

	it('attachments_keep_the_full_layout', () => {
		uploadItems = [{ id: 'u1', name: 'снимок.png', size: 2048, sent: 2048, phase: 'ready', attachmentId: 'a1' } as { phase: string }];
		const target = show();
		expect(capsule(target).hasAttribute('data-idle')).toBe(false);
	});
});
