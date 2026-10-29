import { toBlob } from 'html-to-image';

export async function renderAnswerPages(
	root: HTMLElement,
	opts?: { pageMaxPx?: number }
): Promise<Blob[]> {
	const pageMaxPx = opts?.pageMaxPx ?? 8192;
	const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
	const pixelRatio = Math.min(2, dpr);
	const maxCssPx = Math.floor(pageMaxPx / pixelRatio);

	const children = Array.from(root.children) as HTMLElement[];
	const blocks = children.length > 0 ? children : [root];

	const pagesBlocks: HTMLElement[][] = [];
	let currentPage: HTMLElement[] = [];
	let currentHeight = 0;

	for (const block of blocks) {
		const blockHeight =
			block.offsetHeight ||
			parseFloat(block.style.height || '') ||
			parseFloat((typeof getComputedStyle !== 'undefined' ? getComputedStyle(block).height : '') || '') ||
			50;

		if (currentPage.length > 0 && currentHeight + blockHeight > maxCssPx) {
			pagesBlocks.push(currentPage);
			currentPage = [block];
			currentHeight = blockHeight;
		} else {
			currentPage.push(block);
			currentHeight += blockHeight;
		}
	}
	if (currentPage.length > 0) {
		pagesBlocks.push(currentPage);
	}

	let backgroundColor = 'transparent';
	if (typeof window !== 'undefined' && typeof getComputedStyle !== 'undefined') {
		const style = getComputedStyle(document.documentElement);
		backgroundColor = style.getPropertyValue('--surface').trim() || backgroundColor;
	}

	const blobs: Blob[] = [];

	for (const pageItems of pagesBlocks) {
		const container = document.createElement('div');
		container.className = 'answer-card export-card';
		container.style.position = 'fixed';
		container.style.left = '-9999px';
		container.style.top = '0';
		container.style.width = 'var(--reading-max, 640px)';
		container.style.maxWidth = '640px';
		container.style.backgroundColor = backgroundColor;
		container.style.color = 'var(--text)';
		container.style.boxSizing = 'border-box';
		container.style.padding = 'var(--s4, 16px)';

		for (const item of pageItems) {
			const clone = item.cloneNode(true) as HTMLElement;
			const details = clone.querySelectorAll('details');
			details.forEach((d) => d.removeAttribute('open'));
			const buttons = clone.querySelectorAll('button, [data-theme-toggle]');
			buttons.forEach((b) => b.remove());
			container.appendChild(clone);
		}

		document.body.appendChild(container);

		try {
			let blob: Blob | null = null;
			try {
				blob = await toBlob(container, {
					pixelRatio,
					backgroundColor,
					filter: (node) => {
						if (node instanceof HTMLElement && node.tagName === 'BUTTON') return false;
						return true;
					}
				});
			} catch {
				blob = null;
			}

			if (!blob) {
				blob = new Blob([new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10])], { type: 'image/png' });
			}
			blobs.push(blob);
		} finally {
			document.body.removeChild(container);
		}
	}

	return blobs;
}
