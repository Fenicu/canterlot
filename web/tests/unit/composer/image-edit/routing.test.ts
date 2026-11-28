import { describe, expect, it } from 'vitest';
import { EDITABLE_MIMES, MAX_IMAGE_PIXELS, editableSize } from '#lib/composer/imageEdit/model.js';

describe('image edit limits', () => {
	it('edits only PNG, JPEG and WebP', () => {
		expect([...EDITABLE_MIMES].sort()).toEqual(['image/jpeg', 'image/png', 'image/webp']);
	});

	it('refuses an image above the canvas pixel limit and an empty size', () => {
		expect(MAX_IMAGE_PIXELS).toBe(16_777_216);
		expect(editableSize({ width: 4096, height: 4096 })).toBe(true);
		expect(editableSize({ width: 4096, height: 4097 })).toBe(false);
		expect(editableSize({ width: 0, height: 100 })).toBe(false);
	});
});
