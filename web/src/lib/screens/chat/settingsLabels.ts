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
export const permissionLabel = (value: string): string => PERMISSION_LABELS[value] ?? value;
