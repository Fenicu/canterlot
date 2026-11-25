import { laneRoom, type LayoutMode } from './layout';

export type PaneView = 'delegation' | 'settings' | 'messages' | 'alarms' | 'search';
/** Вкладка панели делегирования: дерево делегатов или план оркестратора. */
export type DelegationTab = 'tree' | 'plan';

/**
 * Память устройства о закрытой панели плана и делегатов. Общая для всех сессий:
 * владелец хочет видеть панель по умолчанию, а закрытие значит «не сейчас на этом экране».
 */
export const LANE_CLOSED_KEY = 'canterlot.lane';

export interface LaneAutoInput {
	mode: LayoutMode;
	width: number;
	/** У открытой сессии есть план или делегаты: шапка показывает «Делегирование». */
	delegated: boolean;
	/** Панель закрыли на этом устройстве. */
	closed: boolean;
}

/** Панель плана и делегатов открывается сама на столе от порога ширины, если есть что показать и её не закрывали. */
export function laneAutoOpen({ mode, width, delegated, closed }: LaneAutoInput): boolean {
	return mode === 'desktop' && laneRoom(width) && delegated && !closed;
}

function deviceStorage(): Storage | null {
	try {
		return typeof localStorage === 'undefined' ? null : localStorage;
	} catch {
		return null;
	}
}

/** Третья панель стола: шапка чата её открывает, оболочка рисует. */
export class Pane {
	/** Панель, открытая явно. Пока её нет, на широком окне стоит панель плана и делегатов. */
	view = $state<PaneView | null>(null);
	mode = $state<LayoutMode>('phone');
	/** Ширина окна в CSS-пикселях: от неё зависит, открывается ли панель сама. */
	width = $state(0);
	/** У открытого чата есть план или делегаты; пишет шапка чата. */
	delegated = $state(false);
	closed = $state(false);
	/** Общая для листа телефона и панели стола: «План» открывает только `openPlan`. */
	delegationTab = $state<DelegationTab>('tree');
	#chat: string | null = null;
	#storage: () => Storage | null;

	constructor(storage: () => Storage | null = deviceStorage) {
		this.#storage = storage;
		try {
			this.closed = storage()?.getItem(LANE_CLOSED_KEY) === 'closed';
		} catch {
			this.closed = false;
		}
	}

	get desktop(): boolean {
		return this.mode === 'desktop';
	}

	/** Стол от порога ширины: панель делегирования показывает план и дерево стопкой. */
	get stacked(): boolean {
		return this.desktop && laneRoom(this.width);
	}

	get auto(): boolean {
		return laneAutoOpen({ mode: this.mode, width: this.width, delegated: this.delegated, closed: this.closed });
	}

	/** Панель, которую рисует оболочка: явно открытая или открытая сама. */
	get shown(): PaneView | null {
		return this.view ?? (this.auto ? 'delegation' : null);
	}

	/** План стоит в панели: полоса плана уходит из колонки чата. */
	get planDocked(): boolean {
		return this.stacked && this.shown === 'delegation';
	}

	toggle(view: PaneView): void {
		this.view = this.view === view ? null : view;
	}

	open(view: PaneView): void {
		if (view === 'delegation') {
			this.#remember(false);
			// Панель, которая и так открывается сама, остаётся на правиле: в чате без делегатов она уйдёт.
			if (this.auto) {
				this.view = null;
				return;
			}
		}
		this.view = view;
	}

	close(): void {
		this.view = null;
	}

	/** Крестик или кнопка шапки: на широком окне закрытие запоминается на этом устройстве. */
	closeDelegation(): void {
		if (this.stacked) this.#remember(true);
		this.view = null;
	}

	/** Открыт другой чат: вкладка прошлого чата не переносится, панель начинает с дерева. */
	forChat(id: string): void {
		if (this.#chat === id) return;
		this.#chat = id;
		this.delegationTab = 'tree';
	}

	/** Полоса или карточка плана: панель делегирования на вкладке «План». */
	openPlan(): void {
		this.delegationTab = 'plan';
	}

	/** Кнопка «Делегирование» на телефоне: лист открывается на дереве делегатов. */
	showDelegationTree(): void {
		this.delegationTab = 'tree';
	}

	/** Кнопка «Делегирование» на столе: открытие начинается с дерева делегатов и стирает память о закрытии. */
	toggleDelegation(): void {
		if (this.shown === 'delegation') {
			this.closeDelegation();
			return;
		}
		this.delegationTab = 'tree';
		this.open('delegation');
	}

	#remember(closed: boolean): void {
		this.closed = closed;
		try {
			const storage = this.#storage();
			if (closed) storage?.setItem(LANE_CLOSED_KEY, 'closed');
			else storage?.removeItem(LANE_CLOSED_KEY);
		} catch {
			// Выбор действует до перезагрузки.
		}
	}
}

export const pane = new Pane();
