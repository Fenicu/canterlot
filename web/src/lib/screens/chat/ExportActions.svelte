<script lang="ts">
	import { errorText } from '#lib/api/errors.js';
	import { onMount } from 'svelte';
	import type { BlockModel } from '#lib/stores/blocks.js';
	import { answerToMarkdown } from '#lib/export/markdown.js';
	import { renderAnswerPages } from '#lib/export/image.js';
	import { deliverImage, type Delivery } from '#lib/export/deliver.js';

	let {
		blocks = [],
		answerElement = null
	}: {
		blocks?: BlockModel[];
		answerElement?: HTMLElement | null;
	} = $props();

	let canShareFiles = $state(false);
	let copyingMarkdown = $state(false);
	let markdownFallback = $state<string | null>(null);
	let copyStatus = $state<string | null>(null);

	let exportingImage = $state(false);
	let readyPages = $state<Blob[] | null>(null);
	let imageStatus = $state<string | null>(null);

	onMount(() => {
		if (typeof navigator !== 'undefined' && typeof navigator.canShare === 'function') {
			try {
				const testFile = new File([''], 'test.png', { type: 'image/png' });
				canShareFiles = Boolean(navigator.canShare({ files: [testFile] }));
			} catch {
				canShareFiles = false;
			}
		}
	});

	function findAnswerCard(items: BlockModel[]): HTMLElement | null {
		for (const item of items) {
			const card = document.querySelector(`[data-block="${item.id}"]`)?.closest('article.card');
			if (card instanceof HTMLElement) return card;
		}
		return null;
	}

	function downloadFile(blob: Blob, name: string): void {
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = name;
		document.body.appendChild(a);
		a.click();
		document.body.removeChild(a);
		setTimeout(() => URL.revokeObjectURL(url), 1000);
	}

	async function handleCopyMarkdown() {
		if (copyingMarkdown) return;
		copyingMarkdown = true;
		markdownFallback = null;
		copyStatus = null;

		const md = answerToMarkdown(blocks);
		try {
			if (!navigator.clipboard || !navigator.clipboard.writeText) {
				throw new Error('Clipboard API unavailable');
			}
			await navigator.clipboard.writeText(md);
			copyStatus = 'Markdown скопирован';
		} catch {
			markdownFallback = md;
		} finally {
			copyingMarkdown = false;
		}
	}

	async function handleExportImage() {
		if (exportingImage) return;
		exportingImage = true;
		imageStatus = null;
		readyPages = null;

		try {
			// Find element to render
			// Карточка ответа из ленты даёт картинку как на экране; если окно ленты её уже убрало, собираем текст блоков.
			let target = answerElement ?? findAnswerCard(blocks);
			let createdTemp = false;

			if (!target) {
				target = document.createElement('div');
				createdTemp = true;
				for (const b of blocks) {
					const text = b.text || b.summary || '';
					if (!text) continue;
					const div = document.createElement('div');
					div.className = 'export-block';
					div.textContent = text;
					target.appendChild(div);
				}
				document.body.appendChild(target);
			}

			const pages = await renderAnswerPages(target);
			if (createdTemp && target.parentNode) {
				target.parentNode.removeChild(target);
			}

			const outcome = await deliverImage(pages, {
				share: navigator.share ? (data) => navigator.share(data) : undefined,
				canShare: navigator.canShare ? (data) => navigator.canShare(data) : undefined,
				clipboard: navigator.clipboard,
				download: downloadFile
			});

			handleDeliveryOutcome(outcome, pages);
		} catch (err: unknown) {
			imageStatus = errorText(err, 'Не удалось экспортировать изображение');
		} finally {
			exportingImage = false;
		}
	}

	async function handleRetryGestureShare() {
		if (!readyPages || readyPages.length === 0) return;
		try {
			const outcome = await deliverImage(readyPages, {
				share: navigator.share ? (data) => navigator.share(data) : undefined,
				canShare: navigator.canShare ? (data) => navigator.canShare(data) : undefined,
				clipboard: navigator.clipboard,
				download: downloadFile
			});
			handleDeliveryOutcome(outcome, readyPages);
		} catch {
			// fallback to download
			readyPages.forEach((p, idx) => {
				downloadFile(p, `canterlot-${idx + 1}.png`);
			});
			readyPages = null;
			imageStatus = 'Изображение сохранено';
		}
	}

	function handleDeliveryOutcome(outcome: Delivery, pages: Blob[]) {
		if (outcome === 'needs_gesture') {
			readyPages = pages;
			imageStatus = 'Требуется подтверждение';
		} else if (outcome === 'copied') {
			readyPages = null;
			imageStatus = 'Изображение скопировано в буфер обмена';
		} else if (outcome === 'shared') {
			readyPages = null;
			imageStatus = 'Изображение отправлено';
		} else if (outcome === 'downloaded') {
			readyPages = null;
			if (pages.length > 1) {
				imageStatus = `Изображение длинное: сохранено ${pages.length} файлов`;
			} else {
				imageStatus = 'Изображение сохранено';
			}
		}
	}
</script>

<div class="copy-actions">
	<button
		type="button"
		class="button"
		onclick={handleCopyMarkdown}
		aria-busy={copyingMarkdown}
	>
		Копировать Markdown
	</button>

	{#if readyPages}
		<button
			type="button"
			class="button primary gesture-retry"
			onclick={handleRetryGestureShare}
		>
			Поделиться
		</button>
	{:else}
		<button
			type="button"
			class="button"
			onclick={handleExportImage}
			aria-busy={exportingImage}
		>
			{canShareFiles ? 'Поделиться картинкой' : 'Копировать картинкой'}
		</button>
	{/if}
</div>

{#if copyStatus}
	<p class="status-msg" role="status">{copyStatus}</p>
{/if}

{#if imageStatus}
	<p class="status-msg" role="status">{imageStatus}</p>
{/if}

{#if markdownFallback !== null}
	<div class="fallback-box">
		<label for="markdown-fallback" class="fallback-label">Не удалось скопировать автоматически</label>
		<textarea
			id="markdown-fallback"
			class="fallback-text"
			rows="4"
			readonly
			onfocus={(e) => e.currentTarget.select()}
		>{markdownFallback}</textarea>
	</div>
{/if}

<style>
	.copy-actions {
		display: flex;
		gap: var(--s2);
		margin-top: var(--s4);
		flex-wrap: wrap;
	}
	.button {
		padding: var(--s2) var(--s4);
		border-radius: var(--radius-card);
		border: var(--s0);
		background: var(--raised);
		color: var(--text);
		font-size: var(--fs-body);
		cursor: pointer;
		min-height: var(--target);
	}
	.button:hover:not(:disabled) {
		background: var(--raised);
	}
	.button.primary {
		background: var(--accent);
		color: var(--accent-ink);
	}
	.status-msg {
		margin: var(--s2) 0 0;
		font-size: var(--fs-meta);
		color: var(--accent);
	}
	.fallback-box {
		margin-top: var(--s2);
		display: flex;
		flex-direction: column;
		gap: var(--s1);
	}
	.fallback-label {
		font-size: var(--fs-meta);
		color: var(--danger);
	}
	.fallback-text {
		width: 100%;
		box-sizing: border-box;
		padding: var(--s2);
		font-family: var(--font-mono);
		font-size: var(--fs-meta);
		background: var(--raised);
		color: var(--text);
		border: var(--s0);
		border-radius: var(--radius-card);
	}
</style>
