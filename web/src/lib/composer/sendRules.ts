export interface KeyCtx {
	key: string;
	shiftKey: boolean;
	ctrlKey: boolean;
	metaKey: boolean;
	altKey: boolean;
	isComposing: boolean;
	keyCode: number;
	pointer: 'touch' | 'mouse' | 'unknown';
	pref: 'auto' | 'send' | 'newline';
}

/**
 * Решение о действии при нажатии клавиши в поле ввода.
 * На сенсорном вводе Enter никогда не отправляет сообщение (Review Focus 2).
 * IME (isComposing или keyCode 229) всегда игнорируется.
 */
export function decideEnter(c: KeyCtx): 'newline' | 'send' | 'send_later_menu' | 'ignore' {
	// IME-композиция или keyCode 229 (Android/Samsung клавиатуры): ввод принадлежит IME
	if (c.isComposing || c.keyCode === 229) {
		return 'ignore';
	}

	// Только клавиша Enter
	const isEnter = c.key === 'Enter' || c.keyCode === 13;
	if (!isEnter) {
		return 'ignore';
	}

	// Сочетание клавиш для меню «Отправить позже»: Ctrl/Cmd + Shift + Enter
	if ((c.ctrlKey || c.metaKey) && c.shiftKey) {
		return 'send_later_menu';
	}

	// Настройка «Перенос строки»: Enter всегда переносит строку
	if (c.pref === 'newline') {
		return 'newline';
	}

	// Настройка «Отправка»: Shift+Enter — перенос, Enter — отправка
	if (c.pref === 'send') {
		return c.shiftKey ? 'newline' : 'send';
	}

	// Настройка «Авто»:
	// Сенсорный ввод — только перенос строки, чтобы избежать случайной отправки
	if (c.pointer === 'touch') {
		return 'newline';
	}

	// Мышь / физическая клавиатура: Shift+Enter — перенос, Enter — отправка
	return c.shiftKey ? 'newline' : 'send';
}

let lastPointerType: 'touch' | 'mouse' | 'unknown' = 'unknown';

if (typeof window !== 'undefined') {
	window.addEventListener(
		'pointerdown',
		(e) => {
			if (e.pointerType === 'touch') {
				lastPointerType = 'touch';
			} else if (e.pointerType === 'mouse' || e.pointerType === 'pen') {
				lastPointerType = 'mouse';
			}
		},
		{ passive: true, capture: true }
	);
}

export function isTouchPrimary(): boolean {
	if (lastPointerType === 'touch') return true;
	if (typeof window !== 'undefined' && typeof window.matchMedia === 'function') {
		if (window.matchMedia('(pointer: coarse)').matches) {
			return true;
		}
	}
	return false;
}

/** Вспомогательная функция для тестов */
export function setPointerTypeForTest(type: 'touch' | 'mouse' | 'unknown'): void {
	lastPointerType = type;
}

const MODEL_PHASES = new Set(['generating', 'running_tool', 'context_maintenance']);

/** Идёт ход: занято место в очереди или модель работает в ходе, который CLI начал сам и без места. */
export function turnIsLive(activity: unknown, runSlot: unknown): boolean {
	if (typeof activity !== 'object' || activity === null) return false;
	const { state, evidence } = activity as { state?: unknown; evidence?: { phase?: unknown } };
	if (state !== 'working' && state !== 'stalled') return false;
	if (typeof runSlot === 'object' && runSlot !== null && 'running' in runSlot) return true;
	return MODEL_PHASES.has(String(evidence?.phase ?? ''));
}
