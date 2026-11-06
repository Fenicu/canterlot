import { SvelteDate } from "svelte/reactivity";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import type { OpJournal, Op } from "#lib/ops/journal.js";
import type { DraftStore } from "#lib/drafts/store.js";
import { DraftSync, type DraftRecord } from "#lib/drafts/sync.js";
import type { Returned } from "#lib/composer/returned.js";
import type { Upload, UploadManager } from "#lib/uploads/manager.js";
import type { ConnectionStatus } from "#lib/sync/connection.js";
import type { LimitsView } from "#lib/api/config.js";
import type { AttachmentRef } from "#lib/stores/inputs.js";
import { freshInput } from "#lib/stores/inputs.js";
import { utf8Length } from "#lib/stores/blocks.js";
import { attachmentKind } from "#lib/timeline/attachments.js";
import { UploadLimitError } from "#lib/uploads/uploader.js";
import { validateScheduleTime } from "#lib/composer/schedule.js";

export const EMPTY_DRAFT_REASON = "Введите сообщение или прикрепите файл";

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
  /** Общий черновик на хабе; без него остаётся только локальный буфер. */
  serverDrafts?: {
    put(
      text: string,
      attachmentIds: string[],
      baseRevision: number,
    ): Promise<DraftRecord>;
  };
  /** Идентификатор установки для отбрасывания эха своей записи. */
  deviceId?: string;
  /** Метаданные готового вложения для лотка на другом устройстве. */
  attachmentInfo?(id: string): Promise<{
    id: string;
    name: string;
    size: number;
  }>;
  now?(): number;
  schedule?(fn: () => void, ms: number): () => void;
  online?(): boolean;
}

interface Accepted {
  op: Op;
  key: string;
  text: string;
  detached: Upload[];
}

export class ComposerModel {
  readonly deps: ComposerDeps;
  text = $state("");
  errorMessage = $state<string | null>(null);
  /** Нажатие в работе: повторное во время проверки сети не создаёт второе сообщение. */
  sending = $state(false);
  /** Номер применённого чужого черновика: по нему экран ставит курсор в конец. */
  draftApplied = $state(0);
  #draft: DraftSync | null = null;
  #attachmentsInFlight = 0;
  #draftAttachmentIds: string[] = [];
  #reconcileToken = 0;

  constructor(deps: ComposerDeps) {
    this.deps = deps;
    if (deps.serverDrafts) {
      this.#draft = new DraftSync({
        deviceId: deps.deviceId ?? "local",
        api: deps.serverDrafts,
        current: () => this.text,
        currentAttachments: () => this.#draftAttachmentIds,
        apply: (text, attachmentIds) => {
          this.text = text;
          this.#draftAttachmentIds = [...attachmentIds];
          this.draftApplied += 1;
          void this.#reconcileAttachments(attachmentIds);
        },
        local: {
          load: () => deps.drafts.load(deps.conversationId),
          set: (text) => deps.drafts.set(deps.conversationId, { text }),
          clear: () => deps.drafts.clear(deps.conversationId),
        },
        now: deps.now ?? (() => Date.now()),
        schedule:
          deps.schedule ??
          ((fn, ms) => {
            const timer = setTimeout(fn, ms);
            return () => clearTimeout(timer);
          }),
        online: deps.online ?? (() => true),
      });
    }
  }

  /** Готовые вложения лотка в порядке показа. */
  #readyAttachmentIds(): string[] {
    return this.deps.uploads
      .list(this.deps.conversationId)
      .filter((item) => item.phase === "ready" && item.attachmentId)
      .map((item) => item.attachmentId!);
  }

  /** Загружает общий черновик и локальный буфер; возвращённые вложения встают в лоток. */
  async startDraft(server: DraftRecord | null): Promise<void> {
    if (!this.#draft) return;
    const attachments = await this.#draft.start(server);
    if (attachments.length > 0) {
      this.deps.uploads.restore(this.deps.conversationId, attachments);
      this.deps.drafts.set(this.deps.conversationId, { attachments: [] });
    }
  }

  typed(): void {
    this.#draft?.typed();
  }

  remoteDraft(record: DraftRecord | null): void {
    this.#draft?.remote(record);
  }

  flushDraft(): Promise<void> {
    return this.#draft?.flush() ?? Promise.resolve();
  }

  leaveDraft(): Promise<void> {
    return this.#draft?.leave() ?? Promise.resolve();
  }

  /** Смена лотка: готовые вложения и удаление должны попасть в общий черновик. */
  attachmentsChanged(): void {
    if (this.#attachmentsInFlight > 0) return;
    this.#draftAttachmentIds = this.#readyAttachmentIds();
    this.typed();
  }

  async #reconcileAttachments(ids: string[]): Promise<void> {
    const token = ++this.#reconcileToken;
    this.#attachmentsInFlight += 1;
    try {
      const ready = this.deps.uploads
        .list(this.deps.conversationId)
        .filter((item) => item.phase === "ready" && item.attachmentId);
      for (const item of ready) {
        if (!ids.includes(item.attachmentId!)) {
          await this.deps.uploads.cancel(item.id).catch(() => undefined);
        }
      }
      const known = this.deps.uploads
        .list(this.deps.conversationId)
        .filter((item) => item.phase === "ready" && item.attachmentId)
        .map((item) => item.attachmentId!);
      const missing = ids.filter((id) => !known.includes(id));
      if (missing.length > 0 && this.deps.attachmentInfo) {
        const found = await Promise.all(
          missing.map((id) =>
            this.deps.attachmentInfo!(id).catch(() => null),
          ),
        );
        this.deps.uploads.restore(
          this.deps.conversationId,
          found.filter(
            (item): item is { id: string; name: string; size: number } =>
              item !== null,
          ),
        );
      }
      if (token !== this.#reconcileToken) return;
      this.#draftAttachmentIds = this.#readyAttachmentIds();
      this.deps.drafts.set(this.deps.conversationId, {
        attachments: this.deps.uploads
          .list(this.deps.conversationId)
          .filter((item) => item.phase === "ready" && item.attachmentId)
          .map((item) => ({
            id: item.attachmentId!,
            name: item.name,
            size: item.size,
          })),
      });
    } finally {
      this.#attachmentsInFlight -= 1;
    }
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

    // 0. Файл не загрузился: без него сообщение ушло бы молча неполным.
    if (uploadsList.some((u) => u.phase === "failed")) {
      return { ok: false, reason: "Файл не загрузился: повторите или уберите его" };
    }

    // 1. Пустой ввод без вложений
    if (trimmed.length === 0 && activeUploads.length === 0) {
      return { ok: false, reason: EMPTY_DRAFT_REASON };
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
    await this.#press(() => this.#accept());
  }

  async #press(accept: () => Promise<Accepted | null>): Promise<void> {
    this.sending = true;
    let accepted: Accepted | null;
    try {
      accepted = await accept();
    } finally {
      // Флаг держат только проверка сети и запись в журнал: доставка может идти сколько угодно.
      this.sending = false;
    }
    if (accepted) await this.#deliver(accepted);
  }

  async #accept(): Promise<Accepted | null> {
    const sendText = this.text;
    const readyUploads = this.deps.uploads
      .list(this.deps.conversationId)
      .filter((u) => u.phase === "ready");

    // Поле свободно с нажатия: текст, набранный за время проверки сети, не прилипнет к этому сообщению.
    this.text = "";
    if (!this.#mayGo(sendText, await this.deps.canSend())) return null;
    const attachmentIds = readyUploads
      .map((u) => u.attachmentId!)
      .filter(Boolean);
    const attachmentRefs: AttachmentRef[] = readyUploads.map((u) => ({
      attachment_id: u.attachmentId ?? u.id,
      name: u.name,
      mime: u.mime || "application/octet-stream",
      size: u.size,
      sha256: "",
      image: attachmentKind({ name: u.name, mime: u.mime ?? "", image: false }) === "image",
    }));

    const key = crypto.randomUUID();
    const createdAt = this.deps.serverNow ? this.deps.serverNow() : Date.now();

    // Шаг 1: локальный элемент: пузырь в ленте, если сообщению ничто не мешает, иначе плашка. Текст остаётся в черновике, пока операция не легла в журнал.
    this.deps.probe?.mark("send_pressed");
    const localInput = freshInput(key, sendText, createdAt, attachmentRefs);
    this.deps.chat.admitLocal(localInput);
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
        device_id: this.deps.deviceId,
      };
      op = await this.deps.ops.begin("input", body, {
        conversationId: this.deps.conversationId,
        persist: true,
        key,
      });
    } catch (err) {
      this.deps.chat.discardLocal(key);
      this.#giveBack(sendText);
      this.errorMessage = "Не удалось сохранить на устройстве";
      throw err;
    }

    // Шаг 3: файлы уходят из лотка, запрос удаления черновика отправку не задерживает.
    // Текст, набранный после нажатия, живёт в черновике, поэтому его не трогаем.
    const detached = this.deps.uploads.detach(readyUploads.map((u) => u.id));
    if (!this.text) {
      this.#draft?.sent();
      void this.deps.drafts.clear(this.deps.conversationId).catch(() => undefined);
    } else {
      this.typed();
    }
    return { op, key, text: sendText, detached };
  }

  /** Шаг 4: доставка. Отказ хаба возвращает текст и файлы, остальное журнал повторяет сам. */
  async #deliver({ op, key, text, detached }: Accepted): Promise<void> {
    await this.deps.ops.run(op).catch(() => undefined);
    if (op.state !== "refused") return;
    this.#restore(key, text, detached);
    this.errorMessage = op.refusal?.message ?? "Отклонено";
  }

  /** После проверки сети: нажатие либо идёт дальше, либо текст возвращается в поле. */
  #mayGo(text: string, reach: "ok" | "offline"): boolean {
    if (reach === "ok" && this.#gateFor(text).ok) return true;
    this.#giveBack(text);
    if (reach === "offline") {
      this.errorMessage =
        "Нет связи: сообщение сохранено как черновик и не отправлено";
      this.deps.drafts.set(this.deps.conversationId, { text: this.text });
      this.typed();
    }
    return false;
  }

  #giveBack(text: string): void {
    this.text = this.text ? `${text}\n${this.text}` : text;
    this.typed();
  }

  #restore(key: string, text: string, detached: Upload[]): void {
    this.#giveBack(text);
    this.deps.drafts.set(this.deps.conversationId, { text: this.text });
    this.typed();
    this.deps.chat.discardLocal(key);
    this.deps.uploads.reattach(detached);
  }

  /** Сообщение вернулось не из этого нажатия: например, хаб отказал после перезагрузки. */
  takeBack({ text, attachments, notice }: Returned): void {
    this.#giveBack(text);
    this.deps.drafts.set(this.deps.conversationId, { text: this.text });
    this.typed();
    this.deps.uploads.restore(this.deps.conversationId, attachments);
    if (notice) this.errorMessage = notice;
  }

  async sendLater(at: number): Promise<void> {
    if (this.sending) return;
    await this.#press(() => this.#acceptLater(at));
  }

  async #acceptLater(at: number): Promise<Accepted | null> {
    const now = this.deps.serverNow ? this.deps.serverNow() : Date.now();
    const valid = validateScheduleTime(at, now);
    if (valid !== "ok") {
      this.errorMessage =
        valid === "too_soon"
          ? "Слишком близкое время отправки"
          : "Слишком далёкое время отправки";
      return null;
    }

    if (!this.gate.ok) return null;

    const sendText = this.text;
    const readyUploads = this.deps.uploads
      .list(this.deps.conversationId)
      .filter((u) => u.phase === "ready");

    this.text = "";
    if (!this.#mayGo(sendText, await this.deps.canSend())) return null;
    const attachmentIds = readyUploads
      .map((u) => u.attachmentId!)
      .filter(Boolean);

    const key = crypto.randomUUID();
    let op: Op;
    try {
      op = await this.deps.ops.begin(
        "scheduled",
        {
          content: [{ type: "text" as const, text: sendText }],
          attachment_ids: attachmentIds.length > 0 ? attachmentIds : undefined,
          run_at: new SvelteDate(at).toISOString(),
          device_id: this.deps.deviceId,
        },
        { conversationId: this.deps.conversationId, persist: true, key },
      );
    } catch (err) {
      this.#giveBack(sendText);
      this.errorMessage = "Не удалось сохранить на устройстве";
      throw err;
    }

    const detached = this.deps.uploads.detach(readyUploads.map((u) => u.id));
    this.errorMessage = null;
    if (!this.text) {
      this.#draft?.sent();
      void this.deps.drafts.clear(this.deps.conversationId).catch(() => undefined);
    } else {
      this.typed();
    }
    return { op, key, text: sendText, detached };
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
}
