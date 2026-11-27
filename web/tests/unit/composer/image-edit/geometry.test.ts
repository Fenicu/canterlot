import { describe, expect, it } from 'vitest';
import {
	clampRect,
	fitImage,
	moveRect,
	resizeRect,
	strokeWidth,
	toImagePoint,
	toScreenPoint
} from '#lib/composer/imageEdit/model.js';

const bounds = { width: 400, height: 200 };

describe('image edit geometry', () => {
	it('fits the whole image into the viewport without stretching', () => {
		expect(fitImage({ width: 400, height: 200 }, { width: 200, height: 200 })).toEqual({
			x: 0,
			y: 50,
			width: 200,
			height: 100
		});
	});

	it('maps a screen point to image coordinates and clamps outside the image', () => {
		const view = fitImage({ width: 400, height: 200 }, { width: 200, height: 200 });
		expect(toImagePoint({ x: 100, y: 100 }, view, bounds)).toEqual({ x: 200, y: 100 });
		expect(toImagePoint({ x: 0, y: 0 }, view, bounds)).toEqual({ x: 0, y: 0 });
		expect(toImagePoint({ x: 200, y: 200 }, view, bounds)).toEqual({ x: 400, y: 200 });
	});

	it('maps an image point back to screen coordinates', () => {
		const view = fitImage({ width: 400, height: 200 }, { width: 200, height: 200 });
		expect(toScreenPoint({ x: 200, y: 100 }, view, bounds)).toEqual({ x: 100, y: 100 });
	});

	it('clamps a crop frame inside the image, shrinking oversized frames', () => {
		expect(clampRect({ x: -10, y: 190, width: 100, height: 100 }, bounds)).toEqual({
			x: 0,
			y: 100,
			width: 100,
			height: 100
		});
		expect(clampRect({ x: 10, y: 10, width: 500, height: 500 }, bounds)).toEqual({
			x: 0,
			y: 0,
			width: 400,
			height: 200
		});
	});

	it('keeps a dragged frame inside the image', () => {
		expect(moveRect({ x: 0, y: 0, width: 100, height: 100 }, -50, -50, bounds)).toEqual({
			x: 0,
			y: 0,
			width: 100,
			height: 100
		});
		expect(moveRect({ x: 0, y: 0, width: 100, height: 100 }, 350, 150, bounds)).toEqual({
			x: 300,
			y: 100,
			width: 100,
			height: 100
		});
	});

	it('resizes from corners and clamps to the image and a minimum size', () => {
		const rect = { x: 10, y: 10, width: 100, height: 100 };
		expect(resizeRect(rect, 'se', 20, 30, bounds)).toEqual({
			x: 10,
			y: 10,
			width: 120,
			height: 130
		});
		expect(resizeRect(rect, 'nw', 20, 30, bounds)).toEqual({
			x: 30,
			y: 40,
			width: 80,
			height: 70
		});
		expect(resizeRect({ x: 350, y: 150, width: 50, height: 50 }, 'se', 500, 500, bounds)).toEqual({
			x: 350,
			y: 150,
			width: 50,
			height: 50
		});
		expect(resizeRect({ x: 10, y: 10, width: 20, height: 20 }, 'nw', 500, 500, bounds, 10)).toEqual({
			x: 20,
			y: 20,
			width: 10,
			height: 10
		});
	});

	it('scales the stroke width with the smaller image side but never below two pixels', () => {
		expect(strokeWidth({ width: 1000, height: 500 })).toBe(3);
		expect(strokeWidth({ width: 100, height: 100 })).toBe(2);
	});
});
