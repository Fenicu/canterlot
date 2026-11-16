import type { Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { FakeHub, FakeHubApi, seedConversation, seedMachine, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { ROUTE_DECISION } from '../../testing/fixtures/routing';
import { PALETTES } from '../../src/lib/palettes';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const SHOTS = process.env.CHAT_HEADER_SHOTS;

const CATALOG = {
	id: 'workstation',
	name: 'workstation',
	harnesses: [
		{
			id: 'claude',
			title: 'Claude Code',
			provider: 'claude',
			models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }],
			permission_modes: ['owner', 'ask', 'bypass', 'read_only']
		}
	]
};

type Variant = 'plain' | 'parent' | 'route' | 'nudge' | 'parent-route-nudge';
const VARIANTS: Variant[] = ['plain', 'parent', 'route', 'nudge', 'parent-route-nudge'];
const WIDTHS = [360, 412] as const;

/** Потолок шапки для варианта: родитель + одна строка маршрута и толчка. */
const MAX_HEIGHT: Record<Variant, number> = {
	plain: 130,
	parent: 150,
	route: 180,
	nudge: 180,
	'parent-route-nudge': 195
};

function nudgeRule(id: string) {
	return {
		session_id: id,
		enabled: true,
		state: 'watching',
		idle_minutes: 3,
		background_minutes: 10,
		max_consecutive: 3,
		max_total: 20,
		done_markers: ['STATUS: DONE', 'STATUS: BLOCKED'],
		context_warn_percent: 80,
		set_by: 'agent',
		set_by_session: null,
		notify_session_id: null,
		consecutive: 2,
		total: 12,
		held: false,
		report_file: '/tmp/w/report.md'
	};
}

const OBSERVED = new Date(Date.now() - 60_000).toISOString();
const AT = (hours: number) => new Date(Date.now() + hours * 3_600_000).toISOString();
// Те же две полосы лимитов, что в шапке ревью: они входят в высоту шапки.
const LIMITS = [
	{
		provider: 'claude',
		weekly_reserve_active: true,
		five_hour: { used_permille: 995, resets_at: AT(4.9), observed_at: OBSERVED },
		weekly: { used_permille: 960, resets_at: AT(167), observed_at: OBSERVED }
	},
	{ provider: 'codex', weekly_reserve_active: false, five_hour: null, weekly: { used_permille: 150, resets_at: AT(24), observed_at: OBSERVED } }
] as never;

interface Booted {
	hub: FakeHub;
	api: FakeHubApi;
	id: string;
}

async function boot(
	page: Page,
	opts: { width: number; height?: number; theme?: string; palette?: string; variant: Variant }
): Promise<Booted> {
	await page.setViewportSize({ width: opts.width, height: opts.height ?? 892 });
	await page.addInitScript(
		({ theme, palette }) => {
			// Матрица палитр меняет значения перед reload: стартовый скрипт не должен их перетирать.
			if (localStorage.getItem('canterlot.theme') === null) localStorage.setItem('canterlot.theme', theme);
			if (localStorage.getItem('canterlot.palette') === null) localStorage.setItem('canterlot.palette', palette);
		},
		{ theme: opts.theme ?? 'dark', palette: opts.palette ?? 'standard' }
	);

	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	seedMachine(api, CATALOG as never);
	seedPresets(api);

	const parent = seedConversation(api, { title: 'Оркестратор релиза 1.38.0 с очень длинным названием для проверки переноса' });
	const id = seedConversation(api, {
		title: 'Восстановление потока после обрыва связи у воркера на fenicu-workstation-proxmox-vm-124',
		model: 'opus-5.5',
		...(opts.variant.includes('parent') ? { parent_id: parent, spawn_depth: 1 } : {})
	});
	const detail = api.details.get(id)!;
	detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
	detail.machine = { id: 'workstation', name: 'workstation' };
	if (opts.variant.includes('route')) detail.routing = ROUTE_DECISION;
	if (opts.variant.includes('nudge')) api.nudgeRules.set(id, nudgeRule(id) as never);
	api.limits = LIMITS;

	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect.poll(() => hub.subscribed(`conversation:${id}`), { timeout: 15_000 }).toBe(true);
	return { hub, api, id };
}

interface Box {
	x: number;
	y: number;
	width: number;
	height: number;
}

interface HeaderGeometry {
	header: Box;
	titleRow: Box;
	title: Box;
	back: Box | null;
	messages: Box;
	menu: Box;
	parent: Box | null;
	route: Box | null;
	nudge: Box | null;
	rows: Array<{ key: string; box: Box; lines: number; textX: number }>;
	textLines: Array<{ key: string; top: number; bottom: number }>;
	lineGaps: number[];
	hitSpans: Record<string, number>;
	hitBoxes: Record<string, Box>;
	hitOverlaps: string[];
	textOverlaps: string[];
	routeFirstWordVisible: boolean | null;
}

async function geometry(page: Page): Promise<HeaderGeometry> {
	return page.evaluate(() => {
		const box = (element: Element | null): Box | null => {
			if (!element) return null;
			const rect = element.getBoundingClientRect();
			return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
		};
		const find = (selector: string): HTMLElement | null => document.querySelector<HTMLElement>(selector);
		const textRects = (element: Element): DOMRect[] => {
			const range = document.createRange();
			range.selectNodeContents(element);
			return [...range.getClientRects()].filter((rect) => rect.width > 1 && rect.height > 1);
		};
		// Видимая часть текста: обрезанный многоточием текст отдаёт и полный прямоугольник за контролом.
		const visibleRects = (element: Element): Array<{ x: number; y: number; width: number; height: number }> => {
			const bounds = element.getBoundingClientRect();
			return textRects(element)
				.map((rect) => ({
					x: Math.max(rect.x, bounds.left),
					y: Math.max(rect.y, bounds.top),
					width: Math.min(rect.right, bounds.right) - Math.max(rect.x, bounds.left),
					height: Math.min(rect.bottom, bounds.bottom) - Math.max(rect.y, bounds.top)
				}))
				.filter((rect) => rect.width > 1 && rect.height > 1);
		};
		const lineCount = (element: Element): number => {
			const ys = new Set(textRects(element).map((rect) => Math.round(rect.top)));
			return ys.size;
		};
		const textX = (element: Element): number => {
			const rects = textRects(element);
			return rects.length ? Math.min(...rects.map((rect) => rect.x)) : element.getBoundingClientRect().x;
		};
		// У ссылки на родителя многоточие живёт на внутренней строке: якорь не обрезает свою цель 44 px.
		const textSource = (element: Element): Element => element.querySelector('.parent-text') ?? element;

		const title = find('.header-title')!;
		const titleBox = box(title)!;
		const rows: HeaderGeometry['rows'] = [];
		for (const [key, selector] of [
			['parent', '[data-parent-link]'],
			['route', '[data-role="route-state-line"]'],
			['nudge', '[data-role="nudge-state-line"]']
		] as const) {
			const element = find(selector);
			if (element) rows.push({ key, box: box(element)!, lines: lineCount(textSource(element)), textX: textX(textSource(element)) });
		}

		// Настоящая зона нажатия: точка попадания по вертикали через текст контрола, включая ::after.
		const controls: Record<string, HTMLElement> = {};
		for (const [key, selector] of [
			['back', '.back'],
			['messages', '.messages-btn'],
			['menu', '.menu-btn'],
			['parent', '[data-parent-link]'],
			['route', '[data-role="route-state-line"]'],
			['nudge', '[data-role="nudge-state-line"]'],
			['limits', '.header-limits summary']
		] as const) {
			const element = find(selector);
			if (element) controls[key] = element;
		}

		const hitSpans: Record<string, number> = {};
		for (const [key, element] of Object.entries(controls)) {
			const rects = textRects(textSource(element));
			const bounds = element.getBoundingClientRect();
			const rect = rects[0] ?? bounds;
			// Центр строки, а не угол: скруглённые углы пилюли срезают только углы цели.
			const x = bounds.x + bounds.width / 2;
			const y = rect.y + rect.height / 2;
			let first: number | null = null;
			let last: number | null = null;
			for (let offset = -48; offset <= 48; offset += 1) {
				const stack = document.elementsFromPoint(x, y + offset);
				if (stack.some((node) => node === element || element.contains(node))) {
					if (first === null) first = offset;
					last = offset;
				}
			}
			hitSpans[key] = first === null || last === null ? 0 : last - first + 1;
		}

		// Коробка цели: элемент плюс его out-of-layout ::after, если он есть.
		const hitBox = (element: HTMLElement): Box => {
			const rect = element.getBoundingClientRect();
			const after = getComputedStyle(element, '::after');
			if (after.content !== 'none' && after.position === 'absolute') {
				const height = Number.parseFloat(after.height);
				if (Number.isFinite(height) && height > 0) {
					const top = after.top === 'auto' ? rect.height - height - (Number.parseFloat(after.bottom) || 0) : Number.parseFloat(after.top);
					const left = after.left === 'auto' ? 0 : Number.parseFloat(after.left) || 0;
					const right = after.right === 'auto' ? 0 : Number.parseFloat(after.right) || 0;
					return { x: rect.x + left, y: rect.y + top, width: rect.width - left - right, height };
				}
			}
			return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
		};
		const hitBoxes = Object.fromEntries(Object.entries(controls).map(([key, element]) => [key, hitBox(element)]));
		const hitOverlaps: string[] = [];
		const entries = Object.entries(hitBoxes);
		for (let i = 0; i < entries.length; i += 1) {
			for (let j = i + 1; j < entries.length; j += 1) {
				const [aKey, a] = entries[i]!;
				const [bKey, b] = entries[j]!;
				if (a.x < b.x + b.width - 0.5 && b.x < a.x + a.width - 0.5 && a.y < b.y + b.height - 0.5 && b.y < a.y + a.height - 0.5) {
					hitOverlaps.push(`${aKey}~${bKey}`);
				}
			}
		}

		// Строки шапки в порядке чтения: название, подзаголовок, родитель, маршрут/автопинок.
		const lineBox = (element: Element | null): { top: number; bottom: number } | null => {
			if (!element) return null;
			const rects = visibleRects(textSource(element));
			if (!rects.length) {
				const rect = element.getBoundingClientRect();
				return { top: rect.top, bottom: rect.bottom };
			}
			return { top: Math.min(...rects.map((rect) => rect.y)), bottom: Math.max(...rects.map((rect) => rect.y + rect.height)) };
		};
		const textLines: HeaderGeometry['textLines'] = [];
		const pushLine = (key: string, element: Element | null) => {
			const line = lineBox(element);
			if (line) textLines.push({ key, ...line });
		};
		pushLine('title', title);
		pushLine('subtitle', find('.subtitle'));
		pushLine('parent', find('[data-parent-link]'));
		const contextLine = [lineBox(find('[data-role="route-state-line"]')), lineBox(find('[data-role="nudge-state-line"]'))].filter(
			(line): line is { top: number; bottom: number } => line !== null
		);
		if (contextLine.length) {
			textLines.push({ key: 'context', top: Math.min(...contextLine.map((line) => line.top)), bottom: Math.max(...contextLine.map((line) => line.bottom)) });
		}
		const lineGaps = textLines.slice(1).map((line, index) => line.top - textLines[index]!.bottom);

		// Первое слово названия маршрута должно попасть в видимую часть кнопки, а не в многоточие.
		let routeFirstWordVisible: boolean | null = null;
		const route = find('[data-role="route-state-line"]');
		if (route) {
			routeFirstWordVisible = false;
			const word = 'Исправление';
			const walker = document.createTreeWalker(route, NodeFilter.SHOW_TEXT);
			let node: Text | null = null;
			let index = -1;
			while (walker.nextNode()) {
				const next = walker.currentNode as Text;
				const found = (next.textContent ?? '').indexOf(word);
				if (found >= 0) {
					node = next;
					index = found;
					break;
				}
			}
			if (node) {
				const range = document.createRange();
				range.setStart(node, index);
				range.setEnd(node, index + word.length);
				const wordRect = range.getBoundingClientRect();
				const text = route.querySelector('.context-text') ?? route;
				const textRect = text.getBoundingClientRect();
				// Когда строка обрезана, многоточие тоже занимает место: слово должно влезть вместе с ним.
				const overflowing = text.scrollWidth > text.clientWidth + 0.5;
				const canvas = document.createElement('canvas').getContext('2d')!;
				const style = getComputedStyle(route);
				canvas.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
				const ellipsis = overflowing ? canvas.measureText('…').width : 0;
				routeFirstWordVisible =
					wordRect.width > 0 &&
					wordRect.left >= textRect.left - 0.5 &&
					wordRect.right + ellipsis <= textRect.right + 0.5;
			}
		}

		// Чужая зона нажатия под видимым текстом контрола: элемент попадает в стопку точек текста.
		const textOverlaps: string[] = [];
		for (const [outerKey, outer] of Object.entries(controls)) {
			for (const [innerKey, inner] of Object.entries(controls)) {
				if (outerKey === innerKey || outer.contains(inner) || inner.contains(outer)) continue;
				for (const rect of visibleRects(textSource(inner))) {
					const points = [
						rect.x + Math.min(3, rect.width / 2),
						rect.x + rect.width / 2,
						rect.x + rect.width - Math.min(3, rect.width / 2)
					];
					const covered = points.some((x) =>
						document.elementsFromPoint(x, rect.y + rect.height / 2).some((node) => node === outer || outer.contains(node))
					);
					if (covered) {
						textOverlaps.push(`${outerKey}->${innerKey}`);
						break;
					}
				}
			}
		}

		return {
			header: box(find('.chat-header'))!,
			titleRow: box(find('.title-row'))!,
			title: titleBox,
			back: box(find('.back')),
			messages: box(find('.messages-btn'))!,
			menu: box(find('.menu-btn'))!,
			parent: box(find('[data-parent-link]')),
			route: box(find('[data-role="route-state-line"]')),
			nudge: box(find('[data-role="nudge-state-line"]')),
			rows,
			textLines,
			lineGaps,
			hitSpans,
			hitBoxes,
			hitOverlaps,
			textOverlaps,
			routeFirstWordVisible
		};
	});
}

function expectAlignedWithTitle(g: HeaderGeometry): void {
	const titleTop = g.titleRow.y;
	for (const [key, control] of [['back', g.back], ['messages', g.messages], ['menu', g.menu]] as const) {
		if (!control) continue;
		expect(Math.abs(control.y - titleTop), `${key} стоит на строке названия`).toBeLessThanOrEqual(1.5);
	}
}

function expectRowsInset(g: HeaderGeometry): void {
	for (const row of g.rows) {
		expect(row.textX, `${row.key} не уходит левее колонки названия`).toBeGreaterThanOrEqual(g.title.x - 1);
		if (row.key === 'parent') {
			expect(Math.abs(row.textX - g.title.x), 'ссылка на родителя начинается от колонки названия').toBeLessThanOrEqual(1);
		}
	}
	// Маршрут и автопинок делят одну строку: от колонки названия начинается её первая цель.
	const contextRows = g.rows.filter((row) => row.key !== 'parent');
	if (contextRows.length) {
		expect(
			Math.min(...contextRows.map((row) => row.textX)) - g.title.x,
			'строка маршрута и автопинка начинается от колонки названия'
		).toBeLessThanOrEqual(1);
	}
}

function expectOneLineRows(g: HeaderGeometry): void {
	for (const row of g.rows) expect(row.lines, `${row.key} занимает одну строку`).toBe(1);
}

function expectEvenLineGaps(g: HeaderGeometry): void {
	expect(g.textLines.length, 'в шапке есть строки текста').toBeGreaterThan(1);
	const min = Math.min(...g.lineGaps);
	const max = Math.max(...g.lineGaps);
	expect(max - min, `разрывы строк шапки ровные: ${g.lineGaps.map((gap) => gap.toFixed(1)).join(', ')}`).toBeLessThanOrEqual(2);
}

function expectTapTargets(g: HeaderGeometry): void {
	for (const [key, span] of Object.entries(g.hitSpans)) {
		expect(span, `зона нажатия ${key} не меньше 44 px`).toBeGreaterThanOrEqual(44);
	}
	expect(g.hitOverlaps, 'зоны нажатия не перекрывают друг друга').toEqual([]);
	expect(g.textOverlaps, 'зоны нажатия не накрывают текст других контролов').toEqual([]);
}

function expectRouteNameVisible(g: HeaderGeometry): void {
	if (g.route) expect(g.routeFirstWordVisible, 'первое слово названия маршрута видно, а не срезано многоточием').toBe(true);
}

async function shoot(page: Page, label: string): Promise<void> {
	if (!SHOTS) return;
	mkdirSync(SHOTS, { recursive: true });
	await page.locator('.chat-header').screenshot({ path: `${SHOTS}/${label}.png`, animations: 'disabled' });
}

for (const width of WIDTHS) {
	for (const variant of VARIANTS) {
		const theme = width === 412 ? 'dark' : 'light';

		test(`header_rows_align_and_fit_${variant}_${width}`, async ({ page }, info) => {
			test.skip(info.project.name !== 'phone', 'телефонная шапка');
			await boot(page, { width, theme, variant });
			await shoot(page, `${variant}-${width}-${theme}`);
			const g = await geometry(page);

			expectAlignedWithTitle(g);
			expectRowsInset(g);
			expectOneLineRows(g);
			expectEvenLineGaps(g);
			expectRouteNameVisible(g);
			expect(g.header.height, `высота шапки ${variant} при ${width}`).toBeLessThanOrEqual(MAX_HEIGHT[variant]);
		});

		test(`header_tap_targets_${variant}_${width}`, async ({ page }, info) => {
			test.skip(info.project.name !== 'phone', 'телефонная шапка');
			await boot(page, { width, theme, variant });
			const g = await geometry(page);
			expectTapTargets(g);
		});
	}
}

test('header_rows_hold_in_every_palette_and_both_themes', async ({ page }, info) => {
	test.skip(info.project.name !== 'phone', 'телефонная шапка');
	test.setTimeout(240_000);
	await boot(page, { width: 360, theme: 'dark', palette: 'standard', variant: 'parent-route-nudge' });
	for (const theme of ['dark', 'light']) {
		for (const palette of PALETTES) {
			await page.evaluate(
				({ palette, theme }) => {
					localStorage.setItem('canterlot.palette', palette);
					localStorage.setItem('canterlot.theme', theme);
				},
				{ palette: palette.key, theme }
			);
			await page.reload();
			await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
			await expect(page.locator('html')).toHaveAttribute('data-palette', palette.key);
			await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
			const g = await geometry(page);
			expectAlignedWithTitle(g);
			expectRowsInset(g);
			expectOneLineRows(g);
			expectEvenLineGaps(g);
			expectRouteNameVisible(g);
			expect(g.header.height, `${palette.key}/${theme}`).toBeLessThanOrEqual(MAX_HEIGHT['parent-route-nudge']);
		}
	}
});

test('desktop_header_keeps_one_line_rows', async ({ page }, info) => {
	test.skip(info.project.name !== 'desktop', 'настольная шапка');
	await boot(page, { width: 1440, height: 900, theme: 'dark', variant: 'parent-route-nudge' });
	// Под нагрузкой строка маршрута успевает измериться до того, как шрифт и соседняя строка
	// автопинка встали на место: ждём устоявшейся раскладки, а не первого кадра.
	await page.evaluate(() => document.fonts.ready);
	await expect.poll(async () => (await geometry(page)).routeFirstWordVisible).toBe(true);
	const g = await geometry(page);
	expect(g.back, 'на столе стрелки назад нет').toBeNull();
	expectAlignedWithTitle(g);
	expectRowsInset(g);
	expectOneLineRows(g);
	expectEvenLineGaps(g);
	expectRouteNameVisible(g);
	expectTapTargets(g);
});
