import { fmt } from '#lib/format/ru.js';

const HOVER_DELAY = 300;
const TOUCH_HIDE = 3000;
const HOVER_MEDIA = '(hover: hover) and (pointer: fine)';
// Фокус от нажатия приходит сразу после pointerdown: в этот миг подсказку показывает само нажатие.
const POINTER_GRACE = 600;

type Source = 'hover' | 'focus' | 'touch';

// Открыта не больше одной подсказки: новая закрывает прежнюю.
let activeClose: (() => void) | null = null;
let sequence = 0;

function finePointer(): boolean {
	return window.matchMedia(HOVER_MEDIA).matches;
}

/**
 * Подсказка с полной датой и секундами над отметкой времени.
 * Живёт в конце документа или открытого диалога, поэтому лента её не обрезает.
 */
export function timeTip(node: HTMLElement, at: number) {
	let ts = at;
	let open = false;
	let source: Source | null = null;
	let tip: HTMLDivElement | null = null;
	let hoverTimer = 0;
	let hideTimer = 0;
	let pointerAt = Number.NEGATIVE_INFINITY;
	let pointerKind = '';
	// Внутри ссылки или кнопки отметка времени не получает свой шаг табуляции: подсказку показывает фокус строки.
	const focusTarget: HTMLElement = node.closest<HTMLElement>('a[href], button') ?? node;
	if (focusTarget === node) node.tabIndex = 0;

	function element(): HTMLDivElement {
		if (!tip) {
			tip = document.createElement('div');
			tip.id = `time-tip-${++sequence}`;
			tip.className = 'time-tip';
			tip.setAttribute('role', 'tooltip');
		}
		return tip;
	}

	/** Верх видимой части ближайшей прокручиваемой области: над ней подсказка уже упирается в край. */
	function boundary(): DOMRect {
		let parent = node.parentElement;
		while (parent && parent !== document.body && parent !== document.documentElement) {
			const style = getComputedStyle(parent);
			if (/(auto|scroll)/.test(style.overflowY) && parent.scrollHeight > parent.clientHeight) {
				return parent.getBoundingClientRect();
			}
			parent = parent.parentElement;
		}
		return new DOMRect(0, 0, window.innerWidth, window.innerHeight);
	}

	function place(el: HTMLDivElement): void {
		const rect = node.getBoundingClientRect();
		const box = boundary();
		const root = getComputedStyle(document.documentElement);
		const gap = Number.parseFloat(root.getPropertyValue('--s2')) || 4;
		const margin = Number.parseFloat(root.getPropertyValue('--s4')) || 8;
		const width = el.offsetWidth;
		const height = el.offsetHeight;
		const above = rect.top - gap - height >= box.top + margin;
		const top = above ? rect.top - gap - height : rect.bottom + gap;
		const left = Math.min(
			Math.max(rect.left + rect.width / 2 - width / 2, margin),
			Math.max(margin, window.innerWidth - margin - width)
		);
		el.dataset.placement = above ? 'above' : 'below';
		el.style.left = `${left}px`;
		el.style.top = `${Math.max(box.top + margin, Math.min(top, window.innerHeight - margin - height))}px`;
	}

	function armAutoHide(): void {
		if (hideTimer) window.clearTimeout(hideTimer);
		hideTimer = 0;
		if (source === 'touch') hideTimer = window.setTimeout(close, TOUCH_HIDE);
	}

	function show(next: Source): void {
		if (open) {
			source = next;
			armAutoHide();
			return;
		}
		activeClose?.();
		activeClose = close;
		source = next;
		const el = element();
		el.textContent = fmt.fullStamp(ts);
		el.removeAttribute('hidden');
		(node.closest('dialog') ?? document.body).append(el);
		place(el);
		open = true;
		focusTarget.setAttribute('aria-describedby', el.id);
		window.addEventListener('keydown', onKey, true);
		window.addEventListener('scroll', close, { capture: true, passive: true });
		window.addEventListener('resize', close);
		if (next === 'touch') {
			window.addEventListener('pointerdown', onOutside, true);
			armAutoHide();
		}
	}

	function close(): void {
		if (!open) return;
		open = false;
		source = null;
		if (hideTimer) window.clearTimeout(hideTimer);
		hideTimer = 0;
		tip?.remove();
		focusTarget.removeAttribute('aria-describedby');
		window.removeEventListener('keydown', onKey, true);
		window.removeEventListener('scroll', close, true);
		window.removeEventListener('resize', close);
		window.removeEventListener('pointerdown', onOutside, true);
		if (activeClose === close) activeClose = null;
	}

	function onKey(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || !open) return;
		event.preventDefault();
		event.stopPropagation();
		close();
	}

	function onOutside(event: Event): void {
		const target = event.target;
		if (target instanceof Node && node.contains(target)) return;
		close();
	}

	function onEnter(): void {
		if (!finePointer()) return;
		window.clearTimeout(hoverTimer);
		hoverTimer = window.setTimeout(() => {
			hoverTimer = 0;
			show('hover');
		}, HOVER_DELAY);
	}

	function onLeave(): void {
		window.clearTimeout(hoverTimer);
		hoverTimer = 0;
		if (open && source === 'hover') close();
	}

	function onFocus(): void {
		if (performance.now() - pointerAt < POINTER_GRACE) return;
		show('focus');
	}

	function onBlur(): void {
		if (open && source === 'focus') close();
	}

	function onPointerDown(event: PointerEvent): void {
		pointerAt = performance.now();
		pointerKind = event.pointerType;
	}

	function onClick(event: MouseEvent): void {
		// Мышь и клавиатура уже показывают дату наведением или фокусом: нажатие её не переключает.
		if (pointerKind !== 'touch' && pointerKind !== 'pen') return;
		event.preventDefault();
		event.stopPropagation();
		if (open) close();
		else show('touch');
	}

	function update(next: number): void {
		ts = next;
		if (open && tip) {
			tip.textContent = fmt.fullStamp(ts);
			place(tip);
		}
	}

	function destroy(): void {
		window.clearTimeout(hoverTimer);
		close();
		node.removeEventListener('pointerenter', onEnter);
		node.removeEventListener('pointerleave', onLeave);
		node.removeEventListener('click', onClick);
		focusTarget.removeEventListener('pointerdown', onPointerDown);
		focusTarget.removeEventListener('focus', onFocus);
		focusTarget.removeEventListener('blur', onBlur);
		tip?.remove();
		tip = null;
	}

	node.addEventListener('pointerenter', onEnter);
	node.addEventListener('pointerleave', onLeave);
	node.addEventListener('click', onClick);
	focusTarget.addEventListener('pointerdown', onPointerDown);
	focusTarget.addEventListener('focus', onFocus);
	focusTarget.addEventListener('blur', onBlur);

	return { update, destroy };
}
