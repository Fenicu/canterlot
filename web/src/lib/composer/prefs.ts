export type EnterPref = 'auto' | 'send' | 'newline';

export const ENTER_PREF_KEY = 'canterlot.enter';

export function getEnterPref(): EnterPref {
	if (typeof localStorage === 'undefined') return 'auto';
	try {
		const val = localStorage.getItem(ENTER_PREF_KEY);
		if (val === 'send' || val === 'newline' || val === 'auto') {
			return val;
		}
	} catch {
		// Ошибки доступа к localStorage игнорируются
	}
	return 'auto';
}

export function setEnterPref(pref: EnterPref): void {
	if (typeof localStorage === 'undefined') return;
	try {
		localStorage.setItem(ENTER_PREF_KEY, pref);
	} catch {
		// Ошибки доступа к localStorage игнорируются
	}
}
