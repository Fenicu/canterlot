const rules = new Intl.PluralRules('ru');
const clockFmt = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const dayFmt = new Intl.DateTimeFormat('ru-RU', { day: 'numeric', month: 'long' });
const shortDayFmt = new Intl.DateTimeFormat('ru-RU', { day: 'numeric', month: 'short' });
const weekdayFmt = new Intl.DateTimeFormat('ru-RU', { weekday: 'short' });
const numberFmt = new Intl.NumberFormat('ru-RU');

function sameDay(a: Date, b: Date): boolean {
	return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

function dayBefore(now: Date, ts: Date): boolean {
	const prev = new Date(now);
	prev.setDate(prev.getDate() - 1);
	return sameDay(prev, ts);
}

function capitalize(value: string): string {
	return value.length === 0 ? value : value[0]!.toLocaleUpperCase('ru-RU') + value.slice(1);
}

function grouped(n: number): string {
	return numberFmt.format(n).replace(/[\u00a0\u202f]/g, ' ');
}

export const fmt = {
	duration(ms: number): string {
		const sign = ms < 0 ? '-' : '';
		const abs = Math.abs(ms);
		if (abs < 1000) {
			const seconds = abs / 1000;
			const text = seconds.toFixed(1).replace('.', ',');
			return `${sign}${text} с`;
		}
		const total = Math.floor(abs / 1000);
		const hours = Math.floor(total / 3600);
		const minutes = Math.floor((total % 3600) / 60);
		const seconds = total % 60;
		const parts: string[] = [];
		if (hours) parts.push(`${hours} ч`);
		if (minutes) parts.push(`${minutes} мин`);
		if (seconds) parts.push(`${seconds} с`);
		if (parts.length === 0) parts.push('0 с');
		return sign + parts.join(' ');
	},
	durationCoarse(ms: number): string {
		if (Math.abs(ms) < 60_000) return fmt.duration(ms);
		const total = Math.floor(Math.abs(ms) / 60_000);
		const hours = Math.floor(total / 60);
		const minutes = total % 60;
		return [hours ? `${hours} ч` : '', minutes ? `${minutes} мин` : ''].filter(Boolean).join(' ');
	},
	/** «1 ч 5 мин из 4 ч 19 мин»: работа внутри ходов против их общей длительности. */
	worked(workingMs: number, totalMs: number): string {
		const working = fmt.durationCoarse(Math.max(0, workingMs));
		const total = fmt.durationCoarse(Math.max(0, totalMs));
		return Math.abs(totalMs - workingMs) < 1000 ? total : `${working} из ${total}`;
	},
	clock(ts: number): string {
		return clockFmt.format(ts);
	},
	/** Дата без времени: «2 ноября», с годом, если он не текущий. */
	date(ts: number, now: number): string {
		const date = new Date(ts);
		const label = dayFmt.format(date);
		return date.getFullYear() === new Date(now).getFullYear() ? label : `${label} ${date.getFullYear()}`;
	},
	/**
	 * Время сообщения в списке: сегодня только часы, вчера с подписью,
	 * раньше — короткая дата и часы («5 нояб. 14:03»).
	 */
	messageTime(ts: number, now: number): string {
		const date = new Date(ts);
		if (sameDay(date, new Date(now))) return clockFmt.format(date);
		if (dayBefore(new Date(now), date)) return `вчера ${clockFmt.format(date)}`;
		return `${shortDayFmt.format(date)} ${clockFmt.format(date)}`;
	},
	/** Дата и время: «28 октября, 15:00», с годом, если он не текущий. */
	stamp(ts: number, now: number): string {
		return `${fmt.date(ts, now)}, ${clockFmt.format(ts)}`;
	},
	/** Диапазон дат ISO: «24–30 октября» или «28 октября – 2 ноября». */
	dayRange(from: string | undefined, to: string | undefined): string {
		if (!from || !to) return from ?? to ?? '';
		const [first, last] = [new Date(`${from}T12:00:00`), new Date(`${to}T12:00:00`)];
		if (first.getMonth() === last.getMonth()) return `${first.getDate()}–${dayFmt.format(last)}`;
		return `${dayFmt.format(first)} – ${dayFmt.format(last)}`;
	},
	dayLabel(ts: number, now: number): string {
		const date = new Date(ts);
		const today = new Date(now);
		const label = dayFmt.format(date);
		if (sameDay(date, today)) return `Сегодня, ${label}`;
		if (dayBefore(today, date)) return `Вчера, ${label}`;
		return label;
	},
	resetAt(ts: number, now: number): string {
		if (!Number.isFinite(ts) || !Number.isFinite(now)) return '';
		const date = new Date(ts);
		const weekday = capitalize(weekdayFmt.format(date).replace('.', ''));
		return `${weekday}, ${dayFmt.format(date)} · ${clockFmt.format(date)}`;
	},
	pct(permille: number): string {
		const value = permille / 10;
		const rounded = Number.isInteger(value) ? String(value) : value.toFixed(1).replace('.', ',');
		return `${rounded}%`;
	},
	pctApprox(permille: number): string {
		const value = permille / 10;
		return `≈ ${value.toFixed(1).replace('.', ',')}%`;
	},
	tokens(n: number): string {
		return grouped(n);
	},
	/** Число в русской записи: «50», «0,5». */
	number(value: number): string {
		return grouped(value);
	},
	/** Скорость генерации: «142 ток/с», медленнее десяти — с десятыми. */
	tokensPerSecond(value: number): string {
		const text = value < 10 ? value.toFixed(1).replace('.', ',') : grouped(Math.round(value));
		return `${text} ток/с`;
	},
	/** Время до первого токена: «1,8 с»; от минуты — как длительность. */
	firstToken(ms: number): string {
		if (ms >= 60_000) return fmt.duration(ms);
		if (ms < 50) return '< 0,1 с';
		const seconds = ms / 1000;
		return `${seconds < 10 ? seconds.toFixed(1).replace('.', ',') : Math.round(seconds)} с`;
	},
	/** «генерация 142 ток/с · первый токен 1,8 с»; неизвестная часть опускается. */
	speed(tokensPerSecond: number | null | undefined, firstTokenMs: number | null | undefined): string {
		const parts: string[] = [];
		if (tokensPerSecond != null) parts.push(`генерация ${fmt.tokensPerSecond(tokensPerSecond)}`);
		if (firstTokenMs != null) parts.push(`первый токен ${fmt.firstToken(firstTokenMs)}`);
		return parts.join(' · ');
	},
	/** Коротко для строк ленты: «980 тыс.», «1 млн», «640». */
	tokensShort(n: number): string {
		const short = (value: number, unit: string): string => {
			const text = Number.isInteger(value) ? String(value) : value.toFixed(1).replace('.', ',');
			return `${text} ${unit}`;
		};
		if (n >= 1_000_000) return short(Math.round(n / 100_000) / 10, 'млн');
		if (n >= 1_000) return short(Math.round(n / 100) / 10, 'тыс.');
		return grouped(n);
	},
	bytes(n: number): string {
		const units = ['Б', 'КиБ', 'МиБ', 'ГиБ'];
		let value = n;
		let unit = 0;
		while (value >= 1024 && unit < units.length - 1) {
			value /= 1024;
			unit += 1;
		}
		const text = Number.isInteger(value) ? String(value) : value.toFixed(1).replace('.', ',');
		return `${text} ${units[unit]}`;
	},
	/** Свободное место в десятичных гигабайтах: «24 ГБ». */
	gb(n: number): string {
		return `${grouped(Math.round(n / 1_000_000_000))} ГБ`;
	},
	plural(n: number, forms: [string, string, string]): string {
		const category = rules.select(n);
		if (category === 'one') return forms[0];
		if (category === 'few') return forms[1];
		return forms[2];
	},
	ago(ts: number, now: number): string {
		const delta = Math.max(0, now - ts);
		const seconds = Math.floor(delta / 1000);
		if (seconds < 1) return 'только что';
		if (seconds < 60) return `${seconds} ${fmt.plural(seconds, ['секунду', 'секунды', 'секунд'])} назад`;
		const minutes = Math.floor(seconds / 60);
		if (minutes < 60) return `${minutes} ${fmt.plural(minutes, ['минуту', 'минуты', 'минут'])} назад`;
		const hours = Math.floor(minutes / 60);
		if (hours < 24) return `${hours} ${fmt.plural(hours, ['час', 'часа', 'часов'])} назад`;
		const days = Math.floor(hours / 24);
		return `${days} ${fmt.plural(days, ['день', 'дня', 'дней'])} назад`;
	}
};
