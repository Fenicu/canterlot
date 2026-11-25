export type LayoutMode = 'phone' | 'tablet' | 'desktop';

/**
 * Медиазапросы меряют rem от исходного шрифта браузера, а не от корня, который растит `--ui-scale`.
 * Поэтому все пороги ниже — CSS-пиксели окна, и JS сравнивает с ними `clientWidth` без поправки на масштаб.
 */
const MEDIA_REM_PX = 16;

/** Телефон уже 44rem, планшет до 74rem, дальше стол. */
export function layoutMode(widthPx: number): LayoutMode {
	if (widthPx < 44 * MEDIA_REM_PX) return 'phone';
	if (widthPx < 74 * MEDIA_REM_PX) return 'tablet';
	return 'desktop';
}

/**
 * Панель браузера отнимает у окна высоту при любом повороте монитора. Масштаб считается по экрану:
 * окну с этой добавкой к высоте. Так боком и стоя один монитор получает один шаг.
 */
export const BROWSER_CHROME_PX = 6 * MEDIA_REM_PX;

/**
 * Шаги `--ui-scale` (медиазапросы в конце tokens.css): длинная и короткая сторона экрана.
 * Пороги ниже номинальных 1920, 2560 и 3840: при системном масштабе окно бывает на доли пикселя уже.
 */
export const SCALE_STEPS = [
	{ long: 118 * MEDIA_REM_PX, short: 62 * MEDIA_REM_PX, scale: 1.0667 },
	{ long: 156 * MEDIA_REM_PX, short: 76 * MEDIA_REM_PX, scale: 1.2 },
	{ long: 230 * MEDIA_REM_PX, short: 116 * MEDIA_REM_PX, scale: 1.6 }
] as const;

/** Множитель интерфейса для окна в CSS-пикселях: тот же, что выбирают медиазапросы tokens.css. */
export function uiScale(widthPx: number, heightPx: number): number {
	const screenHeight = heightPx + BROWSER_CHROME_PX;
	const long = Math.max(widthPx, screenHeight);
	const short = Math.min(widthPx, screenHeight);
	let scale = 1;
	for (const step of SCALE_STEPS) if (long >= step.long && short >= step.short) scale = step.scale;
	return scale;
}

/**
 * С этой ширины окна (CSS-пиксели, масштаб интерфейса уже учтён) панель плана и делегатов
 * открывается сама: при ней колонке чата остаётся не меньше, чем без панели на 1440 px.
 */
export const LANE_ROOM_PX = 1600;

export function laneRoom(widthPx: number): boolean {
	return widthPx >= LANE_ROOM_PX;
}
