export type LayoutMode = 'phone' | 'tablet' | 'desktop';

/**
 * Медиазапросы меряют rem от исходного шрифта браузера, а не от корня, который растит `--ui-scale`.
 * Поэтому все пороги ниже — CSS-пиксели окна, и JS сравнивает с ними `clientWidth` без поправки на масштаб.
 */
const MEDIA_REM_PX = 16;

/** Окно в CSS-пикселях и главный указатель. */
export interface Viewport {
	width: number;
	height: number;
	/** Мышь или тачпад: `(hover: hover) and (pointer: fine)`. */
	mouse: boolean;
}

/**
 * Монитор стоя с мышью — стол: полоса разделов, список и чат, а план и делегаты внизу колонки чата.
 * Ширина: колонке ленты нужно не меньше 600 px. Рядом с ней стоят поля и промежуток панелей (36 px),
 * полоса разделов (78 px), список стоя (18rem, 288 px) и поля ленты (34 px), всего 436 px при масштабе 1.
 * Стоя от 1792 px высоты масштаб уже 1.0667, и 600 + 436 × 1.0667 даёт 1066 px. Уже этого окна
 * (Full HD стоя при 125%, 2K стоя при 150%) остаётся планшет: список и чат, лента не сжимается.
 */
export const PORTRAIT_DESK_MIN_WIDTH = 1066;
/**
 * Высота окна стоя: нижняя панель берёт треть колонки чата, и лента над ней остаётся
 * не ниже, чем на столе боком 1920×1080. Ниже порога окно раскладывается по ширине, как раньше.
 */
export const PORTRAIT_DESK_MIN_HEIGHT = 1440;

/** Стол стоя: план и делегаты в нижней панели колонки чата вместо правой панели. */
export function dockForm({ width, height, mouse }: Viewport): boolean {
	return mouse && height > width && width >= PORTRAIT_DESK_MIN_WIDTH && height >= PORTRAIT_DESK_MIN_HEIGHT;
}

/**
 * Телефон уже 44rem, планшет до 74rem, дальше стол. Монитор стоя с мышью — стол и уже 74rem
 * (см. `dockForm`); сенсорный планшет стоя и боком раскладывается по ширине, как раньше.
 */
export function layoutMode(viewport: Viewport): LayoutMode {
	if (dockForm(viewport)) return 'desktop';
	if (viewport.width < 44 * MEDIA_REM_PX) return 'phone';
	if (viewport.width < 74 * MEDIA_REM_PX) return 'tablet';
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
