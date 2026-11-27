import { describe, expect, it } from 'vitest';
import { outputPlan } from '#lib/composer/imageEdit/model.js';

const image = { width: 1200, height: 800 };
const crop = { x: 100.4, y: 200.6, width: 400.4, height: 300.2 };

describe('image edit output plan', () => {
	it('keeps PNG as PNG at the original crop resolution', () => {
		expect(outputPlan('screen.png', 'image/png', image, crop)).toEqual({
			width: 400,
			height: 300,
			mime: 'image/png',
			name: 'screen.png'
		});
	});

	it('keeps JPEG with its quality and name', () => {
		expect(outputPlan('photo.jpeg', 'image/jpeg', image, crop)).toEqual({
			width: 400,
			height: 300,
			mime: 'image/jpeg',
			quality: 0.92,
			name: 'photo.jpeg'
		});
	});

	it('turns WebP into PNG, changing only the extension', () => {
		expect(outputPlan('shot.webp', 'image/webp', image, crop)).toEqual({
			width: 400,
			height: 300,
			mime: 'image/png',
			name: 'shot.png'
		});
		expect(outputPlan('shot', 'image/webp', image, crop).name).toBe('shot.png');
	});

	it('rounds the crop and clamps it to the image', () => {
		expect(outputPlan('a.png', 'image/png', image, { x: -5, y: -5, width: 5000, height: 5000 })).toMatchObject({
			width: 1200,
			height: 800
		});
		expect(outputPlan('a.png', 'image/png', image, { x: 1199, y: 799, width: 100, height: 100 })).toMatchObject({
			width: 1,
			height: 1
		});
	});
});
