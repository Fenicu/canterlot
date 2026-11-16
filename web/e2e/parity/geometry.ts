import type { Page } from '@playwright/test';

export interface Finding {
	rule: string;
	where: string;
	detail: string;
	count: number;
}

export interface Audit {
	hard: Finding[];
	soft: Finding[];
}

/**
 * Размеры, которые по замыслу шире своего окна и потому прокручиваются вбок или обрезаются.
 * Каждый вход — осознанное решение макета; всё остальное считаем дефектом.
 */
export const HORIZONTAL_SCROLL_ALLOW: { selector: string; reason: string }[] = [
	{
		selector: 'pre',
		reason:
			'Блок кода не переносит строки: команду и вывод читают как есть, перенос ломал бы отступы. Прокрутка вбок — единственный способ увидеть длинную строку.'
	},
	{
		selector: '.prose table',
		reason:
			'Таблица ответа шире карточки по определению: колонки держат ширину содержимого, иначе ячейки с кодом рвутся по буквам. Строки не переносятся, таблица едет вбок целиком.'
	},
	{
		selector: '[data-tabs-scroller]',
		reason:
			'Полоса вкладок групп сессий: вкладки не переносятся на вторую строку, иначе список сессий уезжает вниз. Лишние вкладки достаются прокруткой.'
	},
	{
		selector: '[data-sheet-tabs]',
		reason:
			'Полоса листов книги в просмотрщике таблиц: имена листов держат собственную ширину, а число листов задаёт файл, не экран.'
	},
	{
		selector: '.sheet-grid > .grid',
		reason:
			'Сетка листа книги шире телефона по своей природе: колонки задаёт файл, а не макет. Вбок едет только она, шапка и лист остаются на месте.'
	},
	{
		selector: 'dialog.viewer',
		reason:
			'Галерея снимков: картинка лежит на сцене в натуральную величину и двигается жестами, а не прокруткой. Обрезка по краю окна — то, что делает панораму панорамой.'
	}
];

/** Контейнер, внутри которого сравниваются цели нажатия: карточка, лист, окно или шапка. */
export const CONTROL_GROUP_SELECTOR =
	'article, dialog, [role="dialog"], [role="menu"], [role="listbox"], [role="tablist"], header, footer, form, nav, .card-root, .sheet, .capsule';

/** Ниже этой высоты лента считается схлопнутой: под неё не видно ни одного сообщения. */
export const FEED_FLOOR = 64;

/**
 * Геометрический обход уже загруженной страницы: инварианты раскладки, которых не видит ни axe,
 * ни проверка токенов. `hard` — падение теста, `soft` — сверка с эталоном известных случаев.
 */
export async function geometryAudit(page: Page): Promise<Audit> {
	return page.evaluate(
		({ allow, groupSelector, feedFloor }) => {
			interface Item {
				rule: string;
				where: string;
				detail: string;
				count: number;
			}
			const hard = new Map<string, Item>();
			const soft = new Map<string, Item>();

			const classNames = (el: Element): string => {
				if (el instanceof SVGElement) return '';
				const raw = (el.getAttribute('class') ?? '').trim();
				if (!raw) return '';
				return raw
					.split(/\s+/)
					.filter((name) => name && !name.startsWith('svelte-') && !/^s-[a-z0-9]+$/.test(name))
					.slice(0, 2)
					.map((name) => `.${name}`)
					.join('');
			};
			const name = (el: Element): string => {
				const role = el.getAttribute('data-role') ?? el.getAttribute('role');
				return `${el.tagName.toLowerCase()}${classNames(el)}${role ? `[${role}]` : ''}`;
			};
			/** Устойчивый к правкам ключ: сам элемент и два предка без хешей svelte. */
			const path = (el: Element): string => {
				const parts = [name(el)];
				let node = el.parentElement;
				for (let depth = 0; node && node !== document.body && depth < 2; depth++, node = node.parentElement) parts.unshift(name(node));
				return parts.join(' > ');
			};
			const snippet = (el: Element): string => (el.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 48);
			const r1 = (value: number): number => Math.round(value * 10) / 10;
			const note = (store: Map<string, Item>, rule: string, el: Element, detail: string): void => {
				const where = path(el);
				const key = `${rule}|${where}`;
				const known = store.get(key);
				if (known) known.count += 1;
				else store.set(key, { rule, where, detail, count: 1 });
			};

			const styles = new Map<Element, CSSStyleDeclaration>();
			const style = (el: Element): CSSStyleDeclaration => {
				let found = styles.get(el);
				if (!found) {
					found = getComputedStyle(el);
					styles.set(el, found);
				}
				return found;
			};
			/**
			 * Скрытое по замыслу содержимое. `inert` сюда не входит: оболочка под открытым листом
			 * помечена им, но лист лежит внутри неё и остаётся видимым. Недоступные для нажатия
			 * цели отсеиваются отдельно — в проверке пересечений.
			 */
			const folded = (el: Element): boolean => el.closest('.sr-only, [hidden], details:not([open])') !== null;
			const decorated = (el: Element): boolean => el.closest('[aria-hidden="true"]') !== null;
			const reachable = (el: Element): boolean => !decorated(el) && el.closest('[inert]') === null;
			const visible = (el: Element): boolean => {
				const rect = el.getBoundingClientRect();
				if (rect.width < 1 || rect.height < 1) return false;
				const computed = style(el);
				if (computed.display === 'none' || computed.visibility === 'hidden' || computed.opacity === '0') return false;
				if (computed.contentVisibility === 'hidden') return false;
				return !folded(el);
			};

			const vw = document.documentElement.clientWidth;
			const vh = document.documentElement.clientHeight;
			const all = [...document.body.querySelectorAll<HTMLElement>('*')].filter(visible);

			/** Пересечение всех обрезающих предков и окна: что из элемента реально видно. */
			const clipRect = (el: Element): { left: number; top: number; right: number; bottom: number } => {
				let left = 0;
				let top = 0;
				let right = vw;
				let bottom = vh;
				for (let node = el.parentElement; node && node !== document.documentElement; node = node.parentElement) {
					const computed = style(node);
					if (computed.overflowX === 'visible' && computed.overflowY === 'visible') continue;
					const rect = node.getBoundingClientRect();
					if (computed.overflowX !== 'visible') {
						left = Math.max(left, rect.left);
						right = Math.min(right, rect.right);
					}
					if (computed.overflowY !== 'visible') {
						top = Math.max(top, rect.top);
						bottom = Math.min(bottom, rect.bottom);
					}
				}
				return { left, top, right, bottom };
			};

			// 1. Ни один элемент с невидимым overflow-x не прячет текст вбок.
			for (const el of all) {
				if (el instanceof SVGElement) continue;
				const computed = style(el);
				if (computed.overflowX === 'visible') continue;
				// Поле ввода листает своё значение само — это его штатное поведение.
				if (el.matches('input, textarea, select')) continue;
				// Скрытая от глаз подпись шириной в пиксель: содержимое заведомо шире коробки.
				if (el.clientWidth < 8 || el.scrollWidth <= el.clientWidth + 1) continue;
				// Многоточие и обрезка по строкам — заданная усечённая ширина, а не дефект.
				if (computed.textOverflow === 'ellipsis') continue;
				if (computed.webkitLineClamp && computed.webkitLineClamp !== 'none') continue;
				const allowed = allow.find((selector) => el.matches(selector));
				if (allowed) continue;
				// Кто именно вылез: самый глубокий узел, которому не хватает ширины, экономит
				// поиск по скриншоту — в тексте это сам абзац, а не коробка карточки.
				let offender = '';
				let culprit: Element = el;
				for (const child of el.querySelectorAll('*')) {
					if (child instanceof SVGElement) continue;
					if (child.clientWidth > 0 && child.scrollWidth > child.clientWidth + 1) culprit = child;
				}
				if (culprit !== el) offender = `; глубже всех ${path(culprit)} «${snippet(culprit)}»`;
				note(
					hard,
					`overflow-x ${computed.overflowX}: содержимое шире окна`,
					el,
					`scrollWidth ${el.scrollWidth} > clientWidth ${el.clientWidth} «${snippet(el)}»${offender}`
				);
			}
			if (document.documentElement.scrollWidth > vw + 1) {
				note(hard, 'страница прокручивается вбок', document.body, `scrollWidth ${document.documentElement.scrollWidth} > ${vw}`);
			}

			// 2. Поле ввода в чате: кнопка отправки в окне, лента не схлопнута.
			const composer = document.querySelector('.composer');
			if (composer) {
				const send = composer.querySelector('.send');
				if (!send) note(hard, 'у поля ввода нет кнопки отправки', composer, '');
				else {
					const box = send.getBoundingClientRect();
					if (box.width === 0 || box.height === 0) note(hard, 'кнопка отправки не отрисована', send, '');
					else if (box.left < -0.5 || box.top < -0.5 || box.right > vw + 0.5 || box.bottom > vh + 0.5) {
						note(
							hard,
							'кнопка отправки вне окна',
							composer,
							`${r1(box.left)},${r1(box.top)}..${r1(box.right)},${r1(box.bottom)} при окне ${vw}x${vh}`
						);
					}
				}
				// Открытая карточка запроса важнее ленты: она имеет право забрать её высоту.
				const requestOpen = document.querySelector('.request-cards-slot > *, .request-card-slot > *') !== null;
				const feed = document.querySelector('.feed');
				if (feed && !requestOpen) {
					const height = feed.getBoundingClientRect().height;
					if (height < feedFloor - 0.5) note(hard, `лента ниже ${feedFloor} px`, feed, `height ${r1(height)}`);
				}
			}

			// 3. Ни одна строка текста не накрыта чужим элементом: центр строки принадлежит тексту.
			/** Всплывающий слой (меню, окно, лист) вправе закрывать страницу под собой. */
			const overlayOf = (el: Element): Element | null =>
				el.closest('dialog[open], [role="dialog"], [role="menu"], [popover]');
			/** Липкая полоса прокручивается вместе с содержимым: строки под ней доезжают до верха. */
			const sticky = (el: Element): boolean => {
				for (let node: Element | null = el; node && node !== document.body; node = node.parentElement) {
					if (style(node).position === 'sticky') return true;
				}
				return false;
			};
			/** Прозрачная пустая нажималка поверх строки шапки (CardShell): текст остаётся видимым. */
			const painted = (el: Element): boolean => {
				const computed = style(el);
				if (computed.backgroundColor !== 'rgba(0, 0, 0, 0)' && computed.backgroundColor !== 'transparent') return true;
				if (computed.backgroundImage !== 'none') return true;
				if ((el.textContent ?? '').trim() !== '') return true;
				return el.querySelector('svg, img, canvas, video, picture') !== null;
			};
			const range = document.createRange();
			const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
			let seen = 0;
			for (let node = walker.nextNode(); node && seen < 6000; node = walker.nextNode()) {
				const value = node.textContent ?? '';
				if (value.trim().length < 2) continue;
				const parent = (node as Text).parentElement;
				if (!parent || !visible(parent) || decorated(parent)) continue;
				if (parent instanceof SVGElement) continue;
				seen += 1;
				range.selectNodeContents(node);
				const clip = clipRect(parent);
				// Строка считается видимой только внутри коробки своего элемента: у текста
				// с многоточием прямоугольник строки длиннее коробки и выходит за её край.
				const own = parent.getBoundingClientRect();
				let covered: string | null = null;
				for (const line of range.getClientRects()) {
					if (line.width < 4 || line.height < 4) continue;
					// Центр строки — основной замер; концы нужны, чтобы поймать кнопку, накрывшую
					// хвост строки: цель касания в правом верхнем углу блока кода или цитаты.
					for (const share of [0.5, 0.1, 0.9]) {
						const x = line.left + line.width * share;
						const y = line.top + line.height / 2;
						if (x < own.left + 1 || x > own.right - 1 || y < own.top + 1 || y > own.bottom - 1) continue;
						if (x < clip.left + 1 || x > clip.right - 1 || y < clip.top + 1 || y > clip.bottom - 1) continue;
						if (x < 0 || y < 0 || x >= vw || y >= vh) continue;
						const top = document.elementFromPoint(x, y);
						if (!top) continue;
						// Родной элемент строки или вложенный в него: точка попала в сам текст.
						// Сосед по тому же блоку (кнопка копирования поверх цитаты) — уже накрытие.
						if (top === parent || top.contains(node)) continue;
						// Открытое меню или окно закрывает страницу по своему замыслу.
						const overlay = overlayOf(top);
						if (overlay && !overlay.contains(parent)) continue;
						// Липкая полоса (подвал настроек) — содержимое уезжает из-под неё прокруткой.
						if (sticky(top)) continue;
						// Прозрачная пустая нажималка: строку видно, она лишь служит целью нажатия.
						if (!painted(top)) continue;
						covered = `строка «${value.trim().replace(/\s+/g, ' ').slice(0, 30)}» в ${r1(x)},${r1(y)} накрыта ${path(top)}`;
						break;
					}
					if (covered) break;
				}
				if (covered) note(hard, 'текст накрыт элементом', parent, covered);
			}

			// 4. Две цели нажатия внутри одной карточки, листа или окна не пересекаются.
			const controls = all.filter(
				(el) =>
					el.matches('a[href], button, [role="button"], [role="menuitem"], [role="tab"], [role="radio"], [role="switch"], input, select, textarea, summary') &&
					reachable(el) &&
					!(el instanceof HTMLAnchorElement && el.closest('p, li, span, label, td'))
			);
			/** Вынос цели касания в абсолютный ::after — как в проверке размера цели. */
			const hitBox = (el: Element): DOMRect => {
				const box = el.getBoundingClientRect();
				const hit = getComputedStyle(el, '::after');
				if (hit.content === 'none' || hit.position !== 'absolute') return box;
				const grow = (side: string, start: number, end: number) => {
					const value = parseFloat(side) || 0;
					return value < 0 ? [start - value, end + value] : [start, end];
				};
				const [left, right] = grow(hit.left, box.left, box.right);
				const [top, bottom] = grow(hit.top, box.top, box.bottom);
				return new DOMRect(left, top, right - left, bottom - top);
			};
			const groups = new Map<Element, { el: HTMLElement; box: DOMRect; clip: { left: number; top: number; right: number; bottom: number } }[]>();
			for (const el of controls) {
				const group = el.closest(groupSelector) ?? document.body;
				// Коробки и обрезки считаются один раз на цель: разбор ::after у каждой пары стоил
				// дороже самого обхода и растягивал аудит на каждом экране.
				const entry = { el, box: hitBox(el), clip: clipRect(el) };
				const list = groups.get(group);
				if (list) list.push(entry);
				else groups.set(group, [entry]);
			}
			for (const [group, items] of groups) {
				for (let i = 0; i < items.length; i++) {
					const a = items[i]!;
					for (let j = i + 1; j < items.length; j++) {
						const b = items[j]!;
						if (a.el.contains(b.el) || b.el.contains(a.el)) continue;
						const width = Math.min(a.box.right, b.box.right) - Math.max(a.box.left, b.box.left);
						const height = Math.min(a.box.bottom, b.box.bottom) - Math.max(a.box.top, b.box.top);
						if (width <= 2 || height <= 2) continue;
						const cx = Math.max(a.box.left, b.box.left) + width / 2;
						const cy = Math.max(a.box.top, b.box.top) + height / 2;
						if (cx < a.clip.left || cx > a.clip.right || cy < a.clip.top || cy > a.clip.bottom) continue;
						if (cx < b.clip.left || cx > b.clip.right || cy < b.clip.top || cy > b.clip.bottom) continue;
						note(
							hard,
							'цели нажатия пересекаются',
							group === document.body ? a.el : group,
							`${r1(width)}x${r1(height)} px: «${snippet(a.el)}» (${name(a.el)}) и «${snippet(b.el)}» (${name(b.el)})`
						);
					}
				}
			}

			// 5. Мягкие проверки: известные случаи сверяются с эталоном.
			/**
			 * Перенос строки внутри одного текстового узла: шрифт у него один, поэтому строки
			 * разделяются ровно на междустрочный шаг. Разные кассы и подписи в одной строке
			 * (метка с числом, название с путём) этот счёт не путают.
			 */
			const wrappedLines = (el: Element, lineHeight: number): number => {
				const probe = document.createRange();
				const nodes = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
				let most = 1;
				for (let node = nodes.nextNode(); node; node = nodes.nextNode()) {
					if ((node.textContent ?? '').trim() === '') continue;
					const owner = node.parentElement;
					if (!owner || owner.matches('input, textarea, select, option') || decorated(owner)) continue;
					probe.selectNodeContents(node);
					const tops = [...probe.getClientRects()].filter((rect) => rect.width > 1 && rect.height > 1).map((rect) => rect.top).sort((a, b) => a - b);
					if (tops.length < 2) continue;
					let lines = 1;
					for (let index = 1; index < tops.length; index++) if (tops[index]! - tops[index - 1]! >= lineHeight * 0.5) lines += 1;
					most = Math.max(most, lines);
				}
				return most;
			};
			const hasBox = (el: Element): boolean => {
				const computed = style(el);
				if (computed.backgroundColor !== 'rgba(0, 0, 0, 0)' && computed.backgroundColor !== 'transparent') return true;
				if (['Top', 'Right', 'Bottom', 'Left'].some((side) => computed.getPropertyValue(`border-${side.toLowerCase()}-width`) !== '0px')) return true;
				return computed.outlineStyle !== 'none' && parseFloat(computed.outlineWidth) > 0;
			};
			const textLine = (el: Element): DOMRect | null => {
				const probe = document.createRange();
				probe.selectNodeContents(el);
				const lines = [...probe.getClientRects()].filter((rect) => rect.width > 1 && rect.height > 1);
				if (lines.length === 0) return null;
				return lines.reduce((a, b) => (b.height > a.height ? b : a));
			};
			const hasText = (el: Element): boolean => (el.textContent ?? '').trim() !== '';
			const iconOnly = (el: Element): boolean => el instanceof SVGElement || (el.querySelector('svg') !== null && !hasText(el));
			/** Строки переноса: соседи в одной строке пересекаются по вертикали. */
			const groupRows = (kids: Element[]): Element[][] => {
				// Порядок задаёт `order` в flex и раскладка сетки, а не порядок в разметке:
				// идём по верхнему краю коробок и ищем строку по пересечению среди всех уже собранных.
				const sorted = kids
					.map((kid) => ({ kid, box: kid.getBoundingClientRect() }))
					.sort((a, b) => a.box.top - b.box.top || a.box.left - b.box.left);
				const rows: { items: Element[]; top: number; bottom: number; height: number }[] = [];
				for (const { kid, box } of sorted) {
					const row = rows.find((item) => {
						const overlap = Math.min(item.bottom, box.bottom) - Math.max(item.top, box.top);
						return overlap > Math.min(item.height, box.height) * 0.5;
					});
					if (row) {
						row.items.push(kid);
						row.top = Math.min(row.top, box.top);
						row.bottom = Math.max(row.bottom, box.bottom);
						row.height = row.bottom - row.top;
						continue;
					}
					rows.push({ items: [kid], top: box.top, bottom: box.bottom, height: box.height });
				}
				return rows.sort((a, b) => a.top - b.top).map((row) => row.items);
			};
			/** Ширина, которую строка занимает по тексту: растянутая ячейка сетки оставляет вокруг себя пустоту. */
			const inkWidth = (el: Element): number => {
				const probe = document.createRange();
				const nodes = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
				let left = Number.POSITIVE_INFINITY;
				let right = Number.NEGATIVE_INFINITY;
				for (let node = nodes.nextNode(); node; node = nodes.nextNode()) {
					if ((node.textContent ?? '').trim() === '') continue;
					probe.selectNodeContents(node);
					for (const rect of probe.getClientRects()) {
						if (rect.width < 1) continue;
						left = Math.min(left, rect.left);
						right = Math.max(right, rect.right);
					}
				}
				if (!(right > left)) return el.getBoundingClientRect().width;
				return right - left;
			};

			for (const el of all) {
				if (el instanceof SVGElement) continue;
				const computed = style(el);
				const rect = el.getBoundingClientRect();

				// 5a. Пилюля на две строки: радиус скругления обещал одну. Берём именно токен пилюли
				// (сотни пикселей), а не радиус карточки, который на низкой коробке тоже доходит до половины высоты.
				const radius = parseFloat(computed.borderTopLeftRadius);
				if (radius >= 100 && hasBox(el) && hasText(el)) {
					const lineHeight = parseFloat(computed.lineHeight) || parseFloat(computed.fontSize) * 1.3;
					const lines = wrappedLines(el, lineHeight);
					if (lines > 1 && rect.height > lineHeight * 1.8) {
						note(soft, 'пилюля в две строки', el, `radius ${r1(radius)} height ${r1(rect.height)}, строк ${lines}: «${snippet(el)}»`);
					}
				}

				// 5b. Обрезка по строкам при отступе сверху или снизу: следующая строка видна в отступе.
				const clamp = computed.webkitLineClamp;
				if (clamp && clamp !== 'none') {
					const padTop = parseFloat(computed.paddingTop) || 0;
					const padBottom = parseFloat(computed.paddingBottom) || 0;
					if ((padTop > 0 || padBottom > 0) && el.clientHeight > 0 && el.scrollHeight > el.clientHeight + 1) {
						note(
							soft,
							'обрезанная строка видна в отступе',
							el,
							`clamp ${clamp}, padding ${r1(padTop)}/${r1(padBottom)}, scrollHeight ${el.scrollHeight} > clientHeight ${el.clientHeight}`
						);
					}
				}

				// 5c. Хвост строки упал на свою строку, хотя в предыдущей оставалось место.
				const wraps = (computed.display === 'flex' || computed.display === 'inline-flex') && computed.flexWrap === 'wrap';
				const rowsOfGrid = (computed.display === 'grid' || computed.display === 'inline-grid') && computed.gridTemplateColumns !== 'none';
				// Поле формы (подпись над полем ввода) — намеренная колонка, а не осиротевший хвост строки.
				if ((wraps || rowsOfGrid) && el.querySelector('input, select, textarea') === null) {
					// Крошка в пиксель — скрытый input[type=file] или декоративная метка, не элемент раскладки.
					const kids = [...el.children].filter((child) => {
						const box = child.getBoundingClientRect();
						return box.width >= 8 && box.height >= 8 && visible(child) && !decorated(child);
					});
					if (kids.length >= 2) {
						const rows = groupRows(kids);
						const inner = rect.width - (parseFloat(computed.paddingLeft) || 0) - (parseFloat(computed.paddingRight) || 0);
						const gap = parseFloat(computed.columnGap) || 0;
						if (rows.length > 1 && inner > 150) {
							for (let index = 1; index < rows.length; index++) {
								const row = rows[index]!;
								if (row.length !== 1) continue;
								const child = row[0]!;
								const childBox = child.getBoundingClientRect();
								if (childBox.width >= inner * 0.4) continue;
								// Ширина, занятая предыдущей строкой: если хвост туда помещался, его снесло зря —
								// так выглядят «1 из 2» под заголовком вопроса и «резерв» под строкой лимитов.
								const previous = rows[index - 1]!;
								const used = previous.reduce((sum, item) => sum + inkWidth(item) + gap, 0);
								if (used + childBox.width <= inner) {
									note(
										soft,
										'одинокий узкий ребёнок на своей строке',
										el,
										`«${snippet(child)}» ${r1(childBox.width)} px при ${r1(inner - used)} px свободных в предыдущей строке`
									);
									break;
								}
							}
						}
					}
				}

				// 5d. Значок и текст в одной строке стоят по одной оси: центры не расходятся больше 2 px.
				// Ряд бывает и строкой flex, и строкой сетки: у карточки очереди «⋯» стоит в grid.
				const rowContainer = computed.display === 'grid' || computed.display === 'inline-grid' || ((computed.display === 'flex' || computed.display === 'inline-flex') && /row/.test(computed.flexDirection));
				{
					if (rowContainer) {
						const kids = [...el.children].filter((child) => visible(child) && !decorated(child));
						const icon = kids.find(iconOnly);
						// Значок и текст сравнимы только в одной строке: в переносе они расходятся законно.
						const rows = icon ? groupRows(kids) : [];
						const row = rows.find((items) => items.includes(icon!));
						const text = rows.length === 1 ? row?.find((child) => !iconOnly(child) && hasText(child)) : undefined;
						if (icon && text) {
							const line = textLine(text);
							const textBox = text.getBoundingClientRect();
							// Колонка из двух строк (название и подпись) центрируется как блок:
							// значок стоит по её середине, и сравнивать его с одной строкой нечего.
							if (line && textBox.height <= line.height * 1.5) {
								const iconRect = icon.getBoundingClientRect();
								const delta = iconRect.top + iconRect.height / 2 - (line.top + line.height / 2);
								if (Math.abs(delta) > 2) {
									note(soft, 'значок и текст разъехались по вертикали', el, `центры расходятся на ${r1(delta)} px: «${snippet(el)}»`);
								}
							}
						}
					}
				}
			}

			return { hard: [...hard.values()], soft: [...soft.values()] };
		},
		{ allow: HORIZONTAL_SCROLL_ALLOW.map((entry) => entry.selector), groupSelector: CONTROL_GROUP_SELECTOR, feedFloor: FEED_FLOOR }
	);
}
