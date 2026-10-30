import { SvelteDate } from "svelte/reactivity";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import type { OpJournal, Op } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import type { Upload, UploadManager } from "#lib/uploads/manager.js";
import type { ConnectionStatus } from "#lib/sync/connection.js";
import type { LimitsView } from "#lib/api/config.js";
import type { AttachmentRef } from "#lib/stores/inputs.js";
import { freshInput } from "#lib/stores/inputs.js";
import { utf8Length } from "#lib/stores/blocks.js";
import { UploadLimitError } from "#lib/uploads/uploader.js";
import { validateScheduleTime } from "#lib/composer/schedule.js";

export type SendGate = { ok: true } | { ok: false; reason: string };

export type Limits =
  | LimitsView
  | {
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
  canSend(): Promise<"ok" | "offline">;
  connection(): ConnectionStatus;
  probe?: { mark(name: string): void };
  serverNow?(): number;
  interruptApi?(conversationId: string, key: string): Promise<unknown>;
}

export class ComposerModel {
  readonly deps: ComposerDeps;
  text = $state("");
  errorMessage = $state<string | null>(null);
  interruptLabel = $state("Прервать");
  interruptRunning = $state(false);
  /** Отправка идёт: повторное нажатие во время проверки сети не создаёт второе сообщение. */
  sending = $state(false);

  constructor(deps: ComposerDeps) {
    this.deps = deps;
  }

  get gate(): SendGate {
    return this.#gateFor(this.text);
  }

  #gateFor(text: string): SendGate {
    const trimmed = text.trim();
    const uploadsList = this.deps.uploads.list(this.deps.conversationId);
    const activeUploads = uploadsList.filter(
      (u) => u.phase !== "cancelled" && u.phase !== "failed",
    );

    // 1. Пустой ввод без вложений
    if (trimmed.length === 0 && activeUploads.length === 0) {
      return { ok: false, reason: "Введите сообщение или прикрепите файл" };
    }

    // 2. Вложения ещё загружаются
    const pending = activeUploads.filter((u) => u.phase !== "ready");
    if (pending.length > 0) {
      return {
        ok: false,
        reason: `Файлы ещё загружаются (${pending.length} из ${activeUploads.length})`,
      };
    }

    // 3. Текст превышает лимит в 256 КиБ (проверяется UTF-8 только если длина * 3 может превысить лимит)
    const maxBytes = this.deps.config()?.max_input_text_bytes ?? 262_144;
    if (text.length * 3 > maxBytes) {
      if (utf8Length(text) > maxBytes) {
        return { ok: false, reason: "Текст длиннее 256 КиБ" };
      }
    }

    // 4. Очередь заполнена
    const queueLimit = this.deps.config()?.queue_limit ?? 100;
    if (this.deps.chat.queue.queued >= queueLimit) {
      return {
        ok: false,
        reason: `Очередь заполнена (${queueLimit}): дождитесь выполнения`,
      };
    }

    return { ok: true };
  }

  async send(): Promise<void> {
    if (this.sending || !this.gate.ok) return;
    this.sending = true;
    try {
      await this.#send();
    } finally {
      this.sending = false;
    }
  }

  async #send(): Promise<void> {
    const sendText = this.text;
    const readyUploads = this.deps.uploads
      .list(this.deps.conversationId)
      .filter((u) => u.phase === "ready");

    // Поле свободно с нажатия: текст, набранный за время проверки сети, не прилипнет к этому сообщению.
    this.text = "";
    if (!this.#mayGo(sendText, await this.deps.canSend())) return;
    const attachmentIds = readyUploads
      .map((u) => u.attachmentId!)
      .filter(Boolean);
    const attachmentRefs: AttachmentRef[] = readyUploads.map((u) => ({
      attachment_id: u.attachmentId ?? u.id,
      name: u.name,
      mime: "application/octet-stream",
      size: u.size,
      sha256: "",
      image: false,
    }));

    const key = crypto.randomUUID();
    const createdAt = this.deps.serverNow ? this.deps.serverNow() : Date.now();

    // Шаг 1: локальный элемент в полосе (strip). Текст остаётся в черновике, пока операция не легла в журнал.
    this.deps.probe?.mark("send_pressed");
    const localInput = freshInput(key, sendText, createdAt, attachmentRefs);
    this.deps.chat.inputs.set(key, localInput);
    this.errorMessage = null;
    this.deps.probe?.mark("input_painted");

    // Шаг 2: сохранение операции в журнал (IndexedDB)
    let op: Op<{
      content: Array<{ type: "text"; text: string }>;
      attachment_ids?: string[];
    }>;
    try {
      const body = {
        content: [{ type: "text" as const, text: sendText }],
        attachment_ids: attachmentIds.length > 0 ? attachmentIds : undefined,
      };
      op = await this.deps.ops.begin("input", body, {
        conversationId: this.deps.conversationId,
        persist: true,
        key,
      });
    } catch (err) {
      this.deps.chat.inputs.delete(key);
      this.#giveBack(sendText);
      this.errorMessage = "Не удалось сохранить на устройстве";
      throw err;
    }

    // Шаг 3: файлы уходят из лотка, запрос удаления черновика отправку не задерживает.
    // Текст, набранный после нажатия, живёт в черновике, поэтому его не трогаем.
    const detached = this.deps.uploads.detach(readyUploads.map((u) => u.id));
    if (!this.text) {
      void this.deps.drafts.clear(this.deps.conversationId).catch(() => undefined);
    }

    // Шаг 4: запуск операции
    try {
      await this.deps.ops.run(op);
    } catch (err) {
      if (op.state === "refused") {
        this.#restore(key, sendText, detached);
        this.errorMessage =
          op.refusal?.message ??
          (err instanceof Error ? err.message : "Отклонено");
        return;
      }
    }

    if (op.state === "refused") {
      this.#restore(key, sendText, detached);
      this.errorMessage = op.refusal?.message ?? "Отклонено";
    }
  }

  /** После проверки сети: нажатие либо идёт дальше, либо текст возвращается в поле. */
  #mayGo(text: string, reach: "ok" | "offline"): boolean {
    if (reach === "ok" && this.#gateFor(text).ok) return true;
    this.#giveBack(text);
    if (reach === "offline") {
      this.errorMessage =
        "Нет связи: сообщение сохранено как черновик и не отправлено";
      this.deps.drafts.set(this.deps.conversationId, { text: this.text });
    }
    return false;
  }

  #giveBack(text: string): void {
    this.text = this.text ? `${text}\n${this.text}` : text;
  }

  #restore(key: string, text: string, detached: Upload[]): void {
    this.#giveBack(text);
    this.deps.drafts.set(this.deps.conversationId, { text: this.text });
    this.deps.chat.inputs.delete(key);
    this.deps.uploads.reattach(detached);
  }

  async sendLater(at: number): Promise<void> {
    if (this.sending) return;
    this.sending = true;
    try {
      await this.#sendLater(at);
    } finally {
      this.sending = false;
    }
  }

  async #sendLater(at: number): Promise<void> {
    const now = this.deps.serverNow ? this.deps.serverNow() : Date.now();
    const valid = validateScheduleTime(at, now);
    if (valid !== "ok") {
      this.errorMessage =
        valid === "too_soon"
          ? "Слишком близкое время отправки"
          : "Слишком далёкое время отправки";
      return;
    }

    if (!this.gate.ok) return;

    const sendText = this.text;
    const readyUploads = this.deps.uploads
      .list(this.deps.conversationId)
      .filter((u) => u.phase === "ready");

    this.text = "";
    if (!this.#mayGo(sendText, await this.deps.canSend())) return;
    const attachmentIds = readyUploads
      .map((u) => u.attachmentId!)
      .filter(Boolean);

    let op: Op;
    try {
      op = await this.deps.ops.begin(
        "scheduled",
        {
          content: [{ type: "text" as const, text: sendText }],
          attachment_ids: attachmentIds.length > 0 ? attachmentIds : undefined,
          run_at: new SvelteDate(at).toISOString(),
        },
        { conversationId: this.deps.conversationId, persist: true },
      );
    } catch (err) {
      this.#giveBack(sendText);
      this.errorMessage = "Не удалось сохранить на устройстве";
      throw err;
    }

    this.deps.uploads.detach(readyUploads.map((u) => u.id));
    this.errorMessage = null;
    if (!this.text) {
      void this.deps.drafts.clear(this.deps.conversationId).catch(() => undefined);
    }

    void this.deps.ops.run(op);
  }

  attach(files: FileList | File[]): void {
    const list = this.deps.uploads.list(this.deps.conversationId);
    let count = list.filter(
      (u) => u.phase !== "cancelled" && u.phase !== "failed",
    ).length;
    const fileArray = Array.from(files);

    for (const file of fileArray) {
      try {
        this.deps.uploads.add(this.deps.conversationId, file, count);
        count += 1;
      } catch (err) {
        if (err instanceof UploadLimitError) {
          if (err.reason === "empty") this.errorMessage = "Файл пустой";
          else if (err.reason === "too_large")
            this.errorMessage = "Файл больше 100 МиБ";
          else if (err.reason === "too_many")
            this.errorMessage = "Не более 10 файлов в одном сообщении";
        }
      }
    }
  }

  async interrupt(): Promise<void> {
    if (this.interruptRunning) return;
    this.interruptRunning = true;
    this.interruptLabel = "Прерываю…";

    try {
      const key = crypto.randomUUID();
      if (this.deps.interruptApi) {
        await this.deps.interruptApi(this.deps.conversationId, key);
      } else {
        const { interrupt } = await import("#lib/api/client.js");
        await interrupt(this.deps.conversationId, key);
      }
    } catch {
      this.interruptLabel = "Прервать";
      this.interruptRunning = false;
    }
  }

  applyInterruptStatus(status: string): void {
    const label = terminalInterruptLabel(status);
    if (!label) return;
    this.interruptLabel = label;
    this.interruptRunning = false;
  }
}

const TERMINAL_INTERRUPT: Record<string, string> = {
  completed: "Прервано",
  not_applicable: "Прерывание не требуется",
  superseded: "Прерывание заменено",
  rejected_stale: "Прерывание устарело",
};

export function terminalInterruptLabel(status: string): string | null {
  return TERMINAL_INTERRUPT[status] ?? null;
}
