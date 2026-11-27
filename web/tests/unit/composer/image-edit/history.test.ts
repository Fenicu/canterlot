import { describe, expect, it } from 'vitest';
import { addStroke, createEdit, setCrop, undo } from '#lib/composer/imageEdit/model.js';

const bounds = { width: 400, height: 200 };
const full = { x: 0, y: 0, width: 400, height: 200 };
const stroke = {
	color: 'red' as const,
	points: [
		{ x: 10, y: 20 },
		{ x: 30, y: 40 }
	]
};

describe('image edit undo stack', () => {
	it('starts with the whole image as the crop and an empty history', () => {
		expect(createEdit(bounds)).toEqual({ crop: full, strokes: [], history: [] });
	});

	it('commits a clamped crop as one undoable action', () => {
		const edited = setCrop(createEdit(bounds), { x: -10, y: 0, width: 100, height: 80 }, bounds);
		expect(edited.crop).toEqual({ x: 0, y: 0, width: 100, height: 80 });
		expect(undo(edited).crop).toEqual(full);
	});

	it('does not add an action when the crop did not change', () => {
		const edited = setCrop(createEdit(bounds), full, bounds);
		expect(edited.history).toEqual([]);
	});

	it('undoes the last stroke but keeps earlier strokes and the crop', () => {
		const first = addStroke(createEdit(bounds), stroke);
		const cropped = setCrop(first, { x: 10, y: 10, width: 100, height: 100 }, bounds);
		const second = addStroke(cropped, { ...stroke, color: 'blue' });
		const undone = undo(second);
		expect(undone.strokes).toEqual([stroke]);
		expect(undone.crop).toEqual(cropped.crop);
		const twice = undo(undone);
		expect(twice.crop).toEqual(full);
		expect(twice.strokes).toEqual([stroke]);
		expect(undo(twice).strokes).toEqual([]);
	});

	it('does nothing when there is nothing to undo', () => {
		const empty = createEdit(bounds);
		expect(undo(empty)).toEqual(empty);
	});
});
