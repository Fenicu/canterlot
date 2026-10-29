export type LayoutMode = 'phone' | 'tablet' | 'desktop';

/** Телефон уже 44rem, планшет до 74rem, дальше стол. */
export function layoutMode(widthPx: number, rootPx = 16): LayoutMode {
	if (widthPx < 44 * rootPx) return 'phone';
	if (widthPx < 74 * rootPx) return 'tablet';
	return 'desktop';
}
