import type { LayoutMode } from './layout';

export type PaneView = 'delegation' | 'settings' | 'messages' | 'alarms';

/** Третья панель стола: шапка чата её открывает, оболочка рисует. */
class Pane {
	view = $state<PaneView | null>(null);
	mode = $state<LayoutMode>('phone');

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
}

export const pane = new Pane();
