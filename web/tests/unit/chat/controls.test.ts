import { describe, expect, it, vi } from 'vitest';
import type { HttpResponse } from '$lib/api/http';
import * as client from '$lib/api/client';
import { spendSessionReserve, type SpendOutcome } from '$lib/screens/chat/controls';

function mockResponse(data: SpendOutcome): HttpResponse<SpendOutcome> {
	return {
		status: 200,
		headers: new Headers(),
		data
	};
}

describe('chat/controls', () => {
	it('spend_reserve_outcomes_continued_already_continued_not_held', async () => {
		const cid = '00000000-0000-4000-8000-000000000001';

		// Outcome: continued
		vi.spyOn(client, 'spendReserve').mockResolvedValueOnce(mockResponse('continued'));

		const resContinued = await spendSessionReserve(cid);
		expect(resContinued.outcome).toBe('continued');
		expect(resContinued.message).toBeUndefined();

		// Outcome: already_continued
		vi.spyOn(client, 'spendReserve').mockResolvedValueOnce(mockResponse('already_continued'));

		const resAlready = await spendSessionReserve(cid);
		expect(resAlready.outcome).toBe('already_continued');
		expect(resAlready.message).toBe('Резерв уже потрачен в этой сессии');

		// Outcome: not_held
		vi.spyOn(client, 'spendReserve').mockResolvedValueOnce(mockResponse('not_held'));

		const resNotHeld = await spendSessionReserve(cid);
		expect(resNotHeld.outcome).toBe('not_held');
		expect(resNotHeld.message).toBe('Пауза уже снята');
	});
});
