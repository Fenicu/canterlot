export type Delivery = 'shared' | 'copied' | 'downloaded' | 'needs_gesture';

function padZero(n: number): string {
	return String(n).padStart(2, '0');
}

export function generateFilename(date = new Date(), pageIndex?: number): string {
	const y = date.getFullYear();
	const m = padZero(date.getMonth() + 1);
	const d = padZero(date.getDate());
	const hh = padZero(date.getHours());
	const mm = padZero(date.getMinutes());
	const pageSuffix = pageIndex != null ? `-${pageIndex + 1}` : '';
	return `canterlot-${y}-${m}-${d}-${hh}${mm}${pageSuffix}.png`;
}

export async function deliverImage(
	pages: Blob[],
	env: {
		share?: Navigator['share'];
		canShare?: Navigator['canShare'];
		clipboard?: Clipboard;
		download(blob: Blob, name: string): void;
	}
): Promise<Delivery> {
	if (pages.length === 1 && pages[0]) {
		const page = pages[0];
		const file = new File([page], generateFilename(), { type: 'image/png' });

		if (env.canShare && env.canShare({ files: [file] }) && env.share) {
			try {
				await env.share({ files: [file] });
				return 'shared';
			} catch (err: unknown) {
				if (err && typeof err === 'object' && (err as { name?: string }).name === 'NotAllowedError') {
					return 'needs_gesture';
				}
			}
		}

		if (env.clipboard && typeof env.clipboard.write === 'function') {
			try {
				const item =
					typeof ClipboardItem !== 'undefined'
						? new ClipboardItem({ 'image/png': page })
						: (page as unknown as ClipboardItem);
				await env.clipboard.write([item]);
				return 'copied';
			} catch {
				// fall through to download
			}
		}
	}

	const now = new Date();
	pages.forEach((page, idx) => {
		const name = generateFilename(now, pages.length > 1 ? idx : undefined);
		env.download(page, name);
	});
	return 'downloaded';
}
