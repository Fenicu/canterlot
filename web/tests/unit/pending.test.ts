import { describe, expect, it } from 'vitest';
import type { components } from '#lib/api/generated/openapi.js';
import { codeHint, groupCode, sortPending } from '#lib/screens/machines/pending.js';

type Pending = components['schemas']['PendingMachineView'];

const NOW = Date.parse('2026-11-27T12:00:00.000Z');

function pending(patch: Partial<Pending> = {}): Pending {
	return {
		machine_id: '00000000-0000-4000-8000-000000000001',
		name: 'workstation',
		created_at: new Date(NOW - 60_000).toISOString(),
		code_expires_at: new Date(NOW + 10 * 60_000).toISOString(),
		...patch
	};
}

describe('pending machines', () => {
	it('groups code as XXXX-XXXX…', () => {
		expect(groupCode('ABCDEFGHJKLMNPQRSTUVWXYZ23')).toBe(
			'ABCD-EFGH-JKLM-NPQR-STUV-WXYZ-23'
		);
		expect(groupCode('abcd efgh-jklm')).toBe('ABCD-EFGH-JKLM');
		expect(groupCode('')).toBe('');
	});

	it('expired pending shows «Код истёк»', () => {
		expect(codeHint(pending(), NOW)).toBe('');
		expect(
			codeHint(pending({ code_expires_at: new Date(NOW - 1).toISOString() }), NOW)
		).toBe('Код истёк');
		expect(codeHint(pending({ code_expires_at: null }), NOW)).toBe('Код обменян');
	});

	it('list sorted newest first', () => {
		const older = pending({
			machine_id: '00000000-0000-4000-8000-000000000002',
			created_at: new Date(NOW - 5 * 60_000).toISOString()
		});
		const newer = pending({
			machine_id: '00000000-0000-4000-8000-000000000003',
			created_at: new Date(NOW).toISOString()
		});
		expect(sortPending([older, newer]).map((item) => item.machine_id)).toEqual([
			newer.machine_id,
			older.machine_id
		]);
	});
});
