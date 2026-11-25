export type LayoutMode = 'phone' | 'tablet' | 'desktop';

/** Телефон уже 44rem, планшет до 74rem, дальше стол. */
export function layoutMode(widthPx: number, rootPx = 16): LayoutMode {
	if (widthPx < 44 * rootPx) return 'phone';
	if (widthPx < 74 * rootPx) return 'tablet';
	return 'desktop';
}

/**
 * С этой ширины окна (CSS-пиксели, масштаб интерфейса уже учтён) панель плана и делегатов
 * открывается сама: при ней колонке чата остаётся не меньше, чем без панели на 1440 px.
 */
export const LANE_ROOM_PX = 1600;

export function laneRoom(widthPx: number): boolean {
	return widthPx >= LANE_ROOM_PX;
}
