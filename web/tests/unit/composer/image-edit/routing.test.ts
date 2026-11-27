import { describe, expect, it } from 'vitest';
import { MAX_IMAGE_PIXELS, routeFiles } from '#lib/composer/imageEdit/model.js';

function file(name: string, type: string): File {
	return new File(['mock'], name, { type });
}

describe('image edit routing', () => {
	it('sends only PNG, JPEG and WebP to the editor', () => {
		const png = file('a.png', 'image/png');
		const jpeg = file('b.jpg', 'image/jpeg');
		const webp = file('c.webp', 'image/webp');
		const gif = file('d.gif', 'image/gif');
		const svg = file('e.svg', 'image/svg+xml');
		const doc = file('f.pdf', 'application/pdf');
		const sizes = new Map<File, { width: number; height: number }>([
			[png, { width: 100, height: 100 }],
			[jpeg, { width: 100, height: 100 }],
			[webp, { width: 100, height: 100 }]
		]);
		expect(routeFiles([png, gif, jpeg, svg, webp, doc], sizes)).toEqual({
			edit: [png, jpeg, webp],
			direct: [gif, svg, doc],
			skipped: []
		});
	});

	it('attaches an undecodable image as is and explains why', () => {
		const broken = file('broken.png', 'image/png');
		expect(routeFiles([broken], new Map())).toEqual({
			edit: [],
			direct: [broken],
			skipped: [{ file: broken, reason: 'decode' }]
		});
	});

	it('attaches a huge image as is when it exceeds the canvas pixel limit', () => {
		const huge = file('huge.png', 'image/png');
		const edge = file('edge.png', 'image/png');
		const sizes = new Map<File, { width: number; height: number }>([
			[huge, { width: 4096, height: 4097 }],
			[edge, { width: 4096, height: 4096 }]
		]);
		expect(MAX_IMAGE_PIXELS).toBe(16_777_216);
		expect(routeFiles([huge, edge], sizes)).toEqual({
			edit: [edge],
			direct: [huge],
			skipped: [{ file: huge, reason: 'too_large' }]
		});
	});
});
