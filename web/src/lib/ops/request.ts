import type { Op } from '$lib/ops/journal';

/** Куда журнал кладёт операцию и каким ключом её повторяет после перезагрузки. */
export function opRequest(op: Pick<Op, 'kind' | 'key' | 'conversationId' | 'body'>): {
	url: string;
	init: RequestInit;
} {
	const conversation = op.conversationId ?? '_';
	const url =
		op.kind === 'scheduled'
			? `/api/v1/conversations/${conversation}/scheduled`
			: `/api/v1/conversations/${conversation}/inputs`;
	return {
		url,
		init: {
			method: 'POST',
			headers: {
				'content-type': 'application/json',
				'Idempotency-Key': op.key,
				'X-Canterlot-Client': '1'
			},
			body: JSON.stringify(op.body ?? {})
		}
	};
}
