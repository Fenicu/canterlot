import { SvelteDate } from 'svelte/reactivity';
import type { ChatStore } from '$lib/stores/chat.svelte';
import type { OpJournal, Op } from '$lib/ops/journal';
import type { DraftStore } from '$lib/drafts/store';
import type { UploadManager } from '$lib/uploads/manager';
import type { ConnectionStatus } from '$lib/sync/connection';
import type { LimitsView } from '$lib/api/config';
import type { AttachmentRef } from '$lib/stores/inputs';
import { freshInput } from '$lib/stores/inputs';
import { utf8Length } from '$lib/stores/blocks';
import { UploadLimitError } from '$lib/uploads/uploader';
import { validateScheduleTime } from '$lib/composer/schedule';

export type SendGate = { ok: true } | { ok: false; reason: string };

export type Limits = LimitsView | {
	max_attachment_bytes?: number;
	max_attachments_per_input?: number;
	max_input_text_bytes?: number;
	queue_limit?: number;
	upload_chunk_bytes?: number;
};

export interface ComposerDeps {
	conversationId: string;
	chat: ChatStore;
	ops: OpJournal;
	drafts: DraftStore;
	uploads: UploadManager;
	config(): Limits;
	canSend(): Promise<'ok' | 'offline'>;
	connection(): ConnectionStatus;
	probe?: { mark(name: string): void };
	serverNow?(): number;
	interruptApi?(conversationId: string, key: string): Promise<unknown>;
}

export class ComposerModel {
	readonly deps: ComposerDeps;
	text = $state('');
	errorMessage = $state<string | null>(null);
	interruptLabel = $state('Прервать');
	interruptRunning = $state(false);

	#lastSendAt = 0;
	#lastSendText = '';

	constructor(deps: ComposerDeps) {
		this.deps = deps;
	}

	get gate(): SendGate {
		const trimmed = this.text.trim();
		const uploadsList = this.deps.uploads.list(this.deps.conversationId);
		const activeUploads = uploadsList.filter((u) => u.phase !== 'cancelled' && u.phase !== 'failed');

		// 1. Пустой ввод без вложений
		if (trimmed.length === 0 && activeUploads.length === 0) {
			return { ok: false, reason: 'Введите сообщение или прикрепите файл' };
		}

		// 2. Вложения ещё загружаются
		const pending = activeUploads.filter((u) => u.phase !== 'ready');
		if (pending.length > 0) {
			return {
				ok: false,
				reason: `Файлы ещё загружаются (${pending.length} из ${activeUploads.length})`
			};
		}

		// 3. Текст превышает лимит в 256 КиБ (проверяется UTF-8 только если длина * 3 может превысить лимит)
		const maxBytes = this.deps.config()?.max_input_text_bytes ?? 262_144;
		if (this.text.length * 3 > maxBytes) {
			if (utf8Length(this.text) > maxBytes) {
				return { ok: false, reason: 'Текст длиннее 256 КиБ' };
			}
		}

		// 4. Очередь заполнена
		const queueLimit = this.deps.config()?.queue_limit ?? 100;
		if (this.deps.chat.queue.queued >= queueLimit) {
			return {
				ok: false,
				reason: `Очередь заполнена (${queueLimit}): дождитесь выполнения`
			};
		}

		return { ok: true };
	}

	async send(): Promise<void> {
		if (!this.gate.ok) return;

		// Защита от двойного нажатия: идентичное нажатие в пределах 600 мс игнорируется
		const now = Date.now();
		if (now - this.#lastSendAt < 600 && this.text === this.#lastSendText) {
			return;
		}
		this.#lastSendAt = now;
		this.#lastSendText = this.text;

		// Проверка доступности сети (HTTP-проверка, не зависит от WSS)
		const canSend = await this.deps.canSend();
		if (canSend === 'offline') {
			this.errorMessage = 'Нет связи: сообщение сохранено как черновик и не отправлено';
			this.deps.drafts.set(this.deps.conversationId, { text: this.text });
			return;
		}

		const sendText = this.text;
		const readyUploads = this.deps.uploads
			.list(this.deps.conversationId)
			.filter((u) => u.phase === 'ready');
		const attachmentIds = readyUploads.map((u) => u.attachmentId!).filter(Boolean);
		const attachmentRefs: AttachmentRef[] = readyUploads.map((u) => ({
			attachment_id: u.attachmentId ?? u.id,
			name: u.name,
			mime: 'application/octet-stream',
			size: u.size,
			sha256: '',
			image: false
		}));

		const key = crypto.randomUUID();
		const createdAt = this.deps.serverNow ? this.deps.serverNow() : Date.now();

		// Шаг 1: локальный элемент в полосе (strip) в том же кадре
		this.deps.probe?.mark('send_pressed');
		const localInput = freshInput(key, sendText, createdAt, attachmentRefs);
		this.deps.chat.inputs.set(key, localInput);
		this.deps.probe?.mark('input_painted');

		// Шаг 2: сохранение операции в журнал (IndexedDB)
		let op: Op<{ content: Array<{ type: 'text'; text: string }>; attachment_ids?: string[] }>;
		try {
			const body = {
				content: [{ type: 'text' as const, text: sendText }],
				attachment_ids: attachmentIds.length > 0 ? attachmentIds : undefined
			};
			op = await this.deps.ops.begin('input', body, {
				conversationId: this.deps.conversationId,
				persist: true,
				key
			});
		} catch (err) {
			this.deps.chat.inputs.delete(key);
			this.text = sendText;
			this.errorMessage = 'Не удалось сохранить на устройстве';
			throw err;
		}

		// Шаг 3: очистка поля ввода и черновика
		this.text = '';
		this.errorMessage = null;
		await this.deps.drafts.clear(this.deps.conversationId);

		// Шаг 4: запуск операции
		try {
			await this.deps.ops.run(op);
		} catch (err) {
			if (op.state === 'refused') {
				this.text = sendText;
				this.deps.drafts.set(this.deps.conversationId, { text: sendText });
				this.deps.chat.inputs.delete(key);
				this.errorMessage = op.refusal?.message ?? (err instanceof Error ? err.message : 'Отклонено');
				return;
			}
		}

		if (op.state === 'refused') {
			this.text = sendText;
			this.deps.drafts.set(this.deps.conversationId, { text: sendText });
			this.deps.chat.inputs.delete(key);
			this.errorMessage = op.refusal?.message ?? 'Отклонено';
		}
	}

	async sendLater(at: number): Promise<void> {
		const now = this.deps.serverNow ? this.deps.serverNow() : Date.now();
		const valid = validateScheduleTime(at, now);
		if (valid !== 'ok') {
			this.errorMessage = valid === 'too_soon' ? 'Слишком близкое время отправки' : 'Слишком далёкое время отправки';
			return;
		}

		if (!this.gate.ok) return;

		const canSend = await this.deps.canSend();
		if (canSend === 'offline') {
			this.errorMessage = 'Нет связи: сообщение сохранено как черновик и не отправлено';
			this.deps.drafts.set(this.deps.conversationId, { text: this.text });
			return;
		}

		const sendText = this.text;
		const readyUploads = this.deps.uploads
			.list(this.deps.conversationId)
			.filter((u) => u.phase === 'ready');
		const attachmentIds = readyUploads.map((u) => u.attachmentId!).filter(Boolean);

		const op = await this.deps.ops.begin(
			'scheduled',
			{
				content: [{ type: 'text' as const, text: sendText }],
				attachment_ids: attachmentIds.length > 0 ? attachmentIds : undefined,
				run_at: new SvelteDate(at).toISOString()
			},
			{ conversationId: this.deps.conversationId, persist: true }
		);

		this.text = '';
		this.errorMessage = null;
		await this.deps.drafts.clear(this.deps.conversationId);

		void this.deps.ops.run(op);
	}

	attach(files: FileList | File[]): void {
		const list = this.deps.uploads.list(this.deps.conversationId);
		let count = list.filter((u) => u.phase !== 'cancelled' && u.phase !== 'failed').length;
		const fileArray = Array.from(files);

		for (const file of fileArray) {
			try {
				this.deps.uploads.add(this.deps.conversationId, file, count);
				count += 1;
			} catch (err) {
				if (err instanceof UploadLimitError) {
					if (err.reason === 'empty') this.errorMessage = 'Файл пустой';
					else if (err.reason === 'too_large') this.errorMessage = 'Файл больше 100 МиБ';
					else if (err.reason === 'too_many') this.errorMessage = 'Не более 10 файлов в одном сообщении';
				}
			}
		}
	}

	async interrupt(): Promise<void> {
		if (this.interruptRunning) return;
		this.interruptRunning = true;
		this.interruptLabel = 'Прерываю…';

		try {
			const key = crypto.randomUUID();
			if (this.deps.interruptApi) {
				await this.deps.interruptApi(this.deps.conversationId, key);
			} else {
				const { interrupt } = await import('$lib/api/client');
				await interrupt(this.deps.conversationId, key);
			}
		} catch {
			this.interruptLabel = 'Прервать';
		} finally {
			this.interruptRunning = false;
		}
	}
}
