import { searchHistory } from '#lib/api/client.js';
import type { SearchBody, SearchPage } from './model';

export async function liveSearch(body: SearchBody, signal: AbortSignal): Promise<SearchPage> {
	const response = await searchHistory(body, { signal });
	return response.data;
}
