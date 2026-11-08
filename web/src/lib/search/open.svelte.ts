import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { pane } from '#lib/shell/pane.svelte.js';
import { emptyState, type SearchState, type SessionScope } from './model';

/**
 * Просьба открыть поиск в панели стола: панель забирает начальное состояние
 * по номеру, чтобы повторная просьба с тем же запросом тоже сработала.
 */
class SearchRequest {
	seed = $state<SearchState | null>(null);
	nonce = $state(0);

	open(state: SearchState): void {
		this.seed = state;
		this.nonce += 1;
		pane.open('search');
	}
}

export const searchRequest = new SearchRequest();

/**
 * Открыть поиск: на столе — третья панель, иначе экран `/search`.
 * Запрос едет в `history.state`, а не в адресе.
 */
export function openSearch(q: string, session: SessionScope | null = null): void {
	const state: SearchState = { ...emptyState(session), q };
	if (pane.desktop) {
		searchRequest.open(state);
		return;
	}
	void goto(resolve('/search'), { state: { search: state } });
}
