const rules = new Intl.PluralRules('ru');
const clockFmt = new Intl.DateTimeFormat('ru-RU', { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const dayFmt = new Intl.DateTimeFormat('ru-RU', { day: 'numeric', month: 'long' });
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
	clock(ts: number): string {
		return clockFmt.format(ts);
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
