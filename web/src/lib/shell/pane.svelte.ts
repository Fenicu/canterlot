import type { LayoutMode } from './layout';

export type PaneView = 'delegation' | 'settings' | 'messages' | 'alarms' | 'search';
/** Вкладка панели делегирования: дерево делегатов или план оркестратора. */
export type DelegationTab = 'tree' | 'plan';

/** Третья панель стола: шапка чата её открывает, оболочка рисует. */
class Pane {
	view = $state<PaneView | null>(null);
	mode = $state<LayoutMode>('phone');
	/** Общая для листа телефона и панели стола: «План» открывает только `openPlan`. */
	delegationTab = $state<DelegationTab>('tree');
	#chat: string | null = null;

	get desktop(): boolean {
		return this.mode === 'desktop';
	}

	toggle(view: PaneView): void {
		this.view = this.view === view ? null : view;
	}

	open(view: PaneView): void {
		this.view = view;
	}

	close(): void {
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

	/** Кнопка «Делегирование» на столе: открытие начинается с дерева делегатов. */
	toggleDelegation(): void {
		if (this.view !== 'delegation') this.delegationTab = 'tree';
		this.toggle('delegation');
	}
}

export const pane = new Pane();
