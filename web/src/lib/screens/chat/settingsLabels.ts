export const EFFORT_LABELS: Record<string, string> = {
	low: 'Низкое',
	medium: 'Обычное',
	high: 'Высокое',
	xhigh: 'Максимальное'
};

export const PERMISSION_LABELS: Record<string, string> = {
	default: 'По умолчанию',
	ask: 'Спрашивать',
	auto_edit: 'Автоправки',
	full_access: 'Полный доступ'
};

export const effortLabel = (value: string): string => EFFORT_LABELS[value] ?? value;

/** Строка сессии не держит каталог машины, а `opus[1m]` владельцу показывается названием продукта. */
export const modelLabel = (value: string): string => {
	const base = value.replace(/\[1m\]$/, '');
	return base === value ? value : `${base.charAt(0).toUpperCase()}${base.slice(1)} 1M`;
};
export const permissionLabel = (value: string): string => PERMISSION_LABELS[value] ?? value;
