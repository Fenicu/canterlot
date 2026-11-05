import { describe, expect, it } from 'vitest';
import type { InputOrigin } from '#lib/stores/inputs.js';
import {
	firstLine,
	messagePreview,
	ownerMessageOrigin,
	rowIndexOfInput,
	sortOwnerMessages
} from '#lib/timeline/ownerMessages.js';

const message = (seq: string) => ({ seq, id: seq });

describe('timeline/ownerMessages', () => {
	it('takes the first non-empty trimmed line', () => {
		expect(firstLine('\n \n  Первая строка  \nвторая')).toBe('Первая строка');
		expect(firstLine('\t\n')).toBe('');
	});

	it('uses attachment wording when the message has no text', () => {
		expect(messagePreview('  \n ', 1)).toBe('Вложение');
		expect(messagePreview('', 0)).toBe('Без текста');
		expect(messagePreview('\n привет\n', 0)).toBe('привет');
	});

	it('keeps messages chronological regardless of page order', () => {
		const items = [message('9'), message('3'), message('12')];
		expect(sortOwnerMessages(items).map((item) => item.seq)).toEqual(['3', '9', '12']);
	});

	it('labels non-owner origins quietly', () => {
		expect(ownerMessageOrigin(null)).toBeNull();
		expect(ownerMessageOrigin({ kind: 'message', from: 'a', title: 'Оркестратор', model: 'sonnet-5', chain_depth: 1 } as InputOrigin)).toBe(
			'От сессии Оркестратор · модель sonnet-5'
		);
		expect(ownerMessageOrigin({ kind: 'event', from: 'b', title: 'Миграция', chain_depth: 0 } as InputOrigin)).toBe(
			'Итог сессии Миграция'
		);
		expect(ownerMessageOrigin({ kind: 'alarm', chain_depth: 0 } as InputOrigin)).toBe('Будильник');
	});

	it('finds the row that holds a message', () => {
		const rows = [
			{ entries: [{ kind: 'input', input: { id: 'a' } }, { kind: 'block' }] },
			{ entries: [{ kind: 'input', input: { id: 'b' } }] }
		];
		expect(rowIndexOfInput(rows as never, 'b')).toBe(1);
		expect(rowIndexOfInput(rows as never, 'missing')).toBe(-1);
	});
});
